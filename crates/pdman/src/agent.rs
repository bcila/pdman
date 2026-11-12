//! Uzak `pdman-agent` istemcisi.
//!
//! [`Runtime`] trait'inin HTTP üzerinden implementasyonu: panel, uzak
//! hostları yerel runtime'larla aynı arayüzden yönetir; UI ve olay iletme
//! mekanizması değişmez.

use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, Empty};
use hyper::{Method, Request, Response, Uri, body::Incoming};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use serde::de::DeserializeOwned;

use pdman_common::{
    error::{Error, Result},
    model::{Container, ContainerInspect, Event, Image, Network, Pod, RuntimeKind, Stats, Volume},
    protocol::{self, HostInfo},
    runtime::{ContainerAction, EventStream, LogOpts, LogStream, PodAction, Runtime},
};

/// Sağlık denetimi için istek zaman aşımı.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

/// Bir agent hostuna bağlanırken kullanılan TLS ayarları.
#[derive(Debug, Clone, Default)]
pub struct AgentTls {
    /// Agent'ın sertifikasını imzalayan CA dosyası (PEM). Verilmezse yerleşik
    /// kök sertifikalar (webpki-roots) kullanılır.
    pub ca_file: Option<String>,
    /// İstemci sertifikası (PEM); mTLS gerektiren agentlar için.
    pub cert_file: Option<String>,
    /// İstemci özel anahtarı (PEM).
    pub key_file: Option<String>,
}

impl AgentTls {
    /// rustls istemci yapılandırmasını kurar.
    fn rustls_config(&self) -> Result<rustls::ClientConfig> {
        // Kök sertifikalar: özel CA ya da yerleşik.
        let roots = match &self.ca_file {
            Some(ca_file) => {
                let mut roots = rustls::RootCertStore::empty();

                for cert in load_certs(ca_file)? {
                    roots.add(cert).map_err(|err| {
                        Error::config(format!("CA sertifikası eklenemedi: {err}"))
                    })?;
                }

                roots
            }
            None => rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            },
        };

        let builder = rustls::ClientConfig::builder().with_root_certificates(roots);

        // İstemci kimliği (mTLS).
        match (&self.cert_file, &self.key_file) {
            (Some(cert_file), Some(key_file)) => builder
                .with_client_auth_cert(load_certs(cert_file)?, load_key(key_file)?)
                .map_err(|err| Error::config(format!("istemci sertifikası yüklenemedi: {err}"))),
            (None, None) => Ok(builder.with_no_client_auth()),
            _ => Err(Error::config(
                "cert_file ve key_file birlikte verilmelidir (mTLS)",
            )),
        }
    }
}

/// PEM dosyasından sertifika zincirini yükler.
fn load_certs(path: &str) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let file = std::fs::File::open(path)
        .map_err(|err| Error::config(format!("{path} açılamadı: {err}")))?;

    rustls_pemfile::certs(&mut std::io::BufReader::new(file))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| Error::config(format!("{path} içindeki sertifikalar okunamadı: {err}")))
}

/// PEM dosyasından özel anahtarı yükler.
fn load_key(path: &str) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)
        .map_err(|err| Error::config(format!("{path} açılamadı: {err}")))?;

    rustls_pemfile::private_key(&mut std::io::BufReader::new(file))
        .map_err(|err| Error::config(format!("{path} okunamadı: {err}")))?
        .ok_or_else(|| Error::config(format!("{path} içinde özel anahtar bulunamadı")))
}

/// Agent'tan gelen tek SSE iletisi.
#[derive(Debug, PartialEq, Eq)]
struct SseMessage {
    event: String,
    data: String,
}

/// SSE akışını parça parça çözümleyen durum makinesi.
///
/// Yorum satırları (`:`, örn. keep-alive ping'leri) yok sayılır; `data:`
/// satırları `\n` ile birleştirilir.
#[derive(Default)]
struct SseParser {
    buffer: String,
    event: Option<String>,
    data_lines: Vec<String>,
}

