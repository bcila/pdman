//! `pdman-agent` yapılandırması (`pdman-agent.toml`).
//!
//! ```toml
//! name = "prod1"
//!
//! [server]
//! bind = "0.0.0.0:7654"
//!
//! [auth]
//! # token = "gizli"                        # ya da
//! # token_file = "/etc/pdman-agent/token"
//!
//! [docker]
//! socket = "/var/run/docker.sock"
//!
//! [podman]
//! socket = "/run/podman/podman.sock"
//!
//! [tls]                                     # opsiyonel
//! cert_file = "/etc/pdman-agent/cert.pem"
//! key_file = "/etc/pdman-agent/key.pem"
//! # ca_file = "/etc/pdman-agent/ca.pem"     # verilirse mTLS

use std::{fs, path::Path};

use pdman_common::{Error, Result};
use serde::Deserialize;

/// Agentın kök yapılandırması.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Hostun panelde görünen adı.
    pub name: Option<String>,
    /// HTTP sunucu ayarları.
    pub server: ServerConfig,
    /// Kimlik doğrulama ayarları.
    pub auth: AuthConfig,
    /// Docker socket ayarları.
    pub docker: DockerConfig,
    /// Podman socket ayarları.
    pub podman: PodmanConfig,
    /// TLS sunucu ayarları (verilirse agent TLS ile dinler).
    pub tls: TlsConfig,
}

/// TLS sunucu ayarları (verilirse agent TLS ile dinler).
///
/// `ca_file` verilirse karşılıklı TLS (mTLS) etkin olur: yalnızca bu CA
/// tarafından imzalanan istemci sertifikalarına sahip bağlantılar kabul
/// edilir.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TlsConfig {
    /// Sunucu sertifikası (PEM, sertifika zinciri).
    pub cert_file: String,
    /// Sunucu özel anahtarı (PEM).
    pub key_file: String,
    /// İstemci sertifikalarını imzalayan CA (mTLS; verilirse zorunlu).
    pub ca_file: Option<String>,
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
            bind: "127.0.0.1:7654".to_owned(),
        }
    }
}

/// Kimlik doğrulama ayarları.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// Taşıyıcı token'ın kendisi.
    pub token: Option<String>,
    /// Taşıyıcı token'ının dosya yolu.
    pub token_file: Option<String>,
}

/// Docker socket ayarları.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DockerConfig {
    /// Docker socket'inin yolu.
    pub socket: String,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            socket: "/var/run/docker.sock".to_owned(),
        }
    }
}

/// Podman socket ayarları.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PodmanConfig {
    /// Podman'ın docker-uyumlu socket'inin yolu.
    pub socket: String,
}

impl Default for PodmanConfig {
    fn default() -> Self {
        Self {
            socket: "/run/podman/podman.sock".to_owned(),
        }
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

    /// Hostun görünen adı: yapılandırmadaki ad ya da makine adı.
    pub fn host_name(&self) -> String {
        self.name
            .clone()
            .or_else(|| std::env::var("HOSTNAME").ok())
            .unwrap_or_else(|| "pdman-agent".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let raw = r#"
            name = "prod1"

            [server]
            bind = "0.0.0.0:7654"

            [auth]
            token_file = "/etc/pdman-agent/token"

            [docker]
            socket = "/custom/docker.sock"
        "#;

        let config: Config = toml::from_str(raw).unwrap();
        assert_eq!(config.host_name(), "prod1");
        assert_eq!(config.server.bind, "0.0.0.0:7654");
        assert_eq!(config.docker.socket, "/custom/docker.sock");
        assert_eq!(config.podman.socket, "/run/podman/podman.sock");
    }

    #[test]
    fn empty_config_uses_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.server.bind, "127.0.0.1:7654");
        assert_eq!(config.docker.socket, "/var/run/docker.sock");
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let raw = "[server]\nbindd = \"x\"";
        assert!(toml::from_str::<Config>(raw).is_err());
    }
}
