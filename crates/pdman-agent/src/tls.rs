//! Agent'ın TLS sunucu yapılandırması.
//!
//! `[tls]` bölümü verildiğinde agent TLS ile dinler. `ca_file` verilirse
//! karşılıklı TLS (mTLS) etkin olur: yalnızca bu CA tarafından imzalanan
//! istemci sertifikasına sahip bağlantılar kabul edilir.

use std::{fs::File, io::BufReader, path::Path, sync::Arc};

use pdman_common::{Error, Result};

use crate::config::TlsConfig;

/// Yüklenmiş TLS sunucu yapılandırması.
pub struct AgentTlsConfig {
    /// rustls sunucu yapılandırması.
    config: rustls::ServerConfig,
    /// mTLS (istemci sertifikası doğrulaması) etkin mi?
    mtls: bool,
}

impl AgentTlsConfig {
    /// Sertifika ve anahtar dosyalarından yapılandırmayı kurar.
    pub fn load(cfg: &TlsConfig) -> Result<Self> {
        let certs = load_certs(&cfg.cert_file)?;
        let key = load_key(&cfg.key_file)?;

        let builder = rustls::ServerConfig::builder();

        // ca_file verilirse istemci sertifikası doğrulaması (mTLS) kurulur.
        let (configurator, mtls) = match &cfg.ca_file {
            Some(ca_file) => {
                let mut roots = rustls::RootCertStore::empty();

                for cert in load_certs(ca_file)? {
                    roots.add(cert).map_err(|err| {
                        Error::config(format!("CA sertifikası eklenemedi: {err}"))
                    })?;
                }

                let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                    .build()
                    .map_err(|err| Error::config(format!("mTLS doğrulayıcı kurulamadı: {err}")))?;

                (builder.with_client_cert_verifier(verifier), true)
            }
            None => (builder.with_no_client_auth(), false),
        };

        let config = configurator
            .with_single_cert(certs, key)
            .map_err(|err| Error::config(format!("sunucu sertifikası yüklenemedi: {err}")))?;

        Ok(Self { config, mtls })
    }

    /// mTLS (istemci sertifikası zorunlu) etkin mi?
    pub fn mtls_enabled(&self) -> bool {
        self.mtls
    }
}

impl From<AgentTlsConfig> for Arc<rustls::ServerConfig> {
    fn from(config: AgentTlsConfig) -> Self {
        Arc::new(config.config)
    }
}

/// PEM dosyasından sertifika zincirini yükler.
fn load_certs(path: impl AsRef<Path>) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let file = File::open(path.as_ref())
        .map_err(|err| Error::config(format!("{} açılamadı: {err}", path.as_ref().display())))?;

    let mut reader = BufReader::new(file);

    rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| {
            Error::config(format!(
                "{} içindeki sertifikalar okunamadı: {err}",
                path.as_ref().display()
            ))
        })
}

/// PEM dosyasından özel anahtarı yükler.
fn load_key(path: impl AsRef<Path>) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let file = File::open(path.as_ref())
        .map_err(|err| Error::config(format!("{} açılamadı: {err}", path.as_ref().display())))?;

    let mut reader = BufReader::new(file);

    rustls_pemfile::private_key(&mut reader)
        .map_err(|err| Error::config(format!("{} okunamadı: {err}", path.as_ref().display())))?
        .ok_or_else(|| {
            Error::config(format!(
                "{} içinde özel anahtar bulunamadı",
                path.as_ref().display()
            ))
        })
}