impl SseParser {
    /// Yeni bir parça besler ve tamamlanan iletileri döner.
    fn feed(&mut self, chunk: &str) -> Vec<SseMessage> {
        let mut messages = Vec::new();
        self.buffer.push_str(chunk);

        while let Some(pos) = self.buffer.find("\n\n") {
            let block: String = self.buffer.drain(..pos + 2).collect();

            for line in block.lines() {
                if line.starts_with(':') {
                    continue;
                }

                if let Some(value) = line.strip_prefix("event:") {
                    self.event = Some(value.trim_start().to_owned());
                } else if let Some(value) = line.strip_prefix("data:") {
                    self.data_lines
                        .push(value.strip_prefix(' ').unwrap_or(value).to_owned());
                }
            }

            if !self.data_lines.is_empty() {
                messages.push(SseMessage {
                    event: self.event.take().unwrap_or_default(),
                    data: self.data_lines.join("\n"),
                });
                self.data_lines.clear();
            } else {
                self.event = None;
            }
        }

        messages
    }
}

/// Belirli bir agent hostunun tek bir runtime'ına bağlı istemci.
pub struct AgentClient {
    client: Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Empty<Bytes>,
    >,
    /// Agent'ın temel adresi (`http://10.0.0.5:7654` ya da `https://...`).
    base: String,
    /// Taşıyıcı token; boşsa `Authorization` başlığı gönderilmez.
    token: String,
    /// Bu istemcinin yönettiği runtime.
    runtime: RuntimeKind,
}

/// Agent istemcisinin HTTP konnektör tipi (http + https).
type AgentConnector =
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>;

/// HTTP ve HTTPS'i destekleyen paylaşılan istemci kurar.
fn build_http_client(tls: &AgentTls) -> Result<Client<AgentConnector, Empty<Bytes>>> {
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls.rustls_config()?)
        .https_or_http()
        .enable_http1()
        .build();

    Ok(Client::builder(TokioExecutor::new()).build(connector))
}

impl AgentClient {
    /// Yeni istemci kurar.
    pub fn new(base: &str, token: String, runtime: RuntimeKind, tls: AgentTls) -> Result<Self> {
        let base = base.trim_end_matches('/').to_owned();

        Ok(Self {
            client: build_http_client(&tls)?,
            base,
            token,
            runtime,
        })
    }

    /// Agent'ın sağlık bilgisini alır (registry kurulumunda kullanılır).
    pub async fn health(base: &str, token: &str, tls: AgentTls) -> Result<HostInfo> {
        let client = build_http_client(&tls)?;
        let url = format!("{}{}", base.trim_end_matches('/'), protocol::api::health());

        let request = build_request(Method::GET, &url, token)?;

        let response = tokio::time::timeout(HEALTH_TIMEOUT, client.request(request))
            .await
            .map_err(|_| Error::runtime("agent zaman aşımına uğradı"))?
            .map_err(|err| Error::runtime(format!("agent'e ulaşılamadı: {err}")))?;

        decode_json(response).await
    }

    /// İstek URL'sini kurar (runtime sorgu parametresi dahil).
    fn url(&self, path: &str, extra: &[(&str, String)]) -> String {
        let mut query = format!("runtime={}", self.runtime.as_str());

        for (key, value) in extra {
            query.push_str(&format!("&{key}={value}"));
        }

        format!("{}{path}?{query}", self.base)
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        extra: &[(&str, String)],
    ) -> Result<Response<Incoming>> {
        let url = self.url(path, extra);
        let request = build_request(method, &url, &self.token)?;

        self.client
            .request(request)
            .await
            .map_err(|err| Error::runtime(format!("agent isteği başarısız: {err}")))
    }

