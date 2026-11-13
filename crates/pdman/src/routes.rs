//! HTTP uçları: UI sayfaları, htmx partial'ları, JSON API ve SSE.

use std::{convert::Infallible, sync::Arc};

use askama::Template;
use axum::{
    Form, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        Html, IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast;

use pdman_common::{
    model::{Container, ContainerInspect, Pod, RuntimeKind},
    runtime::{ContainerAction, LogOpts, PodAction},
};

use crate::{
    App, AppState,
    agent::{AgentClient, AgentTls},
    db::ServerRecord,
    hosts::{AgentConnection, HostEntry, HostSource, build_agent_hosts},
};

/// `?host=` sorgu parametresi.
#[derive(Debug, Deserialize)]
pub struct HostQuery {
    host: Option<String>,
}

/// `?host=&id=` sorgu parametreleri (stats partial).
#[derive(Debug, Deserialize)]
pub struct ContainerQuery {
    host: Option<String>,
    id: String,
}

/// `?host=&tail=&follow=` sorgu parametreleri (log akışı).
#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    host: Option<String>,
    tail: Option<u64>,
    follow: Option<bool>,
}

/// Kenar çubuğundaki host görünümü.
struct HostNav {
    id: String,
    /// Grup başlığı runtime'ı zaten gösterdiğinden kind öneki soyulmuş ad
    /// ("Docker (yerel)" → "yerel").
    label: String,
    available: bool,
}

/// Tüm sayfaların paylaştığı çerçeve verisi: runtime'a göre ayrılmış host
/// listesi (kenar çubuğu) ve seçili host bilgisi (üst bilgi çubuğu).
struct Layout {
    docker_hosts: Vec<HostNav>,
    podman_hosts: Vec<HostNav>,
    host_id: String,
    host_name: String,
    host_kind: String,
    host_available: bool,
    is_podman: bool,
    path: String,
    active: String,
    title: String,
    /// Seçili runtime'ın marka rengiyle renklenmiş sınıflar (Docker mavi,
    /// Podman mor). Şablonlarda koşul blokları yerine tek noktadan beslenir.
    brand_badge_class: &'static str,
    nav_active_class: &'static str,
}

/// Container özet kartları için sayaçlar.
struct ContainerCounts {
    total: usize,
    running: usize,
    paused: usize,
    stopped: usize,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    layout: Layout,
    host_id: String,
    containers: Vec<Container>,
    counts: ContainerCounts,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "partials/containers.html")]
struct ContainersTemplate {
    host_id: String,
    containers: Vec<Container>,
    counts: ContainerCounts,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "container.html")]
struct ContainerDetailTemplate {
    layout: Layout,
    host_id: String,
    id: String,
    inspect: ContainerInspect,
}

#[derive(Template)]
#[template(path = "pods.html")]
struct PodsPage {
    layout: Layout,
    host_id: String,
    pods: Vec<Pod>,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "partials/pods.html")]
struct PodsPartial {
    host_id: String,
    pods: Vec<Pod>,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "images.html")]
struct ImagesPage {
    layout: Layout,
    host_id: String,
    images: Vec<ImageView>,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "partials/images.html")]
struct ImagesPartial {
    host_id: String,
    images: Vec<ImageView>,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "volumes.html")]
struct VolumesPage {
    layout: Layout,
    host_id: String,
    volumes: Vec<VolumeView>,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "partials/volumes.html")]
struct VolumesPartial {
    host_id: String,
    volumes: Vec<VolumeView>,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "networks.html")]
struct NetworksPage {
    layout: Layout,
    host_id: String,
    networks: Vec<NetworkView>,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "partials/networks.html")]
struct NetworksPartial {
    host_id: String,
    networks: Vec<NetworkView>,
    error: Option<String>,
}

/// Tablo görünümü için image satırı.
struct ImageView {
    tags: String,
    id: String,
    size: String,
    created: String,
}

/// Tablo görünümü için volume satırı.
struct VolumeView {
    name: String,
    driver: String,
    created_at: String,
}

/// Tablo görünümü için network satırı.
struct NetworkView {
    id: String,
    name: String,
    driver: String,
}

#[derive(Template)]
#[template(path = "partials/stats.html")]
struct StatsTemplate {
    host_id: String,
    id: String,
    cpu: String,
    mem: String,
    mem_limit: String,
    mem_percent: u32,
    error: Option<String>,
}

/// Ana sayfa: kenar çubuğu + seçili hostun container listesi.
pub async fn index(State(state): State<App>, Query(query): Query<HostQuery>) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (containers, error) = load_containers(&host).await;

    render(IndexTemplate {
        layout: layout(
            &state,
            Some(host.as_ref()),
            "/",
            "containers",
            "Container'lar",
        ),
        host_id: host.id.clone(),
        counts: container_counts(&containers),
        containers,
        error,
    })
}

/// Container tablosu partial'ı (htmx ile yenilenir).
pub async fn containers_partial(
    State(state): State<App>,
    Query(query): Query<HostQuery>,
) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (containers, error) = load_containers(&host).await;

    render(ContainersTemplate {
        host_id: host.id.clone(),
        counts: container_counts(&containers),
        containers,
        error,
    })
}

