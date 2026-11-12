//! Panel ve agent arasında paylaşılan hata tipleri.

use std::io;

/// pdman bileşenlerinin ortak hata tipi.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Girdi/çıktı hatası (yapılandırma dosyası, socket vb.).
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// Serileştirme hatası.
    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Yapılandırma hatası.
    #[error("configuration: {0}")]
    Config(String),

    /// Container runtime (Docker/Podman) hatası.
    #[error("runtime: {0}")]
    Runtime(String),

    /// İstenen işlem bu runtime veya bu sürüm tarafından henüz desteklenmiyor.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl Error {
    /// Yeni bir yapılandırma hatası oluşturur.
    pub fn config(source: impl Into<String>) -> Self {
        Self::Config(source.into())
    }

    /// Yeni bir runtime hatası oluşturur.
    pub fn runtime(source: impl Into<String>) -> Self {
        Self::Runtime(source.into())
    }

    /// Desteklenmeyen işlem hatası oluşturur.
    pub fn unsupported(what: impl Into<String>) -> Self {
        Self::Unsupported(what.into())
    }
}

/// Ortak sonuç tipi.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(feature = "runtime")]
impl From<bollard::errors::Error> for Error {
    fn from(err: bollard::errors::Error) -> Self {
        Self::Runtime(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let err = Error::config("pdman.toml okunamadı");
        assert_eq!(err.to_string(), "configuration: pdman.toml okunamadı");

        let err = Error::unsupported("list_pods");
        assert_eq!(err.to_string(), "unsupported: list_pods");
    }

    #[test]
    fn io_error_converts() {
        let err = Error::from(io::Error::new(io::ErrorKind::NotFound, "yok"));
        assert!(err.to_string().contains("io:"));
    }
}