    /// `GET` ile JSON yanıtı çeker.
    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self.request(Method::GET, path, &[]).await?;
        decode_json(response).await
    }

    /// Gövdesiz `POST` gönderir ve durum kodunu denetler.
    async fn post(&self, path: &str) -> Result<()> {
        let response = self.request(Method::POST, path, &[]).await?;
        decode_status(response).await
    }

    /// Agent'ın bir SSE ucuna abone olur.
    async fn sse(
        &self,
        path: &str,
        extra: &[(&str, String)],
    ) -> Result<futures_util::stream::BoxStream<'static, Result<SseMessage>>> {
        let response = self.request(Method::GET, path, extra).await?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::runtime(format!(
                "agent SSE ucu {status} döndü: {}",
                read_body(response).await?
            )));
        }

        let stream = response
            .into_body()
            .into_data_stream()
            .map(|chunk| {
                chunk
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .map_err(|err| Error::runtime(format!("agent akışı koptu: {err}")))
            })
            .scan(SseParser::default(), |parser, chunk| {
                let messages = match chunk {
                    Ok(text) => parser
                        .feed(&text)
                        .into_iter()
                        .map(Ok::<SseMessage, Error>)
                        .collect::<Vec<_>>(),
                    Err(err) => vec![Err(err)],
                };

                async move { Some(messages) }
            })
            .flat_map(futures_util::stream::iter);

        Ok(Box::pin(stream))
    }
}

/// Kimlik doğrulama başlığıyla istek kurar.
fn build_request(method: Method, url: &str, token: &str) -> Result<Request<Empty<Bytes>>> {
    let mut builder = Request::builder().method(method).uri(
        url.parse::<Uri>()
            .map_err(|err| Error::config(format!("geçersiz agent adresi ({url}): {err}")))?,
    );

    if !token.is_empty() {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }

    builder
        .body(Empty::new())
        .map_err(|err| Error::runtime(format!("istek kurulamadı: {err}")))
}

/// Yanıt gövdesini bayt olarak okur.
async fn read_body(response: Response<Incoming>) -> Result<String> {
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|err| Error::runtime(format!("agent yanıtı okunamadı: {err}")))?
        .to_bytes();

    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Yanıtı JSON'a çözümler; başarısız durum kodunda hata üretir.
async fn decode_json<T: DeserializeOwned>(response: Response<Incoming>) -> Result<T> {
    let status = response.status();

    if !status.is_success() {
        return Err(Error::runtime(format!(
            "agent {status} döndü: {}",
            read_body(response).await?
        )));
    }

    let body = read_body(response).await?;

    serde_json::from_str(&body)
        .map_err(|err| Error::runtime(format!("agent yanıtı ayrıştırılamadı: {err}")))
}

/// Yanıtın durum kodunu denetler.
async fn decode_status(response: Response<Incoming>) -> Result<()> {
    let status = response.status();

    if status.is_success() {
        return Ok(());
    }

    Err(Error::runtime(format!(
        "agent {status} döndü: {}",
        read_body(response).await?
    )))
}

#[async_trait::async_trait]
impl Runtime for AgentClient {
    fn kind(&self) -> RuntimeKind {
        self.runtime
    }

    async fn ping(&self) -> Result<()> {
        self.get_json::<HostInfo>(&protocol::api::health())
            .await
            .map(|_| ())
    }

    async fn list_containers(&self) -> Result<Vec<Container>> {
        self.get_json(&protocol::api::containers()).await
    }

    async fn container_action(&self, id: &str, action: ContainerAction) -> Result<()> {
        self.post(&protocol::api::container_action(id, action.path_name()))
            .await
    }

    async fn inspect_container(&self, id: &str) -> Result<ContainerInspect> {
        self.get_json(&protocol::api::container(id)).await
    }

    async fn container_stats(&self, id: &str) -> Result<Stats> {
        self.get_json(&protocol::api::container_stats(id)).await
    }