/// `GET /api/containers?host=` — JSON API.
// axum'un Response tipi büyük; hata tipini küçültmek değere değmiyor.
#[allow(clippy::result_large_err)]
pub async fn api_containers(
    State(state): State<App>,
    Query(query): Query<HostQuery>,
) -> Result<Json<Vec<Container>>, Response> {
    let host = resolve_host(&state, &query)?;

    let Some(runtime) = &host.runtime else {
        return Err(service_unavailable(
            host.error
                .clone()
                .unwrap_or_else(|| "runtime kullanılamıyor".to_owned()),
        ));
    };

    let containers = runtime.list_containers().await.map_err(internal_error)?;

    Ok(Json(containers))
}

/// `GET /api/events` — host olaylarını SSE olarak yayınlar.
///
/// Her olay `containers-changed` adıyla gönderilir; htmx bu tetikleyiciyle
/// container tablosunu yeniler.
pub async fn sse_events(
    State(state): State<App>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = state.events.subscribe();

    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(host_event) => {
                    let data = serde_json::to_string(&host_event.event).unwrap_or_default();
                    let event = SseEvent::default().event("containers-changed").data(data);
                    return Some((Ok(event), rx));
                }
                // Yavaş istemci: kaçırdığı olayları atla.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                // Tüm ileticiler kapandı.
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `POST /containers/{id}/{action}` — container eylemi uygular ve taze
/// container tablosunu döner.
///
/// Eylemler: `start`, `stop`, `restart`, `pause`, `unpause`, `remove`.
pub async fn container_action(
    State(state): State<App>,
    Path((id, action)): Path<(String, String)>,
    Query(query): Query<HostQuery>,
) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let Some(action) = parse_action(&action) else {
        return (StatusCode::NOT_FOUND, format!("bilinmeyen eylem: {action}")).into_response();
    };

    let (containers, error) = match &host.runtime {
        Some(runtime) => match runtime.container_action(&id, action).await {
            Ok(()) => load_containers(&host).await,
            Err(err) => {
                let (containers, _) = load_containers(&host).await;
                (containers, Some(format!("eylem başarısız: {err}")))
            }
        },
        None => (Vec::new(), Some(host_unavailable_message(&host))),
    };

    render(ContainersTemplate {
        host_id: host.id.clone(),
        counts: container_counts(&containers),
        containers,
        error,
    })
}

/// `GET /containers/{id}` — container detay sayfası (inspect + stats + loglar).
pub async fn container_detail(
    State(state): State<App>,
    Path(id): Path<String>,
    Query(query): Query<HostQuery>,
) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let Some(runtime) = &host.runtime else {
        return service_unavailable(host_unavailable_message(&host));
    };

    match runtime.inspect_container(&id).await {
        Ok(inspect) => render(ContainerDetailTemplate {
            layout: layout(
                &state,
                Some(host.as_ref()),
                "/",
                "containers",
                "Container detayı",
            ),
            host_id: host.id.clone(),
            id,
            inspect,
        }),
        Err(err) => internal_error(err),
    }
}

/// `GET /partials/container-stats?host=&id=` — stats kartı (2 saniyede bir
/// yenilenir).
pub async fn container_stats_partial(
    State(state): State<App>,
    Query(query): Query<ContainerQuery>,
) -> Response {
    let host = match resolve_host(&state, &HostQuery { host: query.host }) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let Some(runtime) = &host.runtime else {
        return render(StatsTemplate {
            host_id: host.id.clone(),
            id: query.id,
            cpu: String::new(),
            mem: String::new(),
            mem_limit: String::new(),
            mem_percent: 0,
            error: Some(host_unavailable_message(&host)),
        });
    };

    let template = match runtime.container_stats(&query.id).await {
        Ok(stats) => StatsTemplate {
            host_id: host.id.clone(),
            id: query.id,
            cpu: format!("{:.1}%", stats.cpu_percent),
            mem: format_bytes(stats.mem_usage),
            mem_limit: format_bytes(stats.mem_limit),
            mem_percent: mem_percent(stats.mem_usage, stats.mem_limit),
            error: None,
        },
        Err(err) => StatsTemplate {
            host_id: host.id.clone(),
            id: query.id,
            cpu: String::new(),
            mem: String::new(),
            mem_limit: String::new(),
            mem_percent: 0,
            error: Some(err.to_string()),
        },
    };

    render(template)
}

/// `GET /api/containers/{id}/logs?host=&tail=&follow=` — log akışı (SSE).
///
/// Satırlar `log-line` adlı olaylar olarak gönderilir; htmx bunları log
/// görünümünün sonuna ekler.
pub async fn container_logs_sse(
    State(state): State<App>,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Response {
    let host = match resolve_host(&state, &HostQuery { host: query.host }) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let Some(runtime) = &host.runtime else {
        return service_unavailable(host_unavailable_message(&host));
    };

    let opts = LogOpts {
        follow: query.follow.unwrap_or(true),
        tail: query.tail,
        timestamps: true,
    };

    let stream = match runtime.logs(&id, opts).await {
        Ok(stream) => stream,
        Err(err) => return internal_error(err),
    };

    let stream = stream.filter_map(|item| async move {
        match item {
            // Log satırları HTML olarak yerleştirildiğinden kaçışlanmalı.
            Ok(line) => Some(Ok::<_, Infallible>(
                SseEvent::default()
                    .event("log-line")
                    .data(escape_html(&line)),
            )),
            Err(err) => {
                eprintln!("pdman: log akışı koptu: {err}");
                None
            }
        }
    });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// `GET /pods?host=` — pod listesi sayfası (yalnız Podman).
pub async fn pods_page(State(state): State<App>, Query(query): Query<HostQuery>) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (pods, error) = load_pods(&host).await;

    render(PodsPage {
        layout: layout(&state, Some(host.as_ref()), "/pods", "pods", "Pod'lar"),
        host_id: host.id.clone(),
        pods,
        error,
    })
}

/// `GET /partials/pods?host=` — pod tablosu partial'ı.
pub async fn pods_partial(State(state): State<App>, Query(query): Query<HostQuery>) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (pods, error) = load_pods(&host).await;

    render(PodsPartial {
        host_id: host.id.clone(),
        pods,
        error,
    })
}

