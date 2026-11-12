//! Docker Engine API istemcisi (bollard tabanlı).
//!
//! Aynı istemci, Podman'ın Docker-uyumlu socket'ine de bağlanabilir; Podman'a özgü
//! işlemler (pod'lar) için [`super::podman`] modülüne bakınız.

use std::{io, path::PathBuf};

use bollard::{
    Docker as BollardDocker,
    query_parameters::{
        ListContainersOptionsBuilder, ListImagesOptions, ListNetworksOptions, ListVolumesOptions,
        LogsOptionsBuilder, RemoveContainerOptionsBuilder, StatsOptionsBuilder,
    },
};
use futures_util::StreamExt;

use crate::{
    error::{Error, Result},
    model::{Container, ContainerInspect, Event, Image, Network, RuntimeKind, Stats, Volume},
};

use super::{ContainerAction, EventStream, Runtime};

/// Docker daemon uç noktası (`unix:///var/run/docker.sock` gibi).
pub struct DockerEndpoint(pub String);

/// Docker Engine API istemcisi.
pub struct Docker {
    client: BollardDocker,
}

impl Docker {
    /// `DOCKER_HOST` ve docker context kurallarına göre varsayılan bağlantıyı kurar.
    pub fn connect_default() -> io::Result<Self> {
        if let Ok(host) = std::env::var("DOCKER_HOST") {
            return Self::connect(DockerEndpoint(host));
        }

        let context = docker_current_context()?.unwrap_or_else(|| "default".to_string());

        let endpoint = docker_context_endpoint(&context)?;

        Self::connect(DockerEndpoint(endpoint))
    }

    /// Verilen uç noktaya bağlanır.
    pub fn connect(endpoint: DockerEndpoint) -> io::Result<Self> {
        let client = BollardDocker::connect_with_host(&endpoint.0).map_err(io::Error::other)?;

        Ok(Self { client })
    }

    /// Düz socket yolunu (`/var/run/docker.sock`) bollard host biçimine çevirir.
    pub fn host_uri(socket: &str) -> String {
        if socket.contains("://") {
            socket.to_owned()
        } else {
            format!("unix://{socket}")
        }
    }
}

