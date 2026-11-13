//! Host kayıt defteri.
//!
//! Yerel runtime'lar (Docker ve Podman), `pdman.toml`'daki `[[hosts]]`
//! girişleri ve veritabanına arayüzden eklenen uzak `pdman-agent`
//! sunucuları burada tek listede toplanır. Kayıt defteri çalışma zamanında
//! değiştirilebilir: sunucu eklendiğinde yeni kayıtlar girer, silindiğinde
//! çıkarılır ve olay iletici görevleri iptal edilir.

use std::sync::{Arc, RwLock};

use pdman_common::{
    error::Result,
    model::RuntimeKind,
    runtime::{
        Runtime,
        docker::{Docker, DockerEndpoint},
        podman::Podman,
    },
};
use tokio::{sync::broadcast, task::JoinHandle};

use crate::{
    agent::{AgentClient, AgentTls},
    config::{AgentHost, Config, LocalConfig},
    db::ServerRecord,
    events::{self, HostEvent},
};

/// Hostun kaynağı: silme izni buna göre verilir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSource {
    /// Panelin çalıştığı makinedeki runtime.
    Local,
    /// `pdman.toml` `[[hosts]]` girişi.
    Config,
    /// Arayüzden eklenip veritabanına kaydedilmiş sunucu.
    Database,
}

/// Uzak bağlantının güvenlik özeti (UI gösterimi için).
#[derive(Debug, Clone, Copy, Default)]
pub struct ConnectionSecurity {
    /// `https://` ile mi bağlanılıyor?
    pub tls: bool,
    /// mTLS (istemci sertifikası) kullanılıyor mu?
    pub mtls: bool,
    /// Taşıyıcı token ayarlı mı?
    pub token: bool,
}

/// Panelde yönetilen tek bir host.
pub struct HostEntry {
    /// URL'lerde ve sorgu parametrelerinde kullanılan kısa kimlik
    /// (`docker`, `podman`; uzak hostlar için `<sunucu>-<runtime>`).
    pub id: String,
    /// Panelde görünen ad.
    pub name: String,
    /// Runtime türü.
    pub kind: RuntimeKind,
    /// Runtime istemcisi; bağlantı hiç kurulamadıysa `None`.
    pub runtime: Option<Arc<dyn Runtime>>,
    /// Son sağlık denetimi başarılı mı?
    pub available: bool,
    /// Kullanılamıyorsa nedeni.
    pub error: Option<String>,
    /// Hostun kaynağı.
    pub source: HostSource,
    /// Ait olduğu sunucunun kimliği (yerel hostlarda `id` ile aynı).
    pub server_id: String,
    /// Uzak hostlarda agent'ın adresi.
    pub url: Option<String>,
    /// Uzak bağlantının güvenlik özeti.
    pub security: Option<ConnectionSecurity>,
    /// Olay iletici görevi; sunucu silinirken iptal edilir.
    forwarder: Option<JoinHandle<()>>,
}

/// Tüm kayıtlı hostlar. Girişler [`Arc`] ile paylaşılır; kilidi tutmadan
/// kopyalanabilirler.
pub struct HostRegistry {
    hosts: RwLock<Vec<Arc<HostEntry>>>,
    /// Yeni eklenen hostların olay ileticilerine beslenen kanal.
    events: broadcast::Sender<HostEvent>,
}

impl HostRegistry {
    /// Yapılandırma ve veritabanındaki sunuculardan kayıt defteri kurar;
    /// her hostu ping ile doğrular ve olay ileticilerini başlatır.
    pub async fn build(
        config: &Config,
        db_servers: &[ServerRecord],
        events: broadcast::Sender<HostEvent>,
    ) -> Self {
        let mut hosts = Vec::new();

        if config.local.enabled {
            hosts.push(build_docker(&config.local));
            hosts.push(build_podman(&config.local));
        }

        let mut connections: Vec<AgentConnection> = config
            .hosts
            .iter()
            .map(AgentConnection::from_config)
            .collect();
        connections.extend(db_servers.iter().map(AgentConnection::from_record));

        for connection in &connections {
            hosts.extend(build_agent_hosts(connection).await);
        }

        for host in &mut hosts {
            let Some(runtime) = &host.runtime else {
                continue;
            };
            if let Err(err) = runtime.ping().await {
                host.available = false;
                host.error = Some(err.to_string());
            }
        }

        let registry = Self {
            hosts: RwLock::new(Vec::new()),
            events,
        };
        registry.register(hosts);
        registry
    }

    /// Testler için: hazır bir host listesinden (iletici görevleri
    /// başlatmadan) kayıt defteri kurar.
    pub fn from_hosts(hosts: Vec<HostEntry>) -> Self {
        Self {
            hosts: RwLock::new(hosts.into_iter().map(Arc::new).collect()),
            events: broadcast::channel(1).0,
        }
    }