/// `POST /pods/{id}/{action}` — pod eylemi (start/stop/remove) uygular ve
/// taze pod tablosunu döner.
pub async fn pod_action(
    State(state): State<App>,
    Path((id, action)): Path<(String, String)>,
    Query(query): Query<HostQuery>,
) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let Some(action) = parse_pod_action(&action) else {
        return (StatusCode::NOT_FOUND, format!("bilinmeyen eylem: {action}")).into_response();
    };

    let (pods, error) = match &host.runtime {
        Some(runtime) => match runtime.pod_action(&id, action).await {
            Ok(()) => load_pods(&host).await,
            Err(err) => {
                let (pods, _) = load_pods(&host).await;
                (pods, Some(format!("eylem başarısız: {err}")))
            }
        },
        None => (Vec::new(), Some(host_unavailable_message(&host))),
    };

    render(PodsPartial {
        host_id: host.id.clone(),
        pods,
        error,
    })
}

/// `GET /images?host=` — image listesi sayfası.
pub async fn images_page(State(state): State<App>, Query(query): Query<HostQuery>) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (images, error) = load_images(&host).await;

    render(ImagesPage {
        layout: layout(
            &state,
            Some(host.as_ref()),
            "/images",
            "images",
            "Image'lar",
        ),
        host_id: host.id.clone(),
        images,
        error,
    })
}

/// `GET /partials/images?host=` — image tablosu partial'ı.
pub async fn images_partial(State(state): State<App>, Query(query): Query<HostQuery>) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (images, error) = load_images(&host).await;

    render(ImagesPartial {
        host_id: host.id.clone(),
        images,
        error,
    })
}

/// `POST /images/{id}/remove` — image siler ve taze tabloyu döner.
pub async fn image_remove(
    State(state): State<App>,
    Path(id): Path<String>,
    Query(query): Query<HostQuery>,
) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (images, error) = match &host.runtime {
        Some(runtime) => match runtime.image_remove(&id, false).await {
            Ok(()) => load_images(&host).await,
            Err(err) => {
                let (images, _) = load_images(&host).await;
                (images, Some(format!("eylem başarısız: {err}")))
            }
        },
        None => (Vec::new(), Some(host_unavailable_message(&host))),
    };

    render(ImagesPartial {
        host_id: host.id.clone(),
        images,
        error,
    })
}

/// `GET /volumes?host=` — volume listesi sayfası.
pub async fn volumes_page(State(state): State<App>, Query(query): Query<HostQuery>) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (volumes, error) = load_volumes(&host).await;

    render(VolumesPage {
        layout: layout(
            &state,
            Some(host.as_ref()),
            "/volumes",
            "volumes",
            "Volume'lar",
        ),
        host_id: host.id.clone(),
        volumes,
        error,
    })
}

/// `GET /partials/volumes?host=` — volume tablosu partial'ı.
pub async fn volumes_partial(State(state): State<App>, Query(query): Query<HostQuery>) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (volumes, error) = load_volumes(&host).await;

    render(VolumesPartial {
        host_id: host.id.clone(),
        volumes,
        error,
    })
}

/// `POST /volumes/{name}/remove` — volume siler ve taze tabloyu döner.
pub async fn volume_remove(
    State(state): State<App>,
    Path(name): Path<String>,
    Query(query): Query<HostQuery>,
) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (volumes, error) = match &host.runtime {
        Some(runtime) => match runtime.volume_remove(&name).await {
            Ok(()) => load_volumes(&host).await,
            Err(err) => {
                let (volumes, _) = load_volumes(&host).await;
                (volumes, Some(format!("eylem başarısız: {err}")))
            }
        },
        None => (Vec::new(), Some(host_unavailable_message(&host))),
    };

    render(VolumesPartial {
        host_id: host.id.clone(),
        volumes,
        error,
    })
}

/// `GET /networks?host=` — network listesi sayfası.
pub async fn networks_page(State(state): State<App>, Query(query): Query<HostQuery>) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (networks, error) = load_networks(&host).await;

    render(NetworksPage {
        layout: layout(
            &state,
            Some(host.as_ref()),
            "/networks",
            "networks",
            "Network'ler",
        ),
        host_id: host.id.clone(),
        networks,
        error,
    })
}

/// `GET /partials/networks?host=` — network tablosu partial'ı.
pub async fn networks_partial(
    State(state): State<App>,
    Query(query): Query<HostQuery>,
) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (networks, error) = load_networks(&host).await;

    render(NetworksPartial {
        host_id: host.id.clone(),
        networks,
        error,
    })
}

/// `POST /networks/{id}/remove` — network siler ve taze tabloyu döner.
pub async fn network_remove(
    State(state): State<App>,
    Path(id): Path<String>,
    Query(query): Query<HostQuery>,
) -> Response {
    let host = match resolve_host(&state, &query) {
        Ok(host) => host,
        Err(response) => return response,
    };

    let (networks, error) = match &host.runtime {
        Some(runtime) => match runtime.network_remove(&id).await {
            Ok(()) => load_networks(&host).await,
            Err(err) => {
                let (networks, _) = load_networks(&host).await;
                (networks, Some(format!("eylem başarısız: {err}")))
            }
        },
        None => (Vec::new(), Some(host_unavailable_message(&host))),
    };

    render(NetworksPartial {
        host_id: host.id.clone(),
        networks,
        error,
    })
}

