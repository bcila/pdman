//! Panel ve agent arasında paylaşılan veri modelleri.
//!
//! Bu tipler hem panelin yerel runtime çağrılarında hem de agent HTTP API'sinin
//! istek/yanıt gövdelerinde kullanılır; tek doğruluk kaynağı burasıdır.

use serde::{Deserialize, Serialize};

/// Container runtime türü.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    /// Docker Engine.
    Docker,
    /// Podman (docker-compat API + libpod).
    Podman,
}

impl RuntimeKind {
    /// Kısa ad (`"docker"` / `"podman"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
        }
    }
}

impl std::fmt::Display for RuntimeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Bir container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Container {
    /// Container ID'si (tam veya kısa).
    pub id: String,
    /// Container adı (`/` öneki olmadan).
    pub name: String,
    /// Image adı.
    pub image: String,
    /// Durum makinesi adı: `running`, `exited`, `paused`, ...
    pub state: String,
    /// İnsan tarafından okunabilir durum (`Up 2 hours` gibi).
    pub status: String,
    /// Oluşturulma zamanı (Unix saniye).
    pub created: i64,
}

/// Bir image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// Image ID'si.
    pub id: String,
    /// Image'a bağlı repolar ve tag'ler (`nginx:latest` gibi).
    pub repo_tags: Vec<String>,
    /// Oluşturulma zamanı (Unix saniye).
    pub created: i64,
    /// Bayt cinsinden toplam boyut.
    pub size: u64,
}

/// Bir volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    /// Volume adı.
    pub name: String,
    /// Kullanılan driver.
    pub driver: String,
    /// Sunucudaki bağlama noktası.
    pub mountpoint: String,
    /// Oluşturulma zamanı (runtime'ın verdiği biçimde).
    pub created_at: Option<String>,
}

/// Bir network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Network {
    /// Network ID'si.
    pub id: String,
    /// Network adı.
    pub name: String,
    /// Kullanılan driver (`bridge`, `host`, ...).
    pub driver: String,
}

/// Bir Podman pod'u.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pod {
    /// Pod ID'si.
    pub id: String,
    /// Pod adı.
    pub name: String,
    /// Pod durumu (`Running`, `Exited`, ...).
    pub state: String,
    /// Pod içindeki container sayısı.
    pub num_containers: usize,
    /// Oluşturulma zamanı (podman'ın verdiği biçimde, genellikle RFC3339).
    pub created: String,
}

/// Anlık container istatistikleri.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    /// İlgili container ID'si.
    pub container_id: String,
    /// CPU kullanım yüzdesi.
    pub cpu_percent: f64,
    /// Kullanılan bellek (bayt).
    pub mem_usage: u64,
    /// Bellek sınırı (bayt).
    pub mem_limit: u64,
}

/// Bir container'ın ayrıntılı bilgisi (`inspect`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerInspect {
    /// Container ID'si.
    pub id: String,
    /// Container adı (`/` öneki olmadan).
    pub name: String,
    /// Image adı.
    pub image: String,
    /// Oluşturulma zamanı (runtime'ın verdiği biçimde).
    pub created: String,
    /// Durum makinesi adı: `running`, `exited`, `paused`, ...
    pub state: String,
    /// Container komutu (birleştirilmiş `cmd`).
    pub command: Option<String>,
    /// Port eşlemeleri (`0.0.0.0:8080->80/tcp` gibi).
    pub ports: Vec<String>,
    /// Bağlama noktaları (`/host:/container` gibi).
    pub mounts: Vec<String>,
}

/// Bir runtime olayı (docker/podman `events`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Olay zamanı (Unix saniye).
    pub time: i64,
    /// Olayın nesne türü (`container`, `pod`, `image`, ...).
    pub kind: String,
    /// Gerçekleşen eylem (`start`, `stop`, `destroy`, ...).
    pub action: String,
    /// İlgili nesnenin ID'si.
    pub id: String,
    /// İlgili nesnenin adı (biliniyorsa).
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_kind_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&RuntimeKind::Docker).unwrap(),
            "\"docker\""
        );
        assert_eq!(
            serde_json::to_string(&RuntimeKind::Podman).unwrap(),
            "\"podman\""
        );
        assert_eq!(
            serde_json::from_str::<RuntimeKind>("\"podman\"").unwrap(),
            RuntimeKind::Podman
        );
    }

    #[test]
    fn container_roundtrip() {
        let container = Container {
            id: "abc123".into(),
            name: "web".into(),
            image: "nginx:latest".into(),
            state: "running".into(),
            status: "Up 2 hours".into(),
            created: 1_700_000_000,
        };

        let json = serde_json::to_string(&container).unwrap();
        let parsed: Container = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, container);
    }
}