    /// Kimliğe göre host döner.
    pub fn get(&self, id: &str) -> Option<Arc<HostEntry>> {
        self.hosts
            .read()
            .expect("host kilidi zehirlendi")
            .iter()
            .find(|host| host.id == id)
            .cloned()
    }

    /// Tüm hostlar.
    pub fn hosts(&self) -> Vec<Arc<HostEntry>> {
        self.hosts.read().expect("host kilidi zehirlendi").clone()
    }

    /// Varsayılan host: ilk erişilebilir olan; yoksa ilk host; hiç host yoksa
    /// `None`.
    pub fn default_host(&self) -> Option<Arc<HostEntry>> {
        let hosts = self.hosts();
        hosts
            .iter()
            .find(|host| host.available)
            .or_else(|| hosts.first())
            .cloned()
    }

    /// Veritabanına kayıtlı bir sunucunun host kayıtlarını ekler ya da
    /// günceller: aynı `server_id`'li eski kayıtlar (ve iletici görevleri)
    /// kaldırılır.
    pub fn upsert_server(&self, server_id: &str, entries: Vec<HostEntry>) {
        self.drop_server(server_id);
        self.register(entries);
    }

    /// Veritabanına kayıtlı bir sunucunun host kayıtlarını ve iletici
    /// görevlerini kaldırır. Kayıt silinmişse `true` döner.
    pub fn remove_server(&self, server_id: &str) -> bool {
        self.drop_server(server_id)
    }

    /// `server_id`'li veritabanı kaynaklı kayıtları kaldırır; iletici
    /// görevlerini iptal eder. Kayıt silinmişse `true` döner.
    fn drop_server(&self, server_id: &str) -> bool {
        let mut hosts = self.hosts.write().expect("host kilidi zehirlendi");
        let mut removed = false;

        hosts.retain(|host| {
            if host.server_id == server_id && host.source == HostSource::Database {
                if let Some(forwarder) = &host.forwarder {
                    forwarder.abort();
                }
                removed = true;
                false
            } else {
                true
            }
        });

        removed
    }

    /// Hostları kaydeder; runtime'ı olanlar için olay ileticisi başlatır.
    fn register(&self, entries: Vec<HostEntry>) {
        let mut hosts = self.hosts.write().expect("host kilidi zehirlendi");

        for mut host in entries {
            let forwarder = events::spawn_forwarder(&host, self.events.clone());
            host.forwarder = Some(forwarder);
            hosts.push(Arc::new(host));
        }
    }
}

/// Bir uzak agent bağlantısının tanımı (yapılandırmadan ya da veritabanından).
pub struct AgentConnection {
    /// Sunucunun kısa kimliği; host kimlikleri `<server_id>-<runtime>`
    /// biçiminde üretilir.
    pub server_id: String,
    /// Panelde görünen ad.
    pub name: String,
    /// Agent'ın adresi.
    pub url: String,
    /// Taşıyıcı token'ı; boşsa kimlik doğrulama yok (geri-döngü senaryosu).
    pub token: String,
    /// TLS/mTLS ayarları.
    pub tls: AgentTls,
    /// Bağlantının kaynağı.
    pub source: HostSource,
}

impl AgentConnection {
    /// `pdman.toml` `[[hosts]]` girişinden bağlantı kurar.
    pub fn from_config(agent: &AgentHost) -> Self {
        Self {
            server_id: agent.name.clone(),
            name: agent.name.clone(),
            url: agent.url.clone(),
            token: load_agent_token(agent),
            tls: AgentTls {
                ca_file: agent.ca_file.clone(),
                cert_file: agent.cert_file.clone(),
                key_file: agent.key_file.clone(),
            },
            source: HostSource::Config,
        }
    }

    /// Veritabanı kaydından bağlantı kurar.
    pub fn from_record(record: &ServerRecord) -> Self {
        Self {
            server_id: record.id.clone(),
            name: record.name.clone(),
            url: record.url.clone(),
            token: record.token.clone(),
            tls: AgentTls {
                ca_file: non_empty(&record.ca_file),
                cert_file: non_empty(&record.cert_file),
                key_file: non_empty(&record.key_file),
            },
            source: HostSource::Database,
        }
    }

    /// Bağlantının güvenlik özeti.
    pub fn security(&self) -> ConnectionSecurity {
        ConnectionSecurity {
            tls: self.url.starts_with("https://"),
            mtls: self.tls.cert_file.is_some(),
            token: !self.token.is_empty(),
        }
    }
}

