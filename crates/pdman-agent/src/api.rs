//! Agent'ın HTTP API'si (`/api/v1`).
//!
//! Bu katman, panelin [`Runtime`] soyutlamasının HTTP yansımasını sunar:
//! panel tarafındaki `pdman::agent::AgentClient` aynı trait'i bu uçları
//! çağırarak implement eder. Tüm uçlar (sağlık dahil) taşıyıcı token ile
//! korunur; `?runtime=docker|podman` sorgu parametresi ile runtime seçilir.

// axum'un Response tipi büyük; hata tipini küçültmek değere değmiyor.
#![allow(clippy::result_large_err)]

use std::{collections::HashMap, convert::Infallible, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::StreamExt;
use serde::Deserialize;

use pdman_common::{
    model::{Container, ContainerInspect, Image, Network, Pod, RuntimeKind, Stats, Volume},
    protocol::{HostInfo, RuntimeStatus, api},
    runtime::{LogOpts, Runtime, parse_container_action, parse_pod_action},
};

/// Agent'ın paylaşılan durumu.
pub struct AgentState {
    /// Hostun panelde görünen adı.
    pub name: String,
    /// Yapılandırılmış taşıyıcı token; `None` ise kimlik doğrulama kapalı
    /// (yalnızca geri-döngü adreslerine bind edilirken izin verilir).
    pub token: Option<String>,
    /// Kullanılabilir runtime'lar.
    pub runtimes: HashMap<RuntimeKind, Arc<dyn Runtime>>,
}

impl AgentState {
    /// Sorguya göre runtime'ı seçer.
    fn runtime(&self, query: &RuntimeQuery) -> Result<Arc<dyn Runtime>, Response> {
        let kind = query.runtime.unwrap_or(RuntimeKind::Docker);
        self.runtimes.get(&kind).cloned().ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("{kind} runtime'ı bu hostta yok"),
            )
                .into_response()
        })
    }

    /// Sağlık yanıtı.
    fn host_info(&self) -> HostInfo {
        HostInfo {
            name: self.name.clone(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            runtimes: [RuntimeKind::Docker, RuntimeKind::Podman]
                .into_iter()
                .map(|kind| RuntimeStatus {
                    kind,
                    available: self.runtimes.contains_key(&kind),
                })
                .collect(),
        }
    }
}

/// Agent router'ı.
pub fn router(state: Arc<AgentState>) -> Router {
    Router::new()
        .route(&api::health(), get(health))
        .route(&api::containers(), get(list_containers))
        .route(&api::container("{id}"), get(inspect_container))
        .route(
            &api::container_action("{id}", "{action}"),
            post(container_action),
        )
        .route(&api::container_stats("{id}"), get(container_stats))
        .route(&api::container_logs("{id}"), get(container_logs))
        .route(&api::events(), get(events))
        .route(&api::pods(), get(list_pods))
        .route(&api::pod_action("{id}", "{action}"), post(pod_action))
        .route(&api::images(), get(list_images))
        .route(&api::image_remove("{id}"), post(image_remove))
        .route(&api::volumes(), get(list_volumes))
        .route(&api::volume_remove("{name}"), post(volume_remove))
        .route(&api::networks(), get(list_networks))
        .route(&api::network_remove("{id}"), post(network_remove))
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state)
}

/// Taşıyıcı token denetimi.
async fn auth(
    State(state): State<Arc<AgentState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(expected) = state.token.as_ref() else {
        return next.run(request).await;
    };

    let provided = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    if provided != format!("Bearer {expected}") {
        return (StatusCode::UNAUTHORIZED, "geçersiz ya da eksik token").into_response();
    }

    next.run(request).await
}

/// `?runtime=` sorgu parametresi.
#[derive(Debug, Deserialize)]
struct RuntimeQuery {
    runtime: Option<RuntimeKind>,
}

/// `?tail=&follow=` sorgu parametreleri.
#[derive(Debug, Deserialize)]
struct LogsQuery {
    tail: Option<u64>,
    follow: Option<bool>,
}

async fn health(State(state): State<Arc<AgentState>>) -> Json<HostInfo> {
    Json(state.host_info())
}

async fn list_containers(
    State(state): State<Arc<AgentState>>,
    Query(query): Query<RuntimeQuery>,
) -> Result<Json<Vec<Container>>, Response> {
    let runtime = state.runtime(&query)?;
    runtime
        .list_containers()
        .await
        .map(Json)
        .map_err(agent_error)
}

async fn inspect_container(
    State(state): State<Arc<AgentState>>,
    Path(id): Path<String>,
    Query(query): Query<RuntimeQuery>,
) -> Result<Json<ContainerInspect>, Response> {
    let runtime = state.runtime(&query)?;
    runtime
        .inspect_container(&id)
        .await
        .map(Json)
        .map_err(agent_error)
}

async fn container_action(
    State(state): State<Arc<AgentState>>,
    Path((id, action)): Path<(String, String)>,
    Query(query): Query<RuntimeQuery>,
) -> Result<StatusCode, Response> {
    let runtime = state.runtime(&query)?;

    let Some(action) = parse_container_action(&action) else {
        return Err((StatusCode::NOT_FOUND, format!("bilinmeyen eylem: {action}")).into_response());
    };

    runtime
        .container_action(&id, action)
        .await
        .map(|_| StatusCode::OK)
        .map_err(agent_error)
}

