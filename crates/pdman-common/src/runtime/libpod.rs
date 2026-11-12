//! Podman'ın kendi REST API'sine (libpod) ince istemci.
//!
//! Pod'lar gibi Docker API'de karşılığı olmayan işlemler için kullanılır.
//! Container/image/volume/network işlemleri docker-compat API üzerinden
//! bollard ile yapılır (bkz. [`super::podman`]).
//!
//! İstemci yalnızca unix domain socket üzerinden çalışır (TCP podman
//! sunucuları için pod desteği henüz yok).

use http_body_util::{BodyExt, Empty};
use hyper::{Method, Request, Response, body::Incoming};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use hyperlocal::UnixConnector;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};
/// libpod API sürümü öneki.
///
/// `v4.0.0` seçildi: Podman 4.x ve 5.x sunucularının ikisi de bu sürümdeki
/// istekleri kabul eder (sunucu yalnızca kendi sürümünden yeni istekleri
/// reddeder).
const API_VERSION: &str = "v4.0.0";

/// libpod istemcisi (unix socket üzerinden).
pub struct LibpodClient {
    client: Client<UnixConnector, Empty<bytes::Bytes>>,
    socket: String,
}

impl LibpodClient {
    /// Verilen socket yoluna (düz yol, `unix://` öneksiz) istemci kurar.
    ///
    /// Bağlantı tembeldir; socket'in o anda erişilebilir olması gerekmez.
    pub fn new(socket: &str) -> Self {
        Self {
            client: Client::builder(TokioExecutor::new()).build(UnixConnector),
            socket: socket.to_owned(),
        }
    }

    /// libpod yolunu (`pods/json` gibi) tam URI'ye çevirir.
    fn uri(&self, path: &str) -> hyper::Uri {
        hyperlocal::Uri::new(&self.socket, &format!("/{API_VERSION}/libpod/{path}")).into()
    }

    /// `GET` ile JSON yanıtı çeker.
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self.request(Method::GET, path).await?;
        let body = self.decode_body(response).await?;

        serde_json::from_slice(&body)
            .map_err(|err| Error::runtime(format!("libpod yanıtı ayrıştırılamadı: {err}")))
    }

    /// Gövdesiz `POST` gönderir (pod start/stop).
    pub async fn post(&self, path: &str) -> Result<()> {
        let response = self.request(Method::POST, path).await?;
        self.check_status(response).await
    }

    /// `DELETE` gönderir (pod remove; force sorgu parametresiyle).
    pub async fn delete(&self, path: &str) -> Result<()> {
        let response = self.request(Method::DELETE, path).await?;
        self.check_status(response).await
    }

    async fn request(&self, method: Method, path: &str) -> Result<Response<Incoming>> {
        let request = Request::builder()
            .method(method)
            .uri(self.uri(path))
            .body(Empty::<bytes::Bytes>::new())
            .map_err(|err| Error::runtime(format!("libpod isteği kurulamadı: {err}")))?;

        self.client
            .request(request)
            .await
            .map_err(|err| Error::runtime(format!("libpod isteği başarısız: {err}")))
    }

    /// Yanıt gövdesini okur; başarısız durum kodunda hata üretir.
    async fn decode_body(&self, response: Response<Incoming>) -> Result<Vec<u8>> {
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|err| Error::runtime(format!("libpod yanıtı okunamadı: {err}")))?
            .to_bytes();

        if !status.is_success() {
            return Err(libpod_error(status, &body));
        }

        Ok(body.to_vec())
    }

    /// Yanıtı denetler; başarısız durum kodunda hata üretir.
    async fn check_status(&self, response: Response<Incoming>) -> Result<()> {
        let status = response.status();

        if status.is_success() {
            return Ok(());
        }

        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|err| Error::runtime(format!("libpod yanıtı okunamadı: {err}")))?
            .to_bytes();

        Err(libpod_error(status, &body))
    }
}

/// Podman'ın hata gövdesini (`{"cause": ..., "message": ...}`) okunur hata
/// üretir.
fn libpod_error(status: http::StatusCode, body: &[u8]) -> Error {
    let text = String::from_utf8_lossy(body);

    let message = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .or_else(|| value.get("cause"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| text.trim().to_owned());

    Error::runtime(format!("libpod {status}: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_extracts_message() {
        let body =
            r#"{"cause":"pod bulunamadı","message":"no such pod","response":404}"#.as_bytes();
        let err = libpod_error(http::StatusCode::NOT_FOUND, body);
        assert_eq!(
            err.to_string(),
            "runtime: libpod 404 Not Found: no such pod"
        );
    }

    #[test]
    fn error_falls_back_to_body() {
        let err = libpod_error(http::StatusCode::CONFLICT, "düz metin hata".as_bytes());
        assert!(err.to_string().contains("düz metin hata"));
    }
}