/// Boş dizeyi `None`'a çevirir.
fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Yerel Docker hostunu oluşturur (socket bulunamazsa kullanılamaz kaydeder).
fn build_docker(local: &LocalConfig) -> HostEntry {
    let connected = match &local.docker_socket {
        Some(socket) => Docker::connect(DockerEndpoint(Docker::host_uri(socket))),
        None => Docker::connect_default(),
    };

    match connected {
        Ok(docker) => host_connected(
            HostSource::Local,
            "docker",
            "Docker (yerel)",
            RuntimeKind::Docker,
            Arc::new(docker),
            "docker",
            None,
            None,
        ),
        Err(err) => host_failed(
            HostSource::Local,
            "docker",
            "Docker (yerel)",
            RuntimeKind::Docker,
            "docker",
            err.to_string(),
            None,
            None,
        ),
    }
}

/// Yerel Podman hostunu oluşturur (socket bulunamazsa kullanılamaz kaydeder).
fn build_podman(local: &LocalConfig) -> HostEntry {
    let connected: Result<Podman> = match &local.podman_socket {
        Some(socket) => Podman::connect(socket),
        None => Podman::connect_default(),
    };

    match connected {
        Ok(podman) => host_connected(
            HostSource::Local,
            "podman",
            "Podman (yerel)",
            RuntimeKind::Podman,
            Arc::new(podman),
            "podman",
            None,
            None,
        ),
        Err(err) => host_failed(
            HostSource::Local,
            "podman",
            "Podman (yerel)",
            RuntimeKind::Podman,
            "podman",
            err.to_string(),
            None,
            None,
        ),
    }
}

/// Bağlantısı kurulan bir host kaydı.
#[allow(clippy::too_many_arguments)]
fn host_connected(
    source: HostSource,
    id: &str,
    name: &str,
    kind: RuntimeKind,
    runtime: Arc<dyn Runtime>,
    server_id: &str,
    url: Option<String>,
    security: Option<ConnectionSecurity>,
) -> HostEntry {
    HostEntry {
        id: id.to_owned(),
        name: name.to_owned(),
        kind,
        runtime: Some(runtime),
        available: true,
        error: None,
        source,
        server_id: server_id.to_owned(),
        url,
        security,
        forwarder: None,
    }
}

/// Bağlantı kurulamayan bir host kaydı.
#[allow(clippy::too_many_arguments)]
fn host_failed(
    source: HostSource,
    id: &str,
    name: &str,
    kind: RuntimeKind,
    server_id: &str,
    error: String,
    url: Option<String>,
    security: Option<ConnectionSecurity>,
) -> HostEntry {
    HostEntry {
        id: id.to_owned(),
        name: name.to_owned(),
        kind,
        runtime: None,
        available: false,
        error: Some(error),
        source,
        server_id: server_id.to_owned(),
        url,
        security,
        forwarder: None,
    }
}

/// Bağlantısı kurulan bir agent runtime kaydı.
fn agent_host_connected(
    connection: &AgentConnection,
    kind: RuntimeKind,
    runtime: Arc<dyn Runtime>,
    security: ConnectionSecurity,
) -> HostEntry {
    host_connected(
        connection.source,
        &format!("{}-{}", connection.server_id, kind),
        &format!("{} ({})", connection.name, kind),
        kind,
        runtime,
        &connection.server_id,
        Some(connection.url.clone()),
        Some(security),
    )
}

/// Bağlantı kurulamayan bir agent kaydı.
fn agent_host_failed(connection: &AgentConnection, error: String) -> HostEntry {
    host_failed(
        connection.source,
        &connection.server_id,
        &connection.name,
        RuntimeKind::Docker,
        &connection.server_id,
        error,
        Some(connection.url.clone()),
        Some(connection.security()),
    )
}

/// Bir uzak agent'ın host kayıtlarını oluşturur.
///
/// Agent'ın sağlık yanıtındaki her kullanılabilir runtime için ayrı bir host
/// kaydı açılır (`prod1-docker`, `prod1-podman` gibi). Agent'a
/// ulaşılamıyorsa çevrimdışı bir kayıt gösterilir.
pub async fn build_agent_hosts(connection: &AgentConnection) -> Vec<HostEntry> {
    let security = connection.security();
    let health =
        match AgentClient::health(&connection.url, &connection.token, connection.tls.clone()).await
        {
            Ok(health) => health,
            Err(err) => {
                return vec![agent_host_failed(
                    connection,
                    format!("agent'e ulaşılamadı: {err}"),
                )];
            }
        };

    let mut hosts = Vec::new();

    for status in health.runtimes {
        if !status.available {
            continue;
        }

        let client = match AgentClient::new(
            &connection.url,
            connection.token.clone(),
            status.kind,
            connection.tls.clone(),
        ) {
            Ok(client) => client,
            Err(err) => {
                return vec![agent_host_failed(
                    connection,
                    format!("agent istemcisi kurulamadı: {err}"),
                )];
            }
        };

        hosts.push(agent_host_connected(
            connection,
            status.kind,
            Arc::new(client),
            security,
        ));
    }

    if hosts.is_empty() {
        hosts.push(agent_host_failed(
            connection,
            "agent'ta kullanılabilir runtime yok".to_owned(),
        ));
    }

    hosts
}