// ———— Sunucu yönetimi (Faz 6) ————

/// `POST /servers/add` form alanları.
#[derive(Debug, Deserialize)]
pub struct ServerForm {
    name: String,
    url: String,
    token: Option<String>,
    ca_file: Option<String>,
    cert_file: Option<String>,
    key_file: Option<String>,
}

/// Sunucu tablosundaki tek satır (bir agent'ın tüm runtime kayıtları
/// birleştirilerek oluşturulur).
struct ServerCard {
    id: String,
    name: String,
    url: String,
    /// `config` ya da `database`.
    source: &'static str,
    /// Kullanılabilir runtime'lar ("docker, podman").
    kinds: String,
    tls: bool,
    mtls: bool,
    token: bool,
    available: bool,
    error: Option<String>,
    /// Arayüzden silinebilir mi (yalnız veritabanı kaynaklı)?
    removable: bool,
}

/// Yerel runtime satırı.
struct LocalCard {
    name: String,
    available: bool,
}

/// Eylem sonrası görünen kısa ileti.
struct Flash {
    success: bool,
    message: String,
}

impl Flash {
    fn success(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
        }
    }
}

#[derive(Template)]
#[template(path = "servers.html")]
struct ServersPage {
    layout: Layout,
    db_ready: bool,
    servers: Vec<ServerCard>,
    local_hosts: Vec<LocalCard>,
    flash: Option<Flash>,
}

#[derive(Template)]
#[template(path = "partials/servers.html")]
struct ServersPartial {
    db_ready: bool,
    servers: Vec<ServerCard>,
    local_hosts: Vec<LocalCard>,
    flash: Option<Flash>,
}

/// `GET /servers` — sunucu yönetim sayfası.
pub async fn servers_page(State(state): State<App>) -> Response {
    let host = state.registry.default_host();

    render(ServersPage {
        layout: layout(&state, host.as_deref(), "/", "servers", "Sunucular"),
        db_ready: state.db.is_some(),
        servers: server_cards(&state),
        local_hosts: local_cards(&state),
        flash: None,
    })
}

/// `GET /partials/servers` — sunucu tablosu partial'ı.
pub async fn servers_partial(State(state): State<App>) -> Response {
    render_servers_partial(&state, None)
}

/// `POST /servers/add` — yeni sunucu ekler.
///
/// Formu doğrular, agent'a sağlık denetimi yapar; yalnızca ulaşılabilen
/// sunucular kaydedilir. Kayıt PostgreSQL'e yazılır ve hostlar kayıt
/// defterine girer (olay ileticileri dahil).
pub async fn servers_add(State(state): State<App>, Form(form): Form<ServerForm>) -> Response {
    let Some(db) = state.db.as_ref() else {
        return render_servers_partial(
            &state,
            Some(Flash::error(
                "veritabanı yapılandırılmamış (pdman.toml: [database] url)",
            )),
        );
    };

    let name = form.name.trim().to_owned();
    let url = form.url.trim().to_owned();
    let token = form.token.as_deref().unwrap_or("").trim().to_owned();

    if let Err(message) = validate_server_form(&name, &url, &token) {
        return render_servers_partial(&state, Some(Flash::error(message)));
    }

    let id = slugify(&name);
    if state
        .registry
        .hosts()
        .iter()
        .any(|host| host.server_id == id)
    {
        return render_servers_partial(
            &state,
            Some(Flash::error(format!(
                "{name} adında bir sunucu zaten kayıtlı"
            ))),
        );
    }

    let tls = AgentTls {
        ca_file: trimmed(&form.ca_file),
        cert_file: trimmed(&form.cert_file),
        key_file: trimmed(&form.key_file),
    };

    // Bağlantı güvenliği: agent'a ulaşılırsa kaydet.
    if let Err(err) = AgentClient::health(&url, &token, tls.clone()).await {
        return render_servers_partial(
            &state,
            Some(Flash::error(format!("agent'e ulaşılamadı: {err}"))),
        );
    }

    let connection = AgentConnection {
        server_id: id.clone(),
        name: name.clone(),
        url: url.clone(),
        token: token.clone(),
        tls: tls.clone(),
        source: HostSource::Database,
    };

    let entries = build_agent_hosts(&connection).await;

    let record = ServerRecord {
        id: id.clone(),
        name: name.clone(),
        url,
        token,
        ca_file: tls.ca_file.clone().unwrap_or_default(),
        cert_file: tls.cert_file.clone().unwrap_or_default(),
        key_file: tls.key_file.clone().unwrap_or_default(),
    };
    if let Err(err) = db.insert_server(&record).await {
        return render_servers_partial(
            &state,
            Some(Flash::error(format!("sunucu kaydedilemedi: {err}"))),
        );
    }

    let kinds = entries
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    state.registry.upsert_server(&id, entries);

    render_servers_partial(
        &state,
        Some(Flash::success(format!("{name} eklendi ({kinds})"))),
    )
}