async fn container_stats(
    State(state): State<Arc<AgentState>>,
    Path(id): Path<String>,
    Query(query): Query<RuntimeQuery>,
) -> Result<Json<Stats>, Response> {
    let runtime = state.runtime(&query)?;
    runtime
        .container_stats(&id)
        .await
        .map(Json)
        .map_err(agent_error)
}

/// `GET /containers/{id}/logs` — log akışı (SSE, `log-line` olayları).
async fn container_logs(
    State(state): State<Arc<AgentState>>,
    Path(id): Path<String>,
    Query(query): Query<RuntimeQuery>,
    Query(logs): Query<LogsQuery>,
) -> Response {
    let Ok(runtime) = state.runtime(&query) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "runtime kullanılamıyor").into_response();
    };

    let opts = LogOpts {
        follow: logs.follow.unwrap_or(true),
        tail: logs.tail,
        timestamps: true,
    };

    let stream = match runtime.logs(&id, opts).await {
        Ok(stream) => stream,
        Err(err) => return agent_error(err),
    };

    let stream = stream.filter_map(|item| async move {
        match item {
            // Panel kaçışlamayı tarayıcıya çıkarken kendisi yapar; ham gönder.
            Ok(line) => Some(Ok::<_, Infallible>(
                SseEvent::default().event("log-line").data(line),
            )),
            Err(err) => {
                eprintln!("pdman-agent: log akışı koptu: {err}");
                None
            }
        }
    });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// `GET /events` — runtime olay akışı (SSE, `runtime-event` olayları).
async fn events(
    State(state): State<Arc<AgentState>>,
    Query(query): Query<RuntimeQuery>,
) -> Response {
    let Ok(runtime) = state.runtime(&query) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "runtime kullanılamıyor").into_response();
    };

    let stream = match runtime.events().await {
        Ok(stream) => stream,
        Err(err) => return agent_error(err),
    };

    let stream = stream.filter_map(|item| async move {
        match item {
            Ok(event) => {
                let data = serde_json::to_string(&event).unwrap_or_default();
                Some(Ok::<_, Infallible>(
                    SseEvent::default().event("runtime-event").data(data),
                ))
            }
            Err(err) => {
                eprintln!("pdman-agent: olay akışı koptu: {err}");
                None
            }
        }
    });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn list_pods(
    State(state): State<Arc<AgentState>>,
    Query(query): Query<RuntimeQuery>,
) -> Result<Json<Vec<Pod>>, Response> {
    let runtime = state.runtime(&query)?;
    runtime.list_pods().await.map(Json).map_err(agent_error)
}

async fn pod_action(
    State(state): State<Arc<AgentState>>,
    Path((id, action)): Path<(String, String)>,
    Query(query): Query<RuntimeQuery>,
) -> Result<StatusCode, Response> {
    let runtime = state.runtime(&query)?;

    let Some(action) = parse_pod_action(&action) else {
        return Err((StatusCode::NOT_FOUND, format!("bilinmeyen eylem: {action}")).into_response());
    };

    runtime
        .pod_action(&id, action)
        .await
        .map(|_| StatusCode::OK)
        .map_err(agent_error)
}

async fn list_images(
    State(state): State<Arc<AgentState>>,
    Query(query): Query<RuntimeQuery>,
) -> Result<Json<Vec<Image>>, Response> {
    let runtime = state.runtime(&query)?;
    runtime.list_images().await.map(Json).map_err(agent_error)
}

async fn image_remove(
    State(state): State<Arc<AgentState>>,
    Path(id): Path<String>,
    Query(query): Query<RuntimeQuery>,
) -> Result<StatusCode, Response> {
    let runtime = state.runtime(&query)?;
    runtime
        .image_remove(&id, false)
        .await
        .map(|_| StatusCode::OK)
        .map_err(agent_error)
}

async fn list_volumes(
    State(state): State<Arc<AgentState>>,
    Query(query): Query<RuntimeQuery>,
) -> Result<Json<Vec<Volume>>, Response> {
    let runtime = state.runtime(&query)?;
    runtime.list_volumes().await.map(Json).map_err(agent_error)
}

async fn volume_remove(
    State(state): State<Arc<AgentState>>,
    Path(name): Path<String>,
    Query(query): Query<RuntimeQuery>,
) -> Result<StatusCode, Response> {
    let runtime = state.runtime(&query)?;
    runtime
        .volume_remove(&name)
        .await
        .map(|_| StatusCode::OK)
        .map_err(agent_error)
}

async fn list_networks(
    State(state): State<Arc<AgentState>>,
    Query(query): Query<RuntimeQuery>,
) -> Result<Json<Vec<Network>>, Response> {
    let runtime = state.runtime(&query)?;
    runtime.list_networks().await.map(Json).map_err(agent_error)
}

async fn network_remove(
    State(state): State<Arc<AgentState>>,
    Path(id): Path<String>,
    Query(query): Query<RuntimeQuery>,
) -> Result<StatusCode, Response> {
    let runtime = state.runtime(&query)?;
    runtime
        .network_remove(&id)
        .await
        .map(|_| StatusCode::OK)
        .map_err(agent_error)
}

/// Runtime hatasını HTTP yanıtına çevirir.
fn agent_error(err: pdman_common::Error) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
}
