//! pdman — Docker + Podman yönetim paneli.

mod agent;
mod config;
mod events;
mod hosts;
mod routes;

use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    routing::{get, post},
};
use clap::Parser;
use tokio::sync::broadcast;

use config::Config;
use events::{HostEvent, spawn_forwarders};
use hosts::HostRegistry;

/// Panelin paylaşılan durumu.
pub struct AppState {
    /// Kayıtlı hostlar (yerel docker/podman; Faz 4'te uzak agentlar).
    pub registry: HostRegistry,
    /// Birleştirilmiş runtime olay kanalı (SSE ucuna besler).
    pub events: broadcast::Sender<HostEvent>,
}

/// Handler'ların gördüğü durum tipi.
pub type App = Arc<AppState>;

#[derive(Parser)]
#[command(name = "pdman", version, about = "Docker + Podman yönetim paneli")]
struct Cli {
    /// Yapılandırma dosyasının yolu.
    #[arg(short, long, default_value = "pdman.toml")]
    config: PathBuf,
}

fn load_config(cli: &Cli) -> Config {
    // Varsayılan yol yoksa sessizce varsayılan ayarlarla devam et; kullanıcı
    // açıkça bir yol verdiyse hata bildir.
    if !cli.config.exists() && cli.config.as_os_str() == "pdman.toml" {
        return Config::default();
    }

    Config::load(&cli.config).unwrap_or_else(|err| {
        eprintln!("pdman: {err}");
        std::process::exit(1);
    })
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let config = load_config(&cli);

    let registry = HostRegistry::build(&config).await;
    for host in registry.hosts() {
        match (&host.runtime, &host.error) {
            (Some(_), None) => println!("pdman: {} hazır", host.name),
            _ => println!(
                "pdman: {} kullanılamıyor: {}",
                host.name,
                host.error.as_deref().unwrap_or("bilinmeyen neden")
            ),
        }
    }

    let (events_tx, _events_rx) = broadcast::channel(64);
    spawn_forwarders(registry.hosts(), events_tx.clone());

    let state = Arc::new(AppState {
        registry,
        events: events_tx,
    });

    let app = Router::new()
        .route("/", get(routes::index))
        .route("/containers/{id}", get(routes::container_detail))
        .route(
            "/containers/{id}/{action}",
            axum::routing::post(routes::container_action),
        )
        .route("/partials/containers", get(routes::containers_partial))
        .route(
            "/partials/container-stats",
            get(routes::container_stats_partial),
        )
        .route("/pods", get(routes::pods_page))
        .route("/pods/{id}/{action}", post(routes::pod_action))
        .route("/partials/pods", get(routes::pods_partial))
        .route("/images", get(routes::images_page))
        .route("/images/{id}/remove", post(routes::image_remove))
        .route("/partials/images", get(routes::images_partial))
        .route("/volumes", get(routes::volumes_page))
        .route("/volumes/{name}/remove", post(routes::volume_remove))
        .route("/partials/volumes", get(routes::volumes_partial))
        .route("/networks", get(routes::networks_page))
        .route("/networks/{id}/remove", post(routes::network_remove))
        .route("/partials/networks", get(routes::networks_partial))
        .route("/api/containers", get(routes::api_containers))
        .route("/api/containers/{id}/logs", get(routes::container_logs_sse))
        .route("/api/events", get(routes::sse_events))
        .route("/static/main.css", get(routes::main_css))
        .with_state(state);

    let bind = config.server.bind.clone();
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|err| panic!("pdman: {bind} adresi dinlenemedi: {err}"));

    println!("pdman: http://{bind}");

    axum::serve(listener, app).await.expect("server error");
}