#[async_trait::async_trait]
impl Runtime for Docker {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Docker
    }

    async fn ping(&self) -> Result<()> {
        self.client.ping().await.map(|_| ()).map_err(Error::from)
    }

    async fn list_containers(&self) -> Result<Vec<Container>> {
        let options = ListContainersOptionsBuilder::default().all(true).build();

        let containers = self.client.list_containers(Some(options)).await?;

        Ok(containers
            .into_iter()
            .map(|container| Container {
                id: container.id.unwrap_or_default(),
                name: container
                    .names
                    .and_then(|names| names.into_iter().next())
                    .unwrap_or_default()
                    .trim_start_matches('/')
                    .to_string(),
                image: container.image.unwrap_or_default(),
                state: container
                    .state
                    .map(|state| state.to_string())
                    .unwrap_or_default(),
                status: container.status.unwrap_or_default(),
                created: container.created.unwrap_or_default(),
            })
            .collect())
    }

    async fn container_action(&self, id: &str, action: ContainerAction) -> Result<()> {
        match action {
            ContainerAction::Start => self.client.start_container(id, None).await?,
            ContainerAction::Stop => self.client.stop_container(id, None).await?,
            ContainerAction::Restart => self.client.restart_container(id, None).await?,
            ContainerAction::Pause => self.client.pause_container(id).await?,
            ContainerAction::Unpause => self.client.unpause_container(id).await?,
            ContainerAction::Remove { force } => {
                let options = RemoveContainerOptionsBuilder::default()
                    .force(force)
                    .build();
                self.client.remove_container(id, Some(options)).await?;
            }
        }

        Ok(())
    }

    async fn inspect_container(&self, id: &str) -> Result<ContainerInspect> {
        let response = self.client.inspect_container(id, None).await?;

        Ok(map_inspect(response))
    }

    async fn container_stats(&self, id: &str) -> Result<Stats> {
        let options = StatsOptionsBuilder::default().stream(false).build();

        let mut stream = self.client.stats(id, Some(options));
        let response = stream
            .next()
            .await
            .ok_or_else(|| Error::runtime("stats yanıtı boş döndü"))??;

        Ok(map_stats(response))
    }

    async fn logs(&self, id: &str, opts: super::LogOpts) -> Result<super::LogStream> {
        let mut builder = LogsOptionsBuilder::default()
            .follow(opts.follow)
            .stdout(true)
            .stderr(true)
            .timestamps(opts.timestamps);

        if let Some(tail) = opts.tail {
            builder = builder.tail(&tail.to_string());
        }

        let options = builder.build();

        // bollard'ın log akışı istemciden ödünç alır; 'static bir akış için
        // satırları arka planda bir kanala yönlendiriyoruz.
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let client = self.client.clone();
        let id = id.to_owned();

        tokio::spawn(async move {
            let mut stream = client.logs(&id, Some(options));

            while let Some(item) = stream.next().await {
                match item {
                    Ok(output) => {
                        // Parçalar satır sınırında olmayabilir; olduğu gibi
                        // iletiriz, istemci tarafında birleştirilir.
                        if tx.send(Ok(output.to_string())).await.is_err() {
                            break; // alıcı koptu
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(Err(Error::from(err))).await;
                        break;
                    }
                }
            }
        });

        Ok(Box::pin(futures_util::stream::unfold(
            rx,
            |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
        )))
    }

    async fn list_images(&self) -> Result<Vec<Image>> {
        let images = self.client.list_images(None::<ListImagesOptions>).await?;

        Ok(images
            .into_iter()
            .map(|image| Image {
                id: image.id,
                repo_tags: image.repo_tags,
                created: image.created,
                size: image.size.unsigned_abs(),
            })
            .collect())
    }

    async fn list_volumes(&self) -> Result<Vec<Volume>> {
        let response = self.client.list_volumes(None::<ListVolumesOptions>).await?;

        Ok(response
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(|volume| Volume {
                name: volume.name,
                driver: volume.driver,
                mountpoint: volume.mountpoint,
                created_at: volume.created_at,
            })
            .collect())
    }

    async fn list_networks(&self) -> Result<Vec<Network>> {
        let networks = self
            .client
            .list_networks(None::<ListNetworksOptions>)
            .await?;

        Ok(networks
            .into_iter()
            .map(|network| Network {
                id: network.id.unwrap_or_default(),
                name: network.name.unwrap_or_default(),
                driver: network.driver.unwrap_or_default(),
            })
            .collect())
    }

    async fn image_remove(&self, id: &str, force: bool) -> Result<()> {
        let options = bollard::query_parameters::RemoveImageOptionsBuilder::default()
            .force(force)
            .build();

        self.client.remove_image(id, Some(options), None).await?;
        Ok(())
    }

    async fn volume_remove(&self, name: &str) -> Result<()> {
        self.client
            .remove_volume(name, None::<bollard::query_parameters::RemoveVolumeOptions>)
            .await
            .map_err(Error::from)
    }

    async fn network_remove(&self, id: &str) -> Result<()> {
        self.client.remove_network(id).await.map_err(Error::from)
    }

    async fn events(&self) -> Result<EventStream> {
        // bollard'ın events akışı istemciden ödünç alır; 'static bir akış için
        // olayları arka planda bir kanala yönlendiriyoruz.
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let client = self.client.clone();

        tokio::spawn(async move {
            let mut stream = client.events(None);

            while let Some(item) = stream.next().await {
                match item {
                    Ok(message) => {
                        if tx.send(Ok(map_event_message(message))).await.is_err() {
                            break; // alıcı (SSE istemcisi) koptu
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(Err(Error::from(err))).await;
                        break;
                    }
                }
            }
        });

        Ok(Box::pin(futures_util::stream::unfold(
            rx,
            |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
        )))
    }
}

/// bollard `EventMessage`'ını ortak [`Event`] modeline çevirir.
fn map_event_message(message: bollard::models::EventMessage) -> Event {
    let (id, name) = match message.actor {
        Some(actor) => (
            actor.id.unwrap_or_default(),
            actor
                .attributes
                .map(|mut attrs| {
                    attrs
                        .remove("name")
                        .or_else(|| attrs.remove("container"))
                        .unwrap_or_default()
                })
                .unwrap_or_default(),
        ),
        None => (String::new(), String::new()),
    };

    Event {
        time: message.time.unwrap_or_default(),
        kind: message.typ.map(|t| t.to_string()).unwrap_or_default(),
        action: message.action.unwrap_or_default(),
        id,
        name,
    }
}

/// bollard `ContainerInspectResponse`'unu ortak modele çevirir.
fn map_inspect(response: bollard::models::ContainerInspectResponse) -> ContainerInspect {
    let state = response.state.unwrap_or_default();
    let config = response.config.unwrap_or_default();

    let ports = response
        .network_settings
        .and_then(|settings| settings.ports)
        .map(|port_map| {
            let mut ports: Vec<String> = port_map
                .into_iter()
                .map(|(container_port, bindings)| match bindings {
                    Some(bindings) if !bindings.is_empty() => bindings
                        .into_iter()
                        .map(|binding| {
                            format!(
                                "{}:{}->{}",
                                binding.host_ip.unwrap_or_default(),
                                binding.host_port.unwrap_or_default(),
                                container_port
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                    _ => container_port,
                })
                .collect();
            ports.sort();
            ports
        })
        .unwrap_or_default();

    let mounts = response
        .mounts
        .map(|mounts| {
            mounts
                .into_iter()
                .map(|mount| {
                    format!(
                        "{}:{}",
                        mount.source.unwrap_or_default(),
                        mount.destination.unwrap_or_default()
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    ContainerInspect {
        id: response.id.unwrap_or_default(),
        name: response
            .name
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_owned(),
        image: config.image.unwrap_or_default(),
        created: response.created.unwrap_or_default(),
        state: state
            .status
            .map(|status| status.to_string())
            .unwrap_or_default(),
        command: config
            .cmd
            .map(|cmd| cmd.join(" "))
            .filter(|cmd| !cmd.is_empty()),
        ports,
        mounts,
    }
}

/// bollard stats yanıtını ortak [`Stats`] modeline çevirir.
fn map_stats(response: bollard::models::ContainerStatsResponse) -> Stats {
    let cpu = response.cpu_stats.unwrap_or_default();
    let precpu = response.precpu_stats.unwrap_or_default();
    let memory = response.memory_stats.unwrap_or_default();

    // Docker'ın "used" tanımı: toplam kullanım - önbellek (cgroup v2:
    // inactive_file, v1: cache).
    let inactive = memory
        .stats
        .as_ref()
        .and_then(|stats| stats.get("inactive_file").or_else(|| stats.get("cache")))
        .copied()
        .unwrap_or(0);

    Stats {
        container_id: response.id.unwrap_or_default(),
        cpu_percent: compute_cpu_percent(&cpu, &precpu),
        mem_usage: memory.usage.unwrap_or(0).saturating_sub(inactive),
        mem_limit: memory.limit.unwrap_or(0),
    }
}

/// İki örnek arasındaki deltadan CPU yüzdesini hesaplar.
fn compute_cpu_percent(
    cpu: &bollard::models::ContainerCpuStats,
    precpu: &bollard::models::ContainerCpuStats,
) -> f64 {
    let total = cpu
        .cpu_usage
        .as_ref()
        .and_then(|usage| usage.total_usage)
        .unwrap_or(0);
    let pre_total = precpu
        .cpu_usage
        .as_ref()
        .and_then(|usage| usage.total_usage)
        .unwrap_or(0);
    let system = cpu.system_cpu_usage.unwrap_or(0);
    let pre_system = precpu.system_cpu_usage.unwrap_or(0);

    let cpu_delta = total.saturating_sub(pre_total) as f64;
    let system_delta = system.saturating_sub(pre_system) as f64;

    if system_delta <= 0.0 {
        return 0.0;
    }

    let cpus = cpu.online_cpus.filter(|cpus| *cpus > 0).unwrap_or(1) as f64;

    (cpu_delta / system_delta) * cpus * 100.0
}

fn docker_config_dir() -> Option<PathBuf> {
    if let Ok(config) = std::env::var("DOCKER_CONFIG") {
        return Some(PathBuf::from(config));
    }

    dirs::home_dir().map(|home| home.join(".docker"))
}

fn docker_current_context() -> io::Result<Option<String>> {
    let Some(config_dir) = docker_config_dir() else {
        return Ok(None);
    };

    let config_path = config_dir.join("config.json");

    if !config_path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(config_path)?;

    let config: serde_json::Value = serde_json::from_str(&contents).map_err(io::Error::other)?;

    Ok(config
        .get("currentContext")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned))
}

fn docker_context_endpoint(context: &str) -> io::Result<String> {
    let config_dir = docker_config_dir().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "Docker config directory not found")
    })?;

    let metadata_dir = config_dir.join("contexts").join("meta");

    let entries = std::fs::read_dir(&metadata_dir)?;

    for entry in entries {
        let entry = entry?;

        if !entry.file_type()?.is_dir() {
            continue;
        }

        let meta_path = entry.path().join("meta.json");

        if !meta_path.exists() {
            continue;
        }

        let contents = std::fs::read_to_string(meta_path)?;

        let metadata: serde_json::Value =
            serde_json::from_str(&contents).map_err(io::Error::other)?;

        if metadata.get("Name").and_then(serde_json::Value::as_str) != Some(context) {
            continue;
        }

        let host = metadata
            .get("Endpoints")
            .and_then(|value| value.get("docker"))
            .and_then(|value| value.get("Host"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Docker endpoint not found for context '{context}'"),
                )
            })?;

        return Ok(host.to_owned());
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("Docker context '{context}' not found"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_uri_adds_unix_scheme() {
        assert_eq!(
            Docker::host_uri("/var/run/docker.sock"),
            "unix:///var/run/docker.sock"
        );
        assert_eq!(
            Docker::host_uri("unix:///var/run/docker.sock"),
            "unix:///var/run/docker.sock"
        );
        assert_eq!(Docker::host_uri("ssh://user@host"), "ssh://user@host");
    }

    #[test]
    fn event_message_maps_to_event() {
        use std::collections::HashMap;

        let message = bollard::models::EventMessage {
            typ: Some(bollard::models::EventMessageTypeEnum::CONTAINER),
            action: Some("start".to_owned()),
            actor: Some(bollard::models::EventActor {
                id: Some("abc123".to_owned()),
                attributes: Some(HashMap::from([
                    ("name".to_owned(), "web".to_owned()),
                    ("image".to_owned(), "nginx".to_owned()),
                ])),
            }),
            time: Some(1_700_000_000),
            ..Default::default()
        };

        let event = map_event_message(message);
        assert_eq!(event.kind, "container");
        assert_eq!(event.action, "start");
        assert_eq!(event.id, "abc123");
        assert_eq!(event.name, "web");
        assert_eq!(event.time, 1_700_000_000);
    }

    #[test]
    fn event_message_without_actor_maps_to_empty() {
        let message = bollard::models::EventMessage {
            action: Some("destroy".to_owned()),
            ..Default::default()
        };

        let event = map_event_message(message);
        assert_eq!(event.kind, "");
        assert_eq!(event.id, "");
        assert_eq!(event.name, "");
        assert_eq!(event.action, "destroy");
    }

    #[test]
    fn cpu_percent_from_deltas() {
        let make = |total: u64, system: u64, cpus: u32| bollard::models::ContainerCpuStats {
            cpu_usage: Some(bollard::models::ContainerCpuUsage {
                total_usage: Some(total),
                ..Default::default()
            }),
            system_cpu_usage: Some(system),
            online_cpus: Some(cpus),
            ..Default::default()
        };

        // CPU deltasi 50, sistem deltasi 100, 1 CPU -> %50.
        let percent = compute_cpu_percent(&make(150, 1000, 1), &make(100, 900, 1));
        assert!((percent - 50.0).abs() < 0.01);

        // CPU deltasi 100, sistem deltasi 1000, 4 CPU -> (0.1 * 4) = %40.
        let percent = compute_cpu_percent(&make(200, 2000, 4), &make(100, 1000, 4));
        assert!((percent - 40.0).abs() < 0.01);

        // Sistem deltasi yoksa -> %0.
        let percent = compute_cpu_percent(&make(200, 1000, 4), &make(100, 1000, 4));
        assert_eq!(percent, 0.0);

        // Online CPU bildirilmemisse 1 varsayilir.
        let no_cpus = bollard::models::ContainerCpuStats {
            cpu_usage: Some(bollard::models::ContainerCpuUsage {
                total_usage: Some(150),
                ..Default::default()
            }),
            system_cpu_usage: Some(1000),
            ..Default::default()
        };
        let pre_no_cpus = bollard::models::ContainerCpuStats {
            cpu_usage: Some(bollard::models::ContainerCpuUsage {
                total_usage: Some(100),
                ..Default::default()
            }),
            system_cpu_usage: Some(900),
            ..Default::default()
        };
        assert!((compute_cpu_percent(&no_cpus, &pre_no_cpus) - 50.0).abs() < 0.01);
    }

    #[test]
    fn stats_maps_memory_and_cpu() {
        let response = bollard::models::ContainerStatsResponse {
            id: Some("abc".to_owned()),
            cpu_stats: Some(bollard::models::ContainerCpuStats {
                cpu_usage: Some(bollard::models::ContainerCpuUsage {
                    total_usage: Some(150),
                    ..Default::default()
                }),
                system_cpu_usage: Some(1000),
                online_cpus: Some(1),
                ..Default::default()
            }),
            precpu_stats: Some(bollard::models::ContainerCpuStats {
                cpu_usage: Some(bollard::models::ContainerCpuUsage {
                    total_usage: Some(100),
                    ..Default::default()
                }),
                system_cpu_usage: Some(900),
                ..Default::default()
            }),
            memory_stats: Some(bollard::models::ContainerMemoryStats {
                usage: Some(3000),
                limit: Some(10000),
                stats: Some(std::collections::HashMap::from([(
                    "inactive_file".to_owned(),
                    1000u64,
                )])),
                ..Default::default()
            }),
            ..Default::default()
        };

        let stats = map_stats(response);
        assert_eq!(stats.container_id, "abc");
        assert_eq!(stats.mem_usage, 2000); // 3000 - 1000 önbellek
        assert_eq!(stats.mem_limit, 10000);
        assert!((stats.cpu_percent - 50.0).abs() < 0.01); // 50/100 delta
    }
}
