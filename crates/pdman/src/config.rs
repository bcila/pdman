//! Panel yapılandırması (`pdman.toml`).
//!
//! ```toml
//! [server]
//! bind = "127.0.0.1:3000"
//!
//! [local]
//! enabled = true
//! # docker_socket = "/var/run/docker.sock"
//! # podman_socket = "/run/podman/podman.sock"
//!
//! [database]           # Faz 6: arayüzden eklenen sunucuların saklandığı
//! url = "postgres://pdman:sifre@localhost:5432/pdman"
//! # (url verilmezse PDMAN_DATABASE_URL ya da DATABASE_URL ortam
//! #  değişkenlerinden — .env dahil — okunur)
//!
//! [[hosts]]            # Faz 4: pdman-agent sunucuları
//! name = "prod1"
//! url = "https://10.0.0.5:7654"
//! token_file = "prod1.token"
//! ```

use std::{fs, path::Path};

use pdman_common::{Error, Result};
use serde::Deserialize;

/// Panelin kök yapılandırması.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// HTTP sunucu ayarları.
    pub server: ServerConfig,
    /// Yerel (panelin çalıştığı makine) runtime ayarları.
    pub local: LocalConfig,
    /// Veritabanı ayarları; verilmezse sunucu yönetimi kullanılamaz (Faz 6).
    pub database: DatabaseConfig,
    /// Kayıtlı uzak agent hostları (Faz 4).
    pub hosts: Vec<AgentHost>,
}

/// HTTP sunucu ayarları.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Dinleme adresi.
    pub bind: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:3000".to_owned(),
        }
    }
}

/// Yerel runtime ayarları.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalConfig {
    /// Yerel runtime'lar panelde görünsün mü?
    pub enabled: bool,
    /// Docker socket'inin yolu (yerine göre `unix://` önekli URI de olabilir).
    pub docker_socket: Option<String>,
    /// Podman socket'inin yolu.
    pub podman_socket: Option<String>,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            docker_socket: None,
            podman_socket: None,
        }
    }
}

/// Kayıtlı uzak `pdman-agent` hostu.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentHost {
    /// Panelde görünen ad.
    pub name: String,
    /// Agent'ın adresi (`http://...` ya da `https://...`).
    pub url: String,
    /// Taşıyıcı token'ının dosya yolu.
    pub token_file: Option<String>,
    /// Taşıyıcı token'ının kendisi (`token_file` yerine).
    pub token: Option<String>,
    /// Agent'ın TLS sertifikasını imzalayan CA (PEM); `https://` için.
    pub ca_file: Option<String>,
    /// İstemci sertifikası (PEM); mTLS gerektiren agentlar için.
    pub cert_file: Option<String>,
    /// İstemci özel anahtarı (PEM).
    pub key_file: Option<String>,
}

/// PostgreSQL ayarları (arayüzden eklenen sunucular burada saklanır).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Bağlantı adresi (`postgres://kullanici:sifre@sunucu:5432/pdman`).
    /// Verilmezse `PDMAN_DATABASE_URL` ya da `DATABASE_URL` ortam
    /// değişkenlerinden (`.env` dahil) okunur; hiçbiri yoksa sunucu yönetim
    /// paneli devre dışı kalır.
    pub url: Option<String>,
}

impl DatabaseConfig {
    /// Bağlantı adresini çözer: `pdman.toml` > `PDMAN_DATABASE_URL` >
    /// `DATABASE_URL` (ortam değişkeni ya da `.env`).
    pub fn resolve_url(&self) -> Option<String> {
        if let Some(url) = self
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            return Some(url.to_owned());
        }

        std::env::var("PDMAN_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .ok()
            .map(|url| url.trim().to_owned())
            .filter(|url| !url.is_empty())
    }
}

impl Config {
    /// Verilen yoldan yapılandırmayı yükler.
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .map_err(|e| Error::config(format!("{} okunamadı: {e}", path.display())))?;

        toml::from_str(&contents)
            .map_err(|e| Error::config(format!("{} ayrıştırılamadı: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let raw = r#"
            [server]
            bind = "0.0.0.0:8080"

            [local]
            docker_socket = "/var/run/docker.sock"

            [[hosts]]
            name = "prod1"
            url = "https://10.0.0.5:7654"
            token_file = "prod1.token"
        "#;

        let config: Config = toml::from_str(raw).unwrap();
        assert_eq!(config.server.bind, "0.0.0.0:8080");
        assert!(config.local.enabled);
        assert_eq!(
            config.local.docker_socket.as_deref(),
            Some("/var/run/docker.sock")
        );
        assert_eq!(config.hosts.len(), 1);
        assert_eq!(config.hosts[0].name, "prod1");
    }

    #[test]
    fn empty_config_uses_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.server.bind, "127.0.0.1:3000");
        assert!(config.local.enabled);
        assert!(config.hosts.is_empty());
        assert!(config.database.url.is_none());
    }

    #[test]
    fn parses_database_url() {
        let raw = r#"
            [database]
            url = "postgres://pdman:sifre@localhost:5432/pdman"
        "#;

        let config: Config = toml::from_str(raw).unwrap();
        assert_eq!(
            config.database.resolve_url().as_deref(),
            Some("postgres://pdman:sifre@localhost:5432/pdman")
        );
    }

    #[test]
    fn resolves_empty_url_to_none() {
        // Ortam değişkeni set edilmediği sürece boş/boşluklu değer None'a
        // çözümlenir.
        let config = DatabaseConfig {
            url: Some("   ".to_owned()),
        };
        assert!(config.resolve_url().is_none());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let raw = r#"
            [server]
            bindd = "127.0.0.1:3000"
        "#;

        assert!(toml::from_str::<Config>(raw).is_err());
    }
}