/// `POST /servers/{id}/remove` — sunucuyu veritabanından ve kayıt
/// defterinden kaldırır.
pub async fn servers_remove(State(state): State<App>, Path(id): Path<String>) -> Response {
    let Some(db) = state.db.as_ref() else {
        return render_servers_partial(
            &state,
            Some(Flash::error(
                "veritabanı yapılandırılmamış (pdman.toml: [database] url)",
            )),
        );
    };

    if state
        .registry
        .hosts()
        .iter()
        .any(|host| host.server_id == id && host.source != HostSource::Database)
    {
        return render_servers_partial(
            &state,
            Some(Flash::error(
                "yalnızca arayüzden eklenen sunucular silinebilir (pdman.toml'dakiler dosyadan yönetilir)",
            )),
        );
    }

    if let Err(err) = db.delete_server(&id).await {
        return render_servers_partial(
            &state,
            Some(Flash::error(format!("sunucu silinemedi: {err}"))),
        );
    }

    state.registry.remove_server(&id);
    render_servers_partial(&state, Some(Flash::success("sunucu silindi")))
}

/// `POST /servers/{id}/check` — sunucunun bağlantısını yeniden sınar ve
/// durumunu tazeler.
pub async fn servers_check(State(state): State<App>, Path(id): Path<String>) -> Response {
    let Some(db) = state.db.as_ref() else {
        return render_servers_partial(
            &state,
            Some(Flash::error(
                "veritabanı yapılandırılmamış (pdman.toml: [database] url)",
            )),
        );
    };

    let servers = match db.list_servers().await {
        Ok(servers) => servers,
        Err(err) => {
            return render_servers_partial(
                &state,
                Some(Flash::error(format!("sunucular okunamadı: {err}"))),
            );
        }
    };

    let Some(record) = servers.iter().find(|record| record.id == id) else {
        return render_servers_partial(&state, Some(Flash::error("sunucu bulunamadı")));
    };

    let connection = AgentConnection::from_record(record);
    let entries = build_agent_hosts(&connection).await;
    state.registry.upsert_server(&id, entries);

    match state
        .registry
        .hosts()
        .iter()
        .find(|host| host.server_id == id)
    {
        Some(host) if host.available => render_servers_partial(
            &state,
            Some(Flash::success(format!("{} bağlantısı sağlam", host.name))),
        ),
        Some(host) => render_servers_partial(
            &state,
            Some(Flash::error(format!(
                "{} kullanılamıyor: {}",
                host.name,
                host.error.as_deref().unwrap_or("bilinmeyen neden")
            ))),
        ),
        None => render_servers_partial(&state, Some(Flash::error("sunucu bulunamadı"))),
    }
}

/// Taze veriyle sunucu partial'ı çizer.
fn render_servers_partial(state: &AppState, flash: Option<Flash>) -> Response {
    render(ServersPartial {
        db_ready: state.db.is_some(),
        servers: server_cards(state),
        local_hosts: local_cards(state),
        flash,
    })
}

/// Kayıt defterinden sunucu satırlarını kurar: bir sunucunun runtime
/// kayıtları (`prod1-docker`, `prod1-podman`) tek satırda birleştirilir.
fn server_cards(state: &AppState) -> Vec<ServerCard> {
    let mut cards: Vec<ServerCard> = Vec::new();

    for host in state.registry.hosts() {
        if host.source == HostSource::Local {
            continue;
        }

        let security = host.security.unwrap_or_default();

        if let Some(card) = cards.iter_mut().find(|card| card.id == host.server_id) {
            if !card.kinds.contains(host.kind.as_str()) {
                card.kinds.push_str(", ");
                card.kinds.push_str(host.kind.as_str());
            }
            card.available |= host.available;
            if card.error.is_none() {
                card.error = host.error.clone();
            }
            continue;
        }

        cards.push(ServerCard {
            id: host.server_id.clone(),
            name: server_display_name(&host),
            url: host.url.clone().unwrap_or_default(),
            source: match host.source {
                HostSource::Config => "config",
                _ => "database",
            },
            kinds: host.kind.as_str().to_owned(),
            tls: security.tls,
            mtls: security.mtls,
            token: security.token,
            available: host.available,
            error: host.error.clone(),
            removable: host.source == HostSource::Database,
        });
    }

    cards
}

/// Kayıt defterinden yerel runtime satırlarını kurar.
fn local_cards(state: &AppState) -> Vec<LocalCard> {
    state
        .registry
        .hosts()
        .iter()
        .filter(|host| host.source == HostSource::Local)
        .map(|host| LocalCard {
            name: host.name.clone(),
            available: host.available,
        })
        .collect()
}

/// Host adını sunucu satırı için kısaltır: "prod1 (docker)" → "prod1".
fn server_display_name(host: &HostEntry) -> String {
    host.name
        .strip_suffix(" (docker)")
        .or_else(|| host.name.strip_suffix(" (podman)"))
        .unwrap_or(&host.name)
        .to_owned()
}