/// Agent token'ını yapılandırmadan yükler; yoksa boş dize (kimlik doğrulama
/// yok — SSH tüneli senaryosu).
fn load_agent_token(agent: &AgentHost) -> String {
    if let Some(token) = &agent.token {
        return token.trim().to_owned();
    }

    if let Some(path) = &agent.token_file
        && let Ok(token) = std::fs::read_to_string(path)
    {
        return token.trim().to_owned();
    }

    String::new()
}
#[cfg(test)]
mod tests {
    use super::*;

    struct StubRuntime {
        ping_ok: bool,
    }

    #[async_trait::async_trait]
    impl Runtime for StubRuntime {
        fn kind(&self) -> RuntimeKind {
            RuntimeKind::Docker
        }

        async fn ping(&self) -> pdman_common::Result<()> {
            if self.ping_ok {
                Ok(())
            } else {
                Err(pdman_common::Error::runtime("daemon kapalı"))
            }
        }
    }

    fn entry(id: &str, runtime: Option<Arc<dyn Runtime>>) -> HostEntry {
        let available = runtime.is_some();
        HostEntry {
            id: id.to_owned(),
            name: format!("{id} (yerel)"),
            kind: RuntimeKind::Docker,
            runtime,
            available,
            error: None,
            source: HostSource::Local,
            server_id: id.to_owned(),
            url: None,
            security: None,
            forwarder: None,
        }
    }

    fn db_entry(id: &str, server_id: &str, runtime: Option<Arc<dyn Runtime>>) -> HostEntry {
        let available = runtime.is_some();
        HostEntry {
            id: id.to_owned(),
            name: format!("{server_id} (docker)"),
            kind: RuntimeKind::Docker,
            runtime,
            available,
            error: None,
            source: HostSource::Database,
            server_id: server_id.to_owned(),
            url: Some("https://10.0.0.5:7654".to_owned()),
            security: Some(ConnectionSecurity::default()),
            forwarder: None,
        }
    }

    #[test]
    fn default_host_prefers_available() {
        let offline = entry("docker", None);
        let online = entry("podman", Some(Arc::new(StubRuntime { ping_ok: true })));

        let registry = HostRegistry::from_hosts(vec![offline, online]);
        assert_eq!(
            registry.default_host().map(|h| h.id.clone()),
            Some("podman".to_owned())
        );
        assert!(registry.get("podman").is_some());
        assert!(registry.get("yok").is_none());
    }

    #[test]
    fn default_host_falls_back_to_first() {
        let offline = entry("docker", None);
        let offline2 = entry("podman", None);

        let registry = HostRegistry::from_hosts(vec![offline, offline2]);
        assert_eq!(
            registry.default_host().map(|h| h.id.clone()),
            Some("docker".to_owned())
        );
    }

    #[test]
    fn default_host_none_when_empty() {
        let registry = HostRegistry::from_hosts(Vec::new());
        assert!(registry.default_host().is_none());
    }

    #[test]
    fn remove_server_drops_only_database_entries() {
        let local = entry("docker", None);
        let config = HostEntry {
            source: HostSource::Config,
            server_id: "prod1".to_owned(),
            ..db_entry("prod1-docker", "prod1", None)
        };
        let db1 = db_entry("prod2-docker", "prod2", None);
        let db2 = db_entry("prod2-podman", "prod2", None);

        let registry = HostRegistry::from_hosts(vec![local, config, db1, db2]);
        assert!(registry.remove_server("prod2"));
        assert!(registry.get("prod2-docker").is_none());
        assert!(registry.get("prod2-podman").is_none());
        // Yapılandırma ve yerel kayıtlar korunur.
        assert!(registry.get("prod1-docker").is_some());
        assert!(registry.get("docker").is_some());

        // İkinci silme artık kayıt bulamaz.
        assert!(!registry.remove_server("prod2"));
        // Yapılandırma kaynaklı sunucu arayüzden silinemez.
        assert!(!registry.remove_server("prod1"));
    }

    #[tokio::test]
    async fn build_respects_local_enabled_flag() {
        let mut config = Config::default();
        config.local.enabled = false;
        let (events_tx, _events_rx) = broadcast::channel(8);
        let registry = HostRegistry::build(&config, &[], events_tx).await;
        assert!(registry.hosts().is_empty());
    }
}
