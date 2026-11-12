//! `pdman-common`, panel (`pdman`) ve agent (`pdman-agent`) arasında paylaşılan
//! veri modellerini, hata tiplerini ve Docker/Podman runtime soyutlamasını içerir.
//!
//! Bu crate **yaprak**tır: ne panele ne agent'a bağımlıdır. Bağımlılık yönü:
//!
//! ```text
//! pdman (panel) ──► pdman-common ◄── pdman-agent
//! ```
//!
//! `runtime` feature'ı (varsayılan) bollard tabanlı istemcileri etkinleştirir;
//! yalnızca tipleri gereken taraf `default-features = false` kullanabilir.

pub mod error;
pub mod model;
pub mod protocol;

#[cfg(feature = "runtime")]
pub mod runtime;

pub use error::{Error, Result};