/// Form alanını kırpıp boşsa `None` yapar.
fn trimmed(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Sunucu adından kısa kimlik üretir: "Prod Sunucu 1" → "prod-sunucu-1".
///
/// Türkçe karakterler ASCII karşılıklarına çevrilir; harf/rakam dışındaki
/// her şey tireye dönüşür.
fn slugify(name: &str) -> String {
    let slug: String = name
        .chars()
        .flat_map(|c| match c {
            // `İ`'nin küçük harfi birleştirme işareti getirir; elle eşle.
            'İ' => "i".chars().collect::<Vec<_>>(),
            c => c.to_lowercase().collect::<Vec<_>>(),
        })
        .map(|c| match c {
            'ı' => 'i',
            'ş' => 's',
            'ğ' => 'g',
            'ü' => 'u',
            'ö' => 'o',
            'ç' => 'c',
            c if c.is_ascii_alphanumeric() => c,
            _ => '-',
        })
        .collect();

    slug.split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Adres geri-döngü bir sunucuya mı işaret ediyor?
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_matches(|c| c == '[' || c == ']');

    host == "localhost" || host == "::1" || host == "127.0.0.1" || host.starts_with("127.")
}

/// Sunucu formunu doğrular; hata varsa kullanıcıya gösterilecek mesajı
/// döner.
///
/// Güvenlik kuralları: ağ üzerinden erişilen sunucular `https://` ve
/// token ister (agent tarafı da token'sız geri-döngü dışına bağlanmayı
/// reddeder). Yerel adreslerde düz HTTP ve token'sız bağlantıya izin
/// verilir (SSH tüneli senaryosu).
fn validate_server_form(name: &str, url: &str, token: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("sunucu adı gerekli".to_owned());
    }
    if name.chars().count() > 64 {
        return Err("sunucu adı en fazla 64 karakter olabilir".to_owned());
    }
    if slugify(name).is_empty() {
        return Err("sunucu adı harf ya da rakam içermeli".to_owned());
    }

    let parsed = url
        .parse::<axum::http::Uri>()
        .map_err(|_| format!("geçersiz adres: {url}"))?;

    let scheme = parsed.scheme_str().unwrap_or_default();
    let host = parsed.host().unwrap_or_default();

    if !matches!(scheme, "http" | "https") {
        return Err("adres http:// ya da https:// ile başlamalı".to_owned());
    }
    if host.is_empty() {
        return Err("adreste sunucu adı eksik (örn. https://10.0.0.5:7654)".to_owned());
    }

    if !is_loopback_host(host) {
        if scheme != "https" {
            return Err(
                "ağ üzerinden erişilen sunucular için https:// gerekli — agent tarafında \
                 [tls] yapılandırın (yerel adreslerde http:// kullanılabilir)"
                    .to_owned(),
            );
        }
        if token.is_empty() {
            return Err(
                "ağ üzerinden erişilen sunucular için token gerekli — agent yapılandırmasındaki \
                 taşıyıcı token'ı girin"
                    .to_owned(),
            );
        }
    }

    Ok(())
}

/// `GET /static/main.css` — derleme zamanında binary'ye gömülü stil sayfası.
///
/// Tek-binary hedefi gereği statik dosyalar diskten değil binaryden servis
/// edilir (askama şablonlarındaki yaklaşımın devamı).
pub async fn main_css() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../public/main.css"),
    )
}

/// Hostun container'larını (veya hatasını) yüküler.
async fn load_containers(host: &HostEntry) -> (Vec<Container>, Option<String>) {
    let Some(runtime) = &host.runtime else {
        return (Vec::new(), Some(host_unavailable_message(host)));
    };

    match runtime.list_containers().await {
        Ok(containers) => (containers, None),
        Err(err) => (Vec::new(), Some(err.to_string())),
    }
}

/// Hostun pod'larını (veya hatasını) yüküler.
async fn load_pods(host: &HostEntry) -> (Vec<Pod>, Option<String>) {
    // Docker'da pod kavramı yok; teknik hata yerine açıklayıcı mesaj.
    if host.kind == pdman_common::model::RuntimeKind::Docker {
        return (
            Vec::new(),
            Some("Pod'lar yalnızca Podman hostlarında kullanılabilir.".to_owned()),
        );
    }

    let Some(runtime) = &host.runtime else {
        return (Vec::new(), Some(host_unavailable_message(host)));
    };

    match runtime.list_pods().await {
        Ok(pods) => (pods, None),
        Err(err) => (Vec::new(), Some(err.to_string())),
    }
}

/// Hostun image'larını (veya hatasını) tablo görünümüyle yüküler.
async fn load_images(host: &HostEntry) -> (Vec<ImageView>, Option<String>) {
    let Some(runtime) = &host.runtime else {
        return (Vec::new(), Some(host_unavailable_message(host)));
    };

    match runtime.list_images().await {
        Ok(images) => (
            images
                .into_iter()
                .map(|image| ImageView {
                    tags: if image.repo_tags.is_empty() {
                        "<none>".to_owned()
                    } else {
                        image.repo_tags.join(", ")
                    },
                    id: short_id(&image.id),
                    size: format_bytes(image.size),
                    created: relative_time(image.created),
                })
                .collect(),
            None,
        ),
        Err(err) => (Vec::new(), Some(err.to_string())),
    }
}

/// Hostun volume'larını (veya hatasını) tablo görünümüyle yüküler.
async fn load_volumes(host: &HostEntry) -> (Vec<VolumeView>, Option<String>) {
    let Some(runtime) = &host.runtime else {
        return (Vec::new(), Some(host_unavailable_message(host)));
    };

    match runtime.list_volumes().await {
        Ok(volumes) => (
            volumes
                .into_iter()
                .map(|volume| VolumeView {
                    name: volume.name,
                    driver: volume.driver,
                    created_at: volume.created_at.unwrap_or_else(|| "-".to_owned()),
                })
                .collect(),
            None,
        ),
        Err(err) => (Vec::new(), Some(err.to_string())),
    }
}

