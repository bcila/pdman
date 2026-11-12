//! `pdman-agent`: uzak sunuculardaki Docker/Podman'ı panele açan hafif aracı.
//!
//! Agent, panelin `Runtime` soyutlamasının HTTP yansımasını (`/api/v1`)
//! sunar. Tüm uçlar taşıyıcı token ile korunur; token yapılandırılmamışsa
//! yalnızca geri-döngü adreslerine bind edilmesine izin verilir (SSH tüneli
//! senaryosu).

mod api;
mod config;
mod tls;

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use clap::Parser;
use pdman_common::{
    model::RuntimeKind,
    runtime::{
        Runtime,
        docker::{Docker, DockerEndpoint},
        podman::Podman,
    },
};

use api::AgentState;
use config::Config;
use tls::AgentTlsConfig;

#[derive(Parser)]
#[command(name = "pdman-agent", version, about = "pdman uzak sunucu agentı")]
struct Cli {
    /// Yapılandırma dosyasının yolu.
    #[arg(short, long, default_value = "pdman-agent.toml")]
    config: PathBuf,
}

fn load_config(cli: &Cli) -> Config {
    // Varsayılan yol yoksa sessizce varsayılan ayarlarla devam et; kullanıcı
    // açıkça bir yol verdiyse hata bildir.
    if !cli.config.exists() && cli.config.as_os_str() == "pdman-agent.toml" {
        return Config::default();
    }

    Config::load(&cli.config).unwrap_or_else(|err| {
        eprintln!("pdman-agent: {err}");
        std::process::exit(1);
    })
}

/// Token'ı yapılandırmadan (satır içi ya da dosya) yükler.
fn load_token(config: &Config) -> Result<Option<String>, String> {
    if let Some(token) = &config.auth.token {
        return Ok(Some(token.trim().to_owned()));
    }

    if let Some(path) = &config.auth.token_file {
        let token = std::fs::read_to_string(path)
            .map_err(|err| format!("token dosyası {path} okunamadı: {err}"))?;
        return Ok(Some(token.trim().to_owned()));
    }

    Ok(None)
}

/// Bind adresi geri-döngü mü? (`127.x.x.x`, `::1`, `localhost`)
fn is_loopback_bind(bind: &str) -> bool {
    let host = bind.rsplit_once(':').map(|(host, _)| host).unwrap_or(bind);

    host == "localhost"
        || host == "::1"
        || host
            .strip_prefix("127.")
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// Yapılandırmadan kullanılabilir runtime'ları kurar (ping ile doğrular).
async fn build_runtimes(config: &Config) -> HashMap<RuntimeKind, Arc<dyn Runtime>> {
    let mut runtimes = HashMap::new();

    let docker = Docker::connect(DockerEndpoint(Docker::host_uri(&config.docker.socket)));
    match docker {
        Ok(docker) => {
            if docker.ping().await.is_ok() {
                println!("pdman-agent: docker hazır ({})", config.docker.socket);
                runtimes.insert(RuntimeKind::Docker, Arc::new(docker) as Arc<dyn Runtime>);
            } else {
                println!(
                    "pdman-agent: docker daemon'a ulaşılamadı ({})",
                    config.docker.socket
                );
            }
        }
        Err(err) => println!("pdman-agent: docker socket bulunamadı: {err}"),
    }

    let podman = Podman::connect(&config.podman.socket);
    match podman {
        Ok(podman) => {
            if podman.ping().await.is_ok() {
                println!("pdman-agent: podman hazır ({})", config.podman.socket);
                runtimes.insert(RuntimeKind::Podman, Arc::new(podman) as Arc<dyn Runtime>);
            } else {
                println!(
                    "pdman-agent: podman daemon'a ulaşılamadı ({})",
                    config.podman.socket
                );
            }
        }
        Err(err) => println!("pdman-agent: podman socket bulunamadı: {err}"),
    }

    runtimes
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let config = load_config(&cli);

    let token = load_token(&config).unwrap_or_else(|err| {
        eprintln!("pdman-agent: {err}");
        std::process::exit(1);
    });

    // Token yoksa yalnızca geri-döngü adreslerine izin ver.
    if token.is_none() && !is_loopback_bind(&config.server.bind) {
        eprintln!(
            "pdman-agent: {} adresine bind etmek için token gerekli \
             ([auth] token ya da token_file)",
            config.server.bind
        );
        std::process::exit(1);
    }

    let runtimes = build_runtimes(&config).await;

    let state = Arc::new(AgentState {
        name: config.host_name(),
        token,
        runtimes,
    });

    let app = api::router(state);

    let bind = config.server.bind.clone();
    let addr: std::net::SocketAddr = bind
        .parse()
        .unwrap_or_else(|err| panic!("pdman-agent: geçersiz bind adresi ({bind}): {err}"));

    if config.tls.cert_file.is_empty() {
        println!("pdman-agent: http://{bind}");

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .unwrap_or_else(|err| panic!("pdman-agent: {bind} adresi dinlenemedi: {err}"));

        axum::serve(listener, app).await.expect("server error");
    } else {
        let tls_config = AgentTlsConfig::load(&config.tls).unwrap_or_else(|err| {
            eprintln!("pdman-agent: TLS yapılandırması hatalı: {err}");
            std::process::exit(1);
        });

        if tls_config.mtls_enabled() {
            println!("pdman-agent: https://{bind} (mTLS: istemci sertifikası zorunlu)");
        } else {
            println!("pdman-agent: https://{bind}");
        }

        let tls = axum_server::tls_rustls::RustlsConfig::from_config(tls_config.into());

        axum_server::bind_rustls(addr, tls)
            .serve(app.into_make_service())
            .await
            .expect("server error");
    }
}
