//! Host kayıt defteri.
//!
//! Faz 1 kapsamında yalnızca yerel runtime'lar (Docker ve Podman) kaydedilir.
//! Faz 4'te uzak `pdman-agent` hostları da buraya eklenecek.

use std::sync::Arc;

use pdman_common::{
    error::Result,
    model::RuntimeKind,
    runtime::{
        Runtime,
        docker::{Docker, DockerEndpoint},
        podman::Podman,
    },
};

use crate::agent::AgentClient;
use crate::config::{AgentHost, Config, LocalConfig};

/// Panelde yönetilen tek bir host.
pub struct HostEntry {
    /// URL'lerde ve sorgu parametrelerinde kullanılan kısa kimlik
    /// (`docker`, `podman`; uzak hostlar için Faz 4'te yapılandırma adı).
    pub id: String,
    /// Panelde görünen ad.
    pub name: String,
    /// Runtime türü.
    pub kind: RuntimeKind,
    /// Runtime istemcisi; bağlantı hiç kurulamadıysa `None`.
    pub runtime: Option<Arc<dyn Runtime>>,
    /// Başlangıçtaki `ping` başarılı mı?
    pub available: bool,
    /// Kullanılamıyorsa nedeni.
    pub error: Option<String>,
}

/// Tüm kayıtlı hostlar.
pub struct HostRegistry {
    hosts: Vec<HostEntry>,
}

impl HostRegistry {
    /// Yapılandırmadan hostları oluşturur ve her birini ping ile doğrular.
    pub async fn build(config: &Config) -> Self {
        let mut hosts = Vec::new();

        if config.local.enabled {
            hosts.push(build_docker(&config.local));
            hosts.push(build_podman(&config.local));
        }

        for agent_host in &config.hosts {
            hosts.extend(build_agent_hosts(agent_host).await);
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

        Self { hosts }
    }

    /// Testler ve Faz 4 için: hazır bir host listesinden kayıt defteri kurar.
    pub fn from_hosts(hosts: Vec<HostEntry>) -> Self {
        Self { hosts }
    }

    /// Kimliğe göre host döner.
    pub fn get(&self, id: &str) -> Option<&HostEntry> {
        self.hosts.iter().find(|host| host.id == id)
    }

    /// Tüm hostlar.
    pub fn hosts(&self) -> &[HostEntry] {
        &self.hosts
    }

    /// Varsayılan host: ilk erişilebilir olan; yoksa ilk host; hiç host yoksa
    /// `None`.
    pub fn default_host(&self) -> Option<&HostEntry> {
        self.hosts
            .iter()
            .find(|host| host.available)
            .or_else(|| self.hosts.first())
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
            "docker",
            "Docker (yerel)",
            RuntimeKind::Docker,
            Arc::new(docker),
        ),
        Err(err) => host_failed(
            "docker",
            "Docker (yerel)",
            RuntimeKind::Docker,
            err.to_string(),
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
            "podman",
            "Podman (yerel)",
            RuntimeKind::Podman,
            Arc::new(podman),
        ),
        Err(err) => host_failed(
            "podman",
            "Podman (yerel)",
            RuntimeKind::Podman,
            err.to_string(),
        ),
    }
}

/// Bağlantısı kurulan bir host kaydı.
fn host_connected(id: &str, name: &str, kind: RuntimeKind, runtime: Arc<dyn Runtime>) -> HostEntry {
    HostEntry {
        id: id.to_owned(),
        name: name.to_owned(),
        kind,
        runtime: Some(runtime),
        available: true,
        error: None,
    }
}

/// Bağlantı kurulamayan bir host kaydı.
fn host_failed(id: &str, name: &str, kind: RuntimeKind, error: String) -> HostEntry {
    HostEntry {
        id: id.to_owned(),
        name: name.to_owned(),
        kind,
        runtime: None,
        available: false,
        error: Some(error),
    }
}

/// Yapılandırılmış bir uzak agent'ın host kayıtlarını oluşturur.
///
/// Agent'ın sağlık yanıtındaki her kullanılabilir runtime için ayrı bir host
/// kaydı açılır (`prod1-docker`, `prod1-podman` gibi). Agent'a
/// ulaşılamıyorsa çevrimdışı bir kayıt gösterilir.
async fn build_agent_hosts(agent: &AgentHost) -> Vec<HostEntry> {
    let token = load_agent_token(agent);
    let tls = crate::agent::AgentTls {
        ca_file: agent.ca_file.clone(),
        cert_file: agent.cert_file.clone(),
        key_file: agent.key_file.clone(),
    };

    let health = match AgentClient::health(&agent.url, &token, tls.clone()).await {
        Ok(health) => health,
        Err(err) => {
            return vec![host_failed(
                &agent.name,
                &agent.name,
                RuntimeKind::Docker,
                format!("agent'e ulaşılamadı: {err}"),
            )];
        }
    };

    let mut hosts = Vec::new();

    for status in health.runtimes {
        if !status.available {
            continue;
        }

        let client = match AgentClient::new(&agent.url, token.clone(), status.kind, tls.clone()) {
            Ok(client) => client,
            Err(err) => {
                return vec![host_failed(
                    &agent.name,
                    &agent.name,
                    RuntimeKind::Docker,
                    format!("agent istemcisi kurulamadı: {err}"),
                )];
            }
        };

        hosts.push(host_connected(
            &format!("{}-{}", agent.name, status.kind),
            &format!("{} ({})", agent.name, status.kind),
            status.kind,
            Arc::new(client),
        ));
    }

    if hosts.is_empty() {
        hosts.push(host_failed(
            &agent.name,
            &agent.name,
            RuntimeKind::Docker,
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
        }
    }

    #[test]
    fn default_host_prefers_available() {
        let offline = entry("docker", None);
        let online = entry("podman", Some(Arc::new(StubRuntime { ping_ok: true })));

        let registry = HostRegistry::from_hosts(vec![offline, online]);
        assert_eq!(
            registry.default_host().map(|h| h.id.as_str()),
            Some("podman")
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
            registry.default_host().map(|h| h.id.as_str()),
            Some("docker")
        );
    }

    #[test]
    fn default_host_none_when_empty() {
        let registry = HostRegistry::from_hosts(Vec::new());
        assert!(registry.default_host().is_none());
    }

    #[tokio::test]
    async fn build_respects_local_enabled_flag() {
        let mut config = Config::default();
        config.local.enabled = false;
        let registry = HostRegistry::build(&config).await;
        assert!(registry.hosts().is_empty());
    }
}