/// Hostun network'lerini (veya hatasını) tablo görünümüyle yüküler.
async fn load_networks(host: &HostEntry) -> (Vec<NetworkView>, Option<String>) {
    let Some(runtime) = &host.runtime else {
        return (Vec::new(), Some(host_unavailable_message(host)));
    };

    match runtime.list_networks().await {
        Ok(networks) => (
            networks
                .into_iter()
                .map(|network| NetworkView {
                    id: short_id(&network.id),
                    name: network.name,
                    driver: network.driver,
                })
                .collect(),
            None,
        ),
        Err(err) => (Vec::new(), Some(err.to_string())),
    }
}

/// Host kullanılamıyorsa gösterilecek mesaj.
fn host_unavailable_message(host: &HostEntry) -> String {
    host.error
        .clone()
        .unwrap_or_else(|| "runtime kullanılamıyor".to_owned())
}

/// URL yolundaki eylem adını [`ContainerAction`]a çevirir.
fn parse_action(action: &str) -> Option<ContainerAction> {
    pdman_common::runtime::parse_container_action(action)
}

/// URL yolundaki eylem adını [`PodAction`]a çevirir.
fn parse_pod_action(action: &str) -> Option<PodAction> {
    pdman_common::runtime::parse_pod_action(action)
}

/// Image/network ID'sini kısaltır (`sha256:abc...` → `abc000000000`).
fn short_id(id: &str) -> String {
    id.trim_start_matches("sha256:").chars().take(12).collect()
}

/// Unix zaman damgasını kaba göreli metne çevirir ("3 gün önce" gibi).
fn relative_time(unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);

    let delta = now.saturating_sub(unix);

    if delta < 60 {
        format!("{delta} sn önce")
    } else if delta < 3600 {
        format!("{} dk önce", delta / 60)
    } else if delta < 86_400 {
        format!("{} sa önce", delta / 3600)
    } else if delta < 30 * 86_400 {
        format!("{} gün önce", delta / 86_400)
    } else {
        format!("{} ay önce", delta / (30 * 86_400))
    }
}

/// Bayt sayısını insan tarafından okunur biçime çevirir.
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

    let mut size = bytes as f64;
    let mut unit = 0;

    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// HTML'e yerleştirilecek metni kaçışlar (log satırları için).
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Sorgudan hostu çözer; `?host=` yoksa varsayılan hostu seçer.
#[allow(clippy::result_large_err)]
fn resolve_host(state: &AppState, query: &HostQuery) -> Result<Arc<HostEntry>, Response> {
    match &query.host {
        Some(id) => state.registry.get(id).ok_or_else(|| {
            (StatusCode::NOT_FOUND, format!("bilinmeyen host: {id}")).into_response()
        }),
        None => state.registry.default_host().ok_or_else(|| {
            service_unavailable(
                "hiç kayıtlı host yok — /servers sayfasından ya da pdman.toml'dan ekleyin",
            )
        }),
    }
}

/// Sayfa çerçevesini (kenar çubuğu + üst bilgi çubuğu) oluşturur.
///
/// Hostlar kenar çubuğunda runtime türüne göre tamamen ayrı gruplara
/// (Docker / Podman) paylaştırılır; seçili hostun türüne göre de marka
/// rengi sınıfları seçilir. Host olmayan sayfalarda (ör. sunucu yönetimi)
/// `None` verilir.
fn layout(
    state: &AppState,
    host: Option<&HostEntry>,
    path: &str,
    active: &str,
    title: &str,
) -> Layout {
    let mut docker_hosts = Vec::new();
    let mut podman_hosts = Vec::new();

    for entry in state.registry.hosts() {
        let nav = HostNav {
            id: entry.id.clone(),
            label: nav_label(&entry),
            available: entry.available,
        };
        if entry.kind == RuntimeKind::Podman {
            podman_hosts.push(nav);
        } else {
            docker_hosts.push(nav);
        }
    }

    let (host_id, host_name, host_kind, host_available, is_podman) = match host {
        Some(host) => (
            host.id.clone(),
            host.name.clone(),
            host.kind.as_str().to_owned(),
            host.available,
            host.kind == RuntimeKind::Podman,
        ),
        None => (
            String::new(),
            "host seçilmedi".to_owned(),
            "yok".to_owned(),
            false,
            false,
        ),
    };

    let (brand_badge_class, nav_active_class) = if is_podman {
        (
            "border-violet-400/30 bg-violet-400/10 text-violet-300",
            "bg-violet-400/10 text-violet-300",
        )
    } else {
        (
            "border-sky-400/30 bg-sky-400/10 text-sky-300",
            "bg-sky-400/10 text-sky-300",
        )
    };

    Layout {
        docker_hosts,
        podman_hosts,
        host_id,
        host_name,
        host_kind,
        host_available,
        is_podman,
        path: path.to_owned(),
        active: active.to_owned(),
        title: title.to_owned(),
        brand_badge_class,
        nav_active_class,
    }
}

/// Host adını kenar çubuğu için kısaltır: grup başlığı runtime'ı zaten
/// gösterdiğinden "Docker (yerel)" → "yerel", "prod1 (podman)" → "prod1".
fn nav_label(host: &HostEntry) -> String {
    let cap = match host.kind {
        RuntimeKind::Docker => "Docker",
        RuntimeKind::Podman => "Podman",
    };

    if let Some(inner) = host
        .name
        .strip_prefix(&format!("{cap} ("))
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return inner.to_owned();
    }

    host.name
        .strip_suffix(&format!(" ({})", host.kind.as_str()))
        .unwrap_or(&host.name)
        .to_owned()
}

