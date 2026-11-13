//! Runtime olay akışlarını tek bir kanalda birleştirir.
//!
//! Her hostun `events()` akışı için bir arka plan görevi çalışır; container ve
//! pod olayları [`HostEvent`] olarak yayınlanır. SSE uç noktası
//! (`/api/events`) bu kanala abone olur ve htmx arayüzünü tetikler.

use std::time::Duration;

use futures_util::StreamExt;
use pdman_common::model::Event;
use tokio::{sync::broadcast, task::JoinHandle};

use crate::hosts::HostEntry;

/// Koptuktan sonra olay akışını yeniden açmadan önce beklenen süre.
const RETRY_INTERVAL: Duration = Duration::from_secs(10);

/// Liste yenilemeyi tetikleyen olay türleri.
const REFRESH_KINDS: [&str; 2] = ["container", "pod"];

/// Panele iletilen host olayı.
#[derive(Debug, Clone)]
pub struct HostEvent {
    /// Olayı üreten hostun kimliği (`docker`, `podman`, ...).
    pub host_id: String,
    /// Hostun panelde görünen adı.
    pub host_name: String,
    /// Runtime olayı.
    pub event: Event,
}

/// Tek bir hostun olay ileticisini başlatır; çalışmadığı bir runtime
/// (`None`) için no-op görevi döner. Dönen tutamaç sunucu silinirken
/// görevi iptal etmek için kullanılır.
pub fn spawn_forwarder(host: &HostEntry, sender: broadcast::Sender<HostEvent>) -> JoinHandle<()> {
    let Some(runtime) = host.runtime.clone() else {
        return tokio::spawn(async {});
    };

    let host_id = host.id.clone();
    let host_name = host.name.clone();

    tokio::spawn(async move {
        // Daemon kapatılıp yeniden başlayabilir; akış kopunca yeniden dene.
        loop {
            let stream = match runtime.events().await {
                Ok(stream) => stream,
                Err(err) => {
                    eprintln!("pdman: {host_id} olay akışı açılamadı: {err}");
                    tokio::time::sleep(RETRY_INTERVAL).await;
                    continue;
                }
            };

            let mut stream = stream;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(event) if REFRESH_KINDS.contains(&event.kind.as_str()) => {
                        // Alıcı olmasa da sorun değil; htmx yeniden bağlanır.
                        let _ = sender.send(HostEvent {
                            host_id: host_id.clone(),
                            host_name: host_name.clone(),
                            event,
                        });
                    }
                    Ok(_) => {}
                    Err(err) => {
                        eprintln!("pdman: {host_id} olay akışı koptu: {err}");
                        break;
                    }
                }
            }

            // Akış bitti (daemon durmuş olabilir); bekleyip yeniden dene.
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    })
}
