//! Podman runtime istemcisi.
//!
//! Podman, `podman system service` ile Docker Engine API'siyle uyumlu bir socket
//! açar. Bu nedenle container/image/volume/network işlemleri ve `events` akışı
//! bollard tabanlı [`Docker`] istemcisiyle bu socket üzerinden yapılır.
//!
//! Pod'lar Docker API'de karşılığı olmadığından Podman'ın kendi REST API'sine
//! ([`libpod`](super::libpod)) giden ince bir istemci kullanılır.

use std::path::Path;

use crate::{
    error::{Error, Result},
    model::{Container, ContainerInspect, Image, Network, Pod, RuntimeKind, Stats, Volume},
};

use super::{
    ContainerAction, EventStream, LogOpts, LogStream, PodAction, Runtime,
    docker::{Docker, DockerEndpoint},
    libpod::LibpodClient,
};

/// Podman'ın docker-uyumlu socket'i için bilinen yollar (rootful).
const ROOTFUL_SOCKETS: [&str; 2] = ["/run/podman/podman.sock", "/var/run/podman/podman.sock"];

/// libpod `pods/json` yanıtındaki tek pod kaydı.
#[derive(Debug, Default, serde::Deserialize)]
struct LibpodPod {
    #[serde(rename = "Id", default)]
    id: Option<String>,
    #[serde(rename = "Name", default)]
    name: Option<String>,
    #[serde(rename = "State", default)]
    state: Option<String>,
    #[serde(rename = "NumContainers", default)]
    num_containers: Option<u64>,
    #[serde(rename = "Created", default)]
    created: Option<serde_json::Value>,
}

impl LibpodPod {
    fn into_pod(self) -> Pod {
        Pod {
            id: self.id.unwrap_or_default(),
            name: self.name.unwrap_or_default(),
            state: self.state.unwrap_or_default(),
            num_containers: self.num_containers.unwrap_or_default() as usize,
            created: self
                .created
                .map(|value| match value {
                    serde_json::Value::Number(number) => number.to_string(),
                    serde_json::Value::String(text) => text,
                    other => other.to_string(),
                })
                .unwrap_or_default(),
        }
    }
}

/// Podman runtime'ı.
pub struct Podman {
    /// Docker-uyumlu API üzerinden bollard istemcisi.
    docker: Docker,
    /// Pod'lar için libpod istemcisi; TCP uç noktalarda `None`.
    libpod: Option<LibpodClient>,
}

impl Podman {
    /// Socket keşfi yaparak bağlanır.
    ///
    /// Keşif sırası: `PODMAN_HOST` / `CONTAINER_HOST` ortam değişkenleri,
    /// rootless socket (`$XDG_RUNTIME_DIR/podman/podman.sock`), rootfull
    /// socket'ler.
    pub fn connect_default() -> Result<Self> {
        let socket = discover_socket().ok_or_else(|| Error::runtime("podman socket bulunamadı"))?;

        Self::connect(&socket)
    }

    /// Verilen socket yoluna (veya URI'sine) bağlanır.
    pub fn connect(socket: &str) -> Result<Self> {
        // libpod istemcisi yalnızca unix socket üzerinden çalışır; düz yola
        // ihtiyaç duyar.
        let libpod = if socket.starts_with("unix://") {
            Some(LibpodClient::new(socket.trim_start_matches("unix://")))
        } else if socket.contains("://") {
            None
        } else {
            Some(LibpodClient::new(socket))
        };

        Ok(Self {
            docker: Docker::connect(DockerEndpoint(Docker::host_uri(socket)))?,
            libpod,
        })
    }

    /// libpod istemcisi ya da TCP uç nokta hatası.
    fn libpod(&self) -> Result<&LibpodClient> {
        self.libpod.as_ref().ok_or_else(|| {
            Error::unsupported("pod'lar yalnızca unix socket üzerinden destekleniyor")
        })
    }
}

/// Podman socket'ini keşfeder; bulamazsa `None` döner.
fn discover_socket() -> Option<String> {
    // `CONTAINER_HOST`, containers-common standardı; `PODMAN_HOST` podman'ın
    // kendi kabuğundan kalma. İkisi de yol veya URI olabilir.
    for key in ["PODMAN_HOST", "CONTAINER_HOST"] {
        if let Ok(host) = std::env::var(key) {
            return Some(host);
        }
    }

    // Rootless socket.
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let socket = format!("{xdg}/podman/podman.sock");
        if Path::new(&socket).exists() {
            return Some(socket);
        }
    }

    // Rootfull socket'ler.
    ROOTFUL_SOCKETS
        .into_iter()
        .find(|socket| Path::new(socket).exists())
        .map(str::to_owned)
}