/// Container listesinden özet kartı sayaçlarını çıkarır.
fn container_counts(containers: &[Container]) -> ContainerCounts {
    let running = containers
        .iter()
        .filter(|container| container.state == "running")
        .count();
    let paused = containers
        .iter()
        .filter(|container| container.state == "paused")
        .count();

    ContainerCounts {
        total: containers.len(),
        running,
        paused,
        stopped: containers.len() - running - paused,
    }
}

/// Bellek kullanım yüzdesi (0-100); sınır bilinmiyorsa 0.
fn mem_percent(usage: u64, limit: u64) -> u32 {
    if limit == 0 {
        0
    } else {
        ((usage as f64 / limit as f64) * 100.0).round() as u32
    }
}

fn render<T: Template>(template: T) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => internal_error(err),
    }
}

fn internal_error(err: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("dahili hata: {err}"),
    )
        .into_response()
}

fn service_unavailable(message: impl Into<String>) -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, message.into()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_actions() {
        assert_eq!(parse_action("start"), Some(ContainerAction::Start));
        assert_eq!(parse_action("stop"), Some(ContainerAction::Stop));
        assert_eq!(parse_action("restart"), Some(ContainerAction::Restart));
        assert_eq!(parse_action("pause"), Some(ContainerAction::Pause));
        assert_eq!(parse_action("unpause"), Some(ContainerAction::Unpause));
        assert_eq!(
            parse_action("remove"),
            Some(ContainerAction::Remove { force: false })
        );
        assert_eq!(parse_action("yok"), None);
    }

    #[test]
    fn formats_bytes_human_readable() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(format_bytes(3 * 1024_u64.pow(3)), "3.0 GiB");
    }

    #[test]
    fn escapes_html_characters() {
        assert_eq!(escape_html("a<b>&c"), "a&lt;b&gt;&amp;c");
        assert_eq!(escape_html("düz metin"), "düz metin");
    }

    #[test]
    fn parses_pod_actions() {
        assert_eq!(parse_pod_action("start"), Some(PodAction::Start));
        assert_eq!(parse_pod_action("stop"), Some(PodAction::Stop));
        assert_eq!(
            parse_pod_action("remove"),
            Some(PodAction::Remove { force: true })
        );
        assert_eq!(parse_pod_action("yok"), None);
    }

    #[test]
    fn shortens_ids() {
        assert_eq!(short_id("sha256:abcdef1234567890"), "abcdef123456");
        assert_eq!(short_id("abcdef1234567890"), "abcdef123456");
        assert_eq!(short_id("kısa"), "kısa");
    }

    #[test]
    fn relative_time_buckets() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        assert_eq!(relative_time(now - 30), "30 sn önce");
        assert_eq!(relative_time(now - 90), "1 dk önce");
        assert_eq!(relative_time(now - 3600 * 5), "5 sa önce");
        assert_eq!(relative_time(now - 86_400 * 3), "3 gün önce");
        assert_eq!(relative_time(now - 86_400 * 90), "3 ay önce");
    }

    #[test]
    fn container_counts_buckets_states() {
        let containers = vec![
            container("a", "running"),
            container("b", "running"),
            container("c", "paused"),
            container("d", "exited"),
        ];

        let counts = container_counts(&containers);
        assert_eq!(counts.total, 4);
        assert_eq!(counts.running, 2);
        assert_eq!(counts.paused, 1);
        assert_eq!(counts.stopped, 1);
    }

    #[test]
    fn mem_percent_handles_zero_limit() {
        assert_eq!(mem_percent(500, 0), 0);
        assert_eq!(mem_percent(0, 400), 0);
        assert_eq!(mem_percent(200, 400), 50);
        assert_eq!(mem_percent(400, 400), 100);
    }

    #[test]
    fn slugifies_names() {
        assert_eq!(slugify("prod1"), "prod1");
        assert_eq!(slugify("Prod Sunucu 1"), "prod-sunucu-1");
        assert_eq!(slugify("  İstanbul-Örnek 2 "), "istanbul-ornek-2");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("a--b"), "a-b");
    }

    #[test]
    fn validates_loopback_servers_loosely() {
        // Yerel adreslerde http ve token'sız bağlantıya izin verilir.
        assert!(validate_server_form("yerel", "http://127.0.0.1:7654", "").is_ok());
        assert!(validate_server_form("yerel", "http://localhost:7654", "").is_ok());
        assert!(validate_server_form("yerel", "http://[::1]:7654", "").is_ok());
    }

    #[test]
    fn validates_remote_servers_strictly() {
        // Ağ üzerinden erişilen sunucular https + token ister.
        assert!(validate_server_form("prod1", "http://10.0.0.5:7654", "token").is_err());
        assert!(validate_server_form("prod1", "https://10.0.0.5:7654", "").is_err());
        assert!(validate_server_form("prod1", "https://10.0.0.5:7654", "token").is_ok());
    }

    #[test]
    fn validates_form_basics() {
        assert!(validate_server_form("", "https://10.0.0.5:7654", "t").is_err());
        assert!(validate_server_form("ad", "ftp://10.0.0.5:7654", "t").is_err());
        assert!(validate_server_form("ad", "https://", "t").is_err());
        assert!(validate_server_form("!!!", "https://10.0.0.5:7654", "t").is_err());
        let long_name = "a".repeat(65);
        assert!(validate_server_form(&long_name, "https://10.0.0.5:7654", "t").is_err());
    }

    fn container(id: &str, state: &str) -> Container {
        Container {
            id: id.to_owned(),
            name: id.to_owned(),
            image: "nginx:latest".to_owned(),
            state: state.to_owned(),
            status: String::new(),
            created: 0,
        }
    }
}
