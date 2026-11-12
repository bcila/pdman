//! `pdman-agent`'ın panele sunduğu HTTP API sözleşmesi tipleri.
//!
//! Panel bu tipleri hem yerel host için (in-process) hem de uzak agent'lar için
//! (HTTP/JSON) kullanır; böylece UI iki durum arasında aynı kalır.

use serde::{Deserialize, Serialize};

use crate::model::RuntimeKind;

/// Agent HTTP API'sinin uç yolları.
///
/// Panel istemcisi (`pdman::agent`) ve agent sunucusu
/// (`pdman_agent::api`) bu üreticileri paylaşır; yolların iki tarafta
/// ayrışması engellenir.
pub mod api {
    /// API kökü.
    pub const ROOT: &str = "/api/v1";

    /// `GET /health`
    pub fn health() -> String {
        format!("{ROOT}/health")
    }

    /// `GET /containers`
    pub fn containers() -> String {
        format!("{ROOT}/containers")
    }

    /// `GET /containers/{id}` (inspect)
    pub fn container(id: &str) -> String {
        format!("{ROOT}/containers/{id}")
    }

    /// `POST /containers/{id}/{action}`
    pub fn container_action(id: &str, action: &str) -> String {
        format!("{ROOT}/containers/{id}/{action}")
    }

    /// `GET /containers/{id}/stats`
    pub fn container_stats(id: &str) -> String {
        format!("{ROOT}/containers/{id}/stats")
    }

    /// `GET /containers/{id}/logs`
    pub fn container_logs(id: &str) -> String {
        format!("{ROOT}/containers/{id}/logs")
    }

    /// `GET /events`
    pub fn events() -> String {
        format!("{ROOT}/events")
    }

    /// `GET /pods`
    pub fn pods() -> String {
        format!("{ROOT}/pods")
    }

    /// `POST /pods/{id}/{action}`
    pub fn pod_action(id: &str, action: &str) -> String {
        format!("{ROOT}/pods/{id}/{action}")
    }

    /// `GET /images`
    pub fn images() -> String {
        format!("{ROOT}/images")
    }

    /// `POST /images/{id}/remove`
    pub fn image_remove(id: &str) -> String {
        format!("{ROOT}/images/{id}/remove")
    }

    /// `GET /volumes`
    pub fn volumes() -> String {
        format!("{ROOT}/volumes")
    }

    /// `POST /volumes/{name}/remove`
    pub fn volume_remove(name: &str) -> String {
        format!("{ROOT}/volumes/{name}/remove")
    }

    /// `GET /networks`
    pub fn networks() -> String {
        format!("{ROOT}/networks")
    }

    /// `POST /networks/{id}/remove`
    pub fn network_remove(id: &str) -> String {
        format!("{ROOT}/networks/{id}/remove")
    }
}

/// Agent'a giden isteklerde runtime seçimi için sorgu parametresi değeri.
pub fn runtime_query(kind: RuntimeKind) -> &'static str {
    kind.as_str()
}

/// Bir host'un sağlık/durum bilgisi (`GET /health` yanıtı).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// Host'un panelde görünen adı.
    pub name: String,
    /// Bileşen (agent veya panel) sürümü.
    pub version: String,
    /// Hostta tespit edilen runtime'ların durumu.
    pub runtimes: Vec<RuntimeStatus>,
}

/// Tek bir runtime'ın (docker/podman) kullanılabilirlik durumu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    /// Runtime türü.
    pub kind: RuntimeKind,
    /// Runtime'ın socket'i bulunup bağlanılabilir mi?
    pub available: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_info_roundtrip() {
        let info = HostInfo {
            name: "local".into(),
            version: "0.1.0".into(),
            runtimes: vec![
                RuntimeStatus {
                    kind: RuntimeKind::Docker,
                    available: true,
                },
                RuntimeStatus {
                    kind: RuntimeKind::Podman,
                    available: false,
                },
            ],
        };

        let json = serde_json::to_string(&info).unwrap();
        let parsed: HostInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, info);
        assert!(json.contains("\"docker\""));
    }
}