#[async_trait::async_trait]
impl Runtime for Podman {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Podman
    }

    async fn ping(&self) -> Result<()> {
        self.docker.ping().await
    }

    async fn list_containers(&self) -> Result<Vec<Container>> {
        self.docker.list_containers().await
    }

    async fn container_action(&self, id: &str, action: ContainerAction) -> Result<()> {
        self.docker.container_action(id, action).await
    }

    async fn inspect_container(&self, id: &str) -> Result<ContainerInspect> {
        self.docker.inspect_container(id).await
    }

    async fn container_stats(&self, id: &str) -> Result<Stats> {
        self.docker.container_stats(id).await
    }

    async fn logs(&self, id: &str, opts: LogOpts) -> Result<LogStream> {
        self.docker.logs(id, opts).await
    }

    async fn list_images(&self) -> Result<Vec<Image>> {
        self.docker.list_images().await
    }

    async fn image_remove(&self, id: &str, force: bool) -> Result<()> {
        self.docker.image_remove(id, force).await
    }

    async fn list_volumes(&self) -> Result<Vec<Volume>> {
        self.docker.list_volumes().await
    }

    async fn volume_remove(&self, name: &str) -> Result<()> {
        self.docker.volume_remove(name).await
    }

    async fn list_networks(&self) -> Result<Vec<Network>> {
        self.docker.list_networks().await
    }

    async fn network_remove(&self, id: &str) -> Result<()> {
        self.docker.network_remove(id).await
    }

    async fn events(&self) -> Result<EventStream> {
        self.docker.events().await
    }

    async fn list_pods(&self) -> Result<Vec<Pod>> {
        let pods: Vec<LibpodPod> = self.libpod()?.get_json("pods/json").await?;

        Ok(pods.into_iter().map(LibpodPod::into_pod).collect())
    }

    async fn pod_action(&self, id: &str, action: PodAction) -> Result<()> {
        let libpod = self.libpod()?;

        match action {
            PodAction::Start => libpod.post(&format!("pods/{id}/start")).await,
            PodAction::Stop => libpod.post(&format!("pods/{id}/stop")).await,
            PodAction::Remove { force } => {
                let query = if force { "?force=true" } else { "" };
                libpod.delete(&format!("pods/{id}{query}")).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_fails_fast_when_socket_missing() {
        // bollard, unix socket bağlantısında dosyanın varlığını kontrol eder;
        // olmayan bir socket hemen hata vermelidir.
        assert!(Podman::connect("/tmp/pdman-boyle-bir-socket-yok.sock").is_err());
    }

    #[test]
    fn connect_accepts_plain_path_and_uri() {
        // Var olan bir dosya (socket olması şart değil) bağlantı kurulmasına
        // izin verir; asıl doğrulama ping ile yapılır.
        let dir = std::env::temp_dir();
        let file = dir.join(format!("pdman-test-{}.sock", std::process::id()));
        std::fs::write(&file, b"").unwrap();

        let by_path = Podman::connect(file.to_str().unwrap());
        let by_uri = Podman::connect(&format!("unix://{}", file.display()));

        let _ = std::fs::remove_file(&file);
        assert!(by_path.is_ok());
        assert!(by_uri.is_ok());
    }

    #[test]
    fn pod_entry_maps_to_pod() {
        let entry = LibpodPod {
            id: Some("abc123".to_owned()),
            name: Some("web".to_owned()),
            state: Some("Running".to_owned()),
            num_containers: Some(3),
            created: Some(serde_json::Value::String("2026-01-01T12:00:00Z".to_owned())),
        };

        let pod = entry.into_pod();
        assert_eq!(pod.id, "abc123");
        assert_eq!(pod.name, "web");
        assert_eq!(pod.state, "Running");
        assert_eq!(pod.num_containers, 3);
        assert_eq!(pod.created, "2026-01-01T12:00:00Z");
    }

    #[test]
    fn pod_entry_with_numeric_created_maps_to_string() {
        let entry = LibpodPod {
            created: Some(serde_json::json!(1794049191)),
            ..Default::default()
        };

        assert_eq!(entry.into_pod().created, "1794049191");
    }
}