    async fn logs(&self, id: &str, opts: LogOpts) -> Result<LogStream> {
        let mut extra: Vec<(&str, String)> = Vec::new();

        if let Some(tail) = opts.tail {
            extra.push(("tail", tail.to_string()));
        }

        extra.push(("follow", opts.follow.to_string()));

        let stream = self.sse(&protocol::api::container_logs(id), &extra).await?;

        Ok(Box::pin(stream.filter_map(|item| async move {
            match item {
                Ok(message) if message.event == "log-line" => Some(Ok(message.data)),
                Ok(_) => None,
                Err(err) => Some(Err(err)),
            }
        })))
    }

    async fn events(&self) -> Result<EventStream> {
        let stream = self.sse(&protocol::api::events(), &[]).await?;

        Ok(Box::pin(stream.filter_map(|item| async move {
            match item {
                Ok(message) => serde_json::from_str::<Event>(&message.data).ok().map(Ok),
                Err(err) => Some(Err(err)),
            }
        })))
    }

    async fn list_pods(&self) -> Result<Vec<Pod>> {
        self.get_json(&protocol::api::pods()).await
    }

    async fn pod_action(&self, id: &str, action: PodAction) -> Result<()> {
        self.post(&protocol::api::pod_action(id, action.path_name()))
            .await
    }

    async fn list_images(&self) -> Result<Vec<Image>> {
        self.get_json(&protocol::api::images()).await
    }

    async fn image_remove(&self, id: &str, _force: bool) -> Result<()> {
        self.post(&protocol::api::image_remove(id)).await
    }

    async fn list_volumes(&self) -> Result<Vec<Volume>> {
        self.get_json(&protocol::api::volumes()).await
    }

    async fn volume_remove(&self, name: &str) -> Result<()> {
        self.post(&protocol::api::volume_remove(name)).await
    }

    async fn list_networks(&self) -> Result<Vec<Network>> {
        self.get_json(&protocol::api::networks()).await
    }

    async fn network_remove(&self, id: &str) -> Result<()> {
        self.post(&protocol::api::network_remove(id)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_handles_chunked_messages() {
        let mut parser = SseParser::default();

        // Parça sınırı iletileri bölebilir.
        let messages = parser.feed("event: log-line\nda");
        assert!(messages.is_empty());

        let messages = parser.feed("ta: satır 1\ndata: satır 2\n\n");
        assert_eq!(
            messages,
            vec![SseMessage {
                event: "log-line".to_owned(),
                data: "satır 1\nsatır 2".to_owned(),
            }]
        );
    }

    #[test]
    fn sse_parser_skips_comments_and_defaults_event() {
        let mut parser = SseParser::default();

        let messages = parser.feed(": keep-alive ping\n\ndata: {\"a\":1}\n\n");
        assert_eq!(
            messages,
            vec![SseMessage {
                event: String::new(),
                data: "{\"a\":1}".to_owned(),
            }]
        );
    }

    #[test]
    fn sse_parser_multiple_messages_in_one_chunk() {
        let mut parser = SseParser::default();

        let messages = parser.feed("event: a\ndata: 1\n\nevent: b\ndata: 2\n\n");

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].event, "a");
        assert_eq!(messages[1].event, "b");
        assert_eq!(messages[1].data, "2");
    }

    #[test]
    fn request_url_contains_runtime() {
        let client = AgentClient::new(
            "http://10.0.0.5:7654/",
            "token".to_owned(),
            RuntimeKind::Podman,
            AgentTls::default(),
        )
        .expect("istemci kurulamadı");

        let url = client.url(&protocol::api::containers(), &[]);
        assert_eq!(url, "http://10.0.0.5:7654/api/v1/containers?runtime=podman");

        let url = client.url(
            &protocol::api::container_logs("abc"),
            &[("tail", "100".to_owned())],
        );
        assert_eq!(
            url,
            "http://10.0.0.5:7654/api/v1/containers/abc/logs?runtime=podman&tail=100"
        );
    }
}
