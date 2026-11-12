//! Docker ve Podman için ortak runtime soyutlaması.
//!
//! Bu soyutlamayı panel (yerel socket) ve agent (uzak sunucudaki socket) kullanır;
//! böylece container yönetim kodu tek yerde yaşar.

pub mod docker;
pub mod libpod;
pub mod podman;

use futures_core::stream::BoxStream;

use crate::{
    error::{Error, Result},
    model::{Container, ContainerInspect, Event, Image, Network, Pod, RuntimeKind, Stats, Volume},
};

/// Container üzerinde uygulanabilecek eylemler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerAction {
    /// Container'ı başlatır.
    Start,
    /// Container'ı durdurur.
    Stop,
    /// Container'ı yeniden başlatır.
    Restart,
    /// Container'ı duraklatır.
    Pause,
    /// Duraklatılmış container'ı sürdürür.
    Unpause,
    /// Container'ı siler.
    Remove {
        /// Çalışıyor olsa bile zorla sil.
        force: bool,
    },
}

impl ContainerAction {
    /// URL yolunda kullanılan kısa ad.
    pub fn path_name(&self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Pause => "pause",
            Self::Unpause => "unpause",
            Self::Remove { .. } => "remove",
        }
    }
}

/// URL yolundaki eylem adını [`ContainerAction`]a çevirir.
///
/// Panel ve agent aynı kuralları kullanır; `remove` güvenli tarafta kalır
/// (zorlamasız).
pub fn parse_container_action(action: &str) -> Option<ContainerAction> {
    match action {
        "start" => Some(ContainerAction::Start),
        "stop" => Some(ContainerAction::Stop),
        "restart" => Some(ContainerAction::Restart),
        "pause" => Some(ContainerAction::Pause),
        "unpause" => Some(ContainerAction::Unpause),
        "remove" => Some(ContainerAction::Remove { force: false }),
        _ => None,
    }
}

/// Pod üzerinde uygulanabilecek eylemler (yalnızca Podman).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PodAction {
    /// Pod'u başlatır.
    Start,
    /// Pod'u durdurur.
    Stop,
    /// Pod'u siler.
    Remove {
        /// Çalışıyor olsa bile zorla sil.
        force: bool,
    },
}

impl PodAction {
    /// URL yolunda kullanılan kısa ad.
    pub fn path_name(&self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Remove { .. } => "remove",
        }
    }
}

/// URL yolundaki eylem adını [`PodAction`]a çevirir.
///
/// Pod silme her zaman zorlamalıdır; podman pod'ları genellikle içinde
/// container olduğu sürece zorlamasız silinemez.
pub fn parse_pod_action(action: &str) -> Option<PodAction> {
    match action {
        "start" => Some(PodAction::Start),
        "stop" => Some(PodAction::Stop),
        "remove" => Some(PodAction::Remove { force: true }),
        _ => None,
    }
}

/// Log okuma seçenekleri.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogOpts {
    /// Akışı açık tut ve yeni satırları akıt.
    pub follow: bool,
    /// Yalnızca son N satır.
    pub tail: Option<u64>,
    /// Satırlara zaman damgası ekle.
    pub timestamps: bool,
}

/// Canlı log akışı.
pub type LogStream = BoxStream<'static, Result<String>>;
/// Canlı olay akışı.
pub type EventStream = BoxStream<'static, Result<Event>>;

/// Bir container runtime'ına (Docker veya Podman) erişim.
///
/// Akış tabanlı ve Podman'a özgü yöntemler varsayılan olarak
/// [`Error::Unsupported`] döner; ilgili fazlarda implementasyonlar eklenir:
/// - ~~Faz 1: `events`~~
/// - ~~Faz 2: `logs`, `container_stats`, `inspect_container`~~
/// - Faz 3: `list_pods`, `pod_action` (Podman)
#[async_trait::async_trait]
pub trait Runtime: Send + Sync {
    /// Runtime türü.
    fn kind(&self) -> RuntimeKind;

    /// Runtime'a erişilebildiğini doğrular (daemon ayakta mı?).
    async fn ping(&self) -> Result<()> {
        Err(Error::unsupported("ping"))
    }

    /// Tüm container'ları listeler (çalışan + durmuş).
    async fn list_containers(&self) -> Result<Vec<Container>> {
        Err(Error::unsupported("list_containers"))
    }

    /// Bir container'ın ayrıntılı bilgisi.
    async fn inspect_container(&self, _id: &str) -> Result<ContainerInspect> {
        Err(Error::unsupported("inspect_container"))
    }

    /// Bir container üzerinde eylem uygular.
    async fn container_action(&self, _id: &str, _action: ContainerAction) -> Result<()> {
        Err(Error::unsupported("container_action"))
    }

    /// Image'ları listeler.
    async fn list_images(&self) -> Result<Vec<Image>> {
        Err(Error::unsupported("list_images"))
    }

    /// Volume'ları listeler.
    async fn list_volumes(&self) -> Result<Vec<Volume>> {
        Err(Error::unsupported("list_volumes"))
    }

    /// Network'leri listeler.
    async fn list_networks(&self) -> Result<Vec<Network>> {
        Err(Error::unsupported("list_networks"))
    }

    /// Bir image'ı siler.
    async fn image_remove(&self, _id: &str, _force: bool) -> Result<()> {
        Err(Error::unsupported("image_remove"))
    }

    /// Bir volume'u siler.
    async fn volume_remove(&self, _name: &str) -> Result<()> {
        Err(Error::unsupported("volume_remove"))
    }

    /// Bir network'ü siler.
    async fn network_remove(&self, _id: &str) -> Result<()> {
        Err(Error::unsupported("network_remove"))
    }

    /// Bir container'ın anlık istatistikleri.
    async fn container_stats(&self, _id: &str) -> Result<Stats> {
        Err(Error::unsupported("container_stats"))
    }

    /// Bir container'ın log akışı.
    async fn logs(&self, _id: &str, _opts: LogOpts) -> Result<LogStream> {
        Err(Error::unsupported("logs"))
    }

    /// Runtime olay akışı (docker/podman events).
    async fn events(&self) -> Result<EventStream> {
        Err(Error::unsupported("events"))
    }

    /// Podman pod'larını listeler.
    async fn list_pods(&self) -> Result<Vec<Pod>> {
        Err(Error::unsupported("list_pods"))
    }

    /// Bir pod üzerinde eylem uygular.
    async fn pod_action(&self, _id: &str, _action: PodAction) -> Result<()> {
        Err(Error::unsupported("pod_action"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_action_path_names_roundtrip() {
        // Not: `remove` çözümlendiğinde her zaman güvenli tarafta kalır
        // (`force: false`), bu yüzden yuvarlama testinin dışında tutulur.
        for action in [
            ContainerAction::Start,
            ContainerAction::Stop,
            ContainerAction::Restart,
            ContainerAction::Pause,
            ContainerAction::Unpause,
        ] {
            assert_eq!(parse_container_action(action.path_name()), Some(action));
        }

        assert_eq!(
            parse_container_action(ContainerAction::Remove { force: true }.path_name()),
            Some(ContainerAction::Remove { force: false })
        );
        assert_eq!(parse_container_action("yok"), None);
    }

    #[test]
    fn pod_action_path_names_roundtrip() {
        assert_eq!(parse_pod_action("start"), Some(PodAction::Start));
        assert_eq!(parse_pod_action("stop"), Some(PodAction::Stop));
        assert_eq!(
            parse_pod_action("remove"),
            Some(PodAction::Remove { force: true })
        );
        assert_eq!(parse_pod_action("yok"), None);
    }
}
