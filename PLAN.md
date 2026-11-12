# pdman — Plan

**pdman**, yerel ve uzak sunuculardaki Docker ve Podman container/pod'larını tek bir
minimal panelden yönetmek için tasarlanmış bir platformdur.

- **`pdman`** — Panel. Web arayüzü (axum + askama + htmx + tailwind). Yerel runtime'lara
  doğrudan socket üzerinden, uzak sunuculara `pdman-agent` üzerinden bağlanır.
- **`pdman-agent`** — Uzak sunucularda çalışan, panelin konuştuğu hafif aracı. Bulunduğu
  sunucudaki Docker ve Podman'ı yönetir.
- **`pdman-common`** — Panel ve agent arasında paylaşılan tipler (veri modelleri + protokol).

---

## 1. Mimari

```
┌──────────────────────────┐
│          pdman           │  ← tarayıcı → panel (htmx)
│  axum + askama + htmx    │
└─────────┬────────┬───────┘
          │        │
          │        └──────────── https + token ────────────┐
          │                                                  ▼
          │ unix socket                        ┌──────────────────────┐
          ▼                                    │      pdman-agent     │
┌─────────────────────┐   unix socket          │  (uzak sunucu)       │
│   yerel sunucu      │◄───────────────────────└──────────┬───────────┘
│  docker.sock        │                                   │ unix socket
│  podman.sock        │                        ┌──────────▼───────────┐
└─────────────────────┘                        │  uzak docker.sock    │
                                               │  uzak podman.sock    │
                                               └──────────────────────┘
```

Temel ilkeler:

1. **Tek soyutlama, iki kullanım.** "Runtime motoru" (Docker/Podman istemcileri) tek
   yerde durur; panel bunu yerel host için, agent uzak host için kullanır. Böylece
   kod tekrarı olmaz.
2. **Agent protokolü = panelin dahili API'si.** Panel, yerel host'u da sanki bir agent
   gibi aynı arayüzden yönetir; yerelde ağ katmanı devredışı kalır (in-process çağrı).
3. **Minimal arayüz.** Portainer gibi her şeyi yapmaya çalışan bir panel değil; hızlı,
   tek binary, az bağımlılık.

### 1.1 Crate yapısı (hedef)

```
crates/
  pdman/              # panel
    src/
      main.rs
      config.rs       # panel.toml
      hosts.rs        # host kaydı (local + agent'lar), sağlık durumu
      agent.rs        # pdman-agent HTTP istemcisi
      routes/         # UI (htmx partial'ları) + JSON API
      templates/      # askama
      style/          # tailwind kaynağı -> public/main.css
  pdman-agent/
    src/
      main.rs
      config.rs       # agent.toml
      api.rs          # axum router (pdman-common protokolünü serve eder)
      events.rs       # docker/podman event akışlarını birleştirir
  pdman-common/
    src/
      model.rs        # Container, Pod, Image, Volume, Network, Host, Stats, ...
      protocol.rs     # agent API sözleşmesi (istek/yanıt tipleri)
      runtime/
        mod.rs        # Runtime trait'i
        docker.rs     # bollard tabanlı
        podman.rs     # docker-compat API + libpod (pod'lar)
  pdman-core/         # (opsiyonel, pdman-common büyürse ayrılır)
```

> Not: Runtime motoru başlangıçta `pdman-common` içinde yaşar; grow` olduğunda
> `pdman-core` olarak ayrılabilir. `pdman-common` bağımlılık ağacını hafif tutmak için
> feature flag'ler kullanılır (`runtime` feature'ı bollard/reqwest'i çeker; sadece
> tipleri gereken taraf `default-features = false` ile kullanır).

### 1.2 Runtime soyutlaması (Docker + Podman)

Podman, `podman system service` ile Docker Engine API'siyle **uyumlu** bir socket
açar. Bu nedenle:

- **Container, image, volume, network** işlemleri → mevcut `bollard` istemcisi hem
  Docker hem Podman socket'ine bağlanabilir.
- **Podman pod'ları** → Docker API'de karşılığı yoktur. Pod'lar (ve ileride
  podman'a özgü özellikler) için Podman'ın kendi REST API'sinin (`/v5.0.0/libpod/...`)
  ihtiyaç duyduğumuz uçları için ince bir `reqwest` istemcisi yazılır. Pod'lar dışında
  libpod'a gerek yok; minimal kalır.

```rust
pub enum RuntimeKind { Docker, Podman }

#[async_trait]
pub trait Runtime: Send + Sync {
    fn kind(&self) -> RuntimeKind;
    async fn list_containers(&self) -> Result<Vec<Container>>;
    async fn container_action(&self, id: &str, action: ContainerAction) -> Result<()>;
    async fn logs(&self, id: &str, opts: &LogOpts) -> Result<LogStream>;   // stream
    async fn stats(&self, id: &str) -> Result<ContainerStats>;
    async fn events(&self) -> Result<EventStream>;                          // stream
    async fn list_images(&self) -> Result<Vec<Image>>;
    async fn list_volumes(&self) -> Result<Vec<Volume>>;
    async fn list_networks(&self) -> Result<Vec<Network>>;
    // yalnızca Podman:
    async fn list_pods(&self) -> Result<Vec<Pod>>;
    async fn pod_action(&self, id: &str, action: PodAction) -> Result<()>;
}
```

Mevcut `runtime.rs`'teki docker context çözümleme (`DOCKER_HOST`, `~/.docker/config.json`)
korunur ve `Host` kavramına taşınır. Podman socket keşfi: rootless için
`$XDG_RUNTIME_DIR/podman/podman.sock`, rootful için `/run/podman/podman.sock`.

### 1.3 Agent protokolü

Panel ↔ agent arası **HTTP/JSON + SSE** (gRPC yerine). Gerekçeler:

- Agent tarafında da axum kullanılır → tek web stack, paylaşılan tipler `serde` ile
  doğrudan serişleşir.
- htmx ile log/event akışları SSE ile çok kolay tüketilir.
- `curl` ile debug edilebilir; minimal bağımlılık (tonic/prost yok).

| Uç | Açıklama |
|---|---|
| `GET /api/v1/health` | Agent sağlığı, runtime'ların (docker/podman) durumu |
| `GET /api/v1/containers` | Tüm container'lar |
| `POST /api/v1/containers/:id/:action` | start / stop / restart / pause / unpause / remove |
| `GET /api/v1/containers/:id/logs?follow` | Log akışı (SSE) |
| `GET /api/v1/containers/:id/stats` | CPU/RAM istatistikleri |
| `GET /api/v1/events` | Birleşik docker+podman event akışı (SSE) |
| `GET /api/v1/pods` | (yalnız podman) Pod'lar |
| `POST /api/v1/pods/:id/:action` | start / stop / remove |
| `GET /api/v1/images`, `/volumes`, `/networks` | Listeleme (+ remove) |

Panel bu uçları yerel host için in-process, uzak hostlar için `agent.rs` istemcisiyle
çağırır; UI aynı kalır.

### 1.4 Yapılandırma

`pdman.toml` (panel):

```toml
[server]
bind = "127.0.0.1:3000"

[local]
enabled = true
# docker_socket / podman_socket ile override edilebilir

[[hosts]]
name = "prod1"
url = "https://10.0.0.5:7654"
token_file = "prod1.token"   # veya token = "..."
```

`pdman-agent.toml`:

```toml
[server]
bind = "0.0.0.0:7654"
token_file = "/etc/pdman-agent/token"   # statik bearer token

[docker]
socket = "/var/run/docker.sock"          # yoksa devre dışı

[podman]
socket = "unix:///run/podman/podman.sock"
```

### 1.5 Güvenlik

- **Faz 1-4 (başlangıç):** agent'da statik bearer token (`Authorization: Bearer ...`),
  sabit süreli yanıt imzası yok; panel yalnızca güvenilir ağda/VPN'de çalışır varsayımı.
- **Faz 5:** isteğe bağlı mTLS (rustls); agent'ın `127.0.0.1`'e bind edilip SSH tunnel
  üzerinden kullanılması da desteklenir (docker context `ssh://` mantığına benzer,
  agent kurulumu istenmeyen senaryolar için).
- Panelin kendisi başlangıçta localhost'a bind eder; çok kullanıcılı kimlik doğrulama
  **kapsam dışıdır** (kişisel yönetim aracı).

### 1.6 Arayüz (htmx + askama)

Mevcut yığın korunur: askama şablonları + htmx + tailwind (npm build adımı mevcut).

Ekranlar:

1. **Host'lar** — sol kenar çubuğu: `local`, kayıtlı agent'lar + sağlık göstergesi.
2. **Container'lar** — seçili hostun container listesi; durum renkleri, htmx ile
   aksiyon butonları (start/stop/restart/remove), SSE ile otomatik yenileme.
3. **Container detayı** — inspect özeti, canlı log akışı (SSE + autoscroll), stats.
4. **Pod'lar** — (yalnız podman hostları) pod listesi ve aksiyonları.
5. **Images / Volumes / Networks** — listeleme + remove.

Karmaşıklığı düşük tutmak adına başta SPA yok; tüm etkileşim htmx partial'ları.

### 1.7 Yalnızca agent kurulumu (sunucu tarafı)

Rust'ta bir binary'ye yalnızca kendi bağımlılık grafiğindeki crate'ler derlenir ve
linklenir; workspace üyesi olmak binary'ye dahil olmak anlamına gelmez. Bu yüzden
bağımlılık yönü şu kurallarla sabitlenir:

```
pdman (panel) ──► pdman-common ◄── pdman-agent
```

- `pdman-common` **yaprak** crate'tir: ne `pdman`'a ne `pdman-agent`'a bağımlıdır.
- `pdman-agent` yalnızca `pdman-common` (+ kendi web bağımlılıkları: axum, reqwest, ...)
  kullanır. Panelin kodu ve bağımlılıkları (askama, htmx statikleri, host kayıt
  mantığı vb.) agent binary'sine **asla girmez**.

Sunucuya kurulum yöntemleri (küçükten büyüğe):

1. **Kaynaktan, tek paket:** `cargo build --release -p pdman-agent` — yalnızca
   `pdman-agent` + `pdman-common` + agent'ın kendi bağımlılıkları derlenir.
   `cargo install --git <repo> pdman-agent` de aynı şekilde çalışır.
   Dikkat: `-p` olmadan `cargo build` workspace'in **tüm** üyelerini (panel dahil)
   derler; sunucuda daima `-p pdman-agent` kullanılmalı ya da ön-derlenmiş binary
   indirilmelidir.
2. **Ön-derlenmiş statik binary (önerilen):** CI'da `cargo build -p pdman-agent`
   ile Linux amd64/arm64 hedefleri derlenir, strip edilip release'lere ayrı artifact
   olarak konur. Sunucuya tek dosya + systemd unit + `pdman-agent.toml` örneği
   indirilir; Rust toolchain'i gerekmez.
3. **Konteyner:** agent için multi-stage Dockerfile (`scratch`/distroless üstüne tek
   binary) — sunucuda agent'ı podman/docker ile çalıştırmak isteyenler için.

**Koruma (CI guard):** `cargo tree -p pdman-agent` çıktısında `pdman` panel crate'inin
geçmemesi bir CI adımıyla assert edilir; birisi yanlışlıkla agent'a panel bağımlılığı
eklerse build kırılır. Ayrıca `pdman-common`'da feature flag'ler (`runtime`, `protocol`)
sayesinde yalnız tipleri gereken taraf ağır bağımlılıkları (bollard, reqwest) çekmez.

---

## 2. Yol haritası

### Faz 0 — Temel (mevcut kodun toparlanması) — ✅ tamamlandı
- [x] `pdman-common`: `Container`, `Pod`, `Image`, `Volume`, `Network`, `Host`, `Stats`,
      `Event` tipleri; `thiserror` ile ortak hata tipi.
- [x] `runtime.rs` → `pdman-common::runtime` (trait + bollard Docker implementasyonu
      taşındı; `container_action`, `list_images/volumes/networks` eklendi).
- [x] TOML yapılandırma (panel `pdman.toml` + agent `pdman-agent.toml`), `clap` ile
      `--config` bayrağı.
- [x] Workspace lint ayarları, CI (fmt + clippy + test + agent bağımlılık guard'ı:
      `scripts/check-agent-deps.sh`).
- [x] Agent iskeleti: `/health` uç noktası (`HostInfo`), socket varlık kontrolü.
- [x] `pdman-common`'da `runtime` feature'ı: bollard/async-trait/futures-core/dirs
      yalnızca bu feature ile çekilir.

### Faz 1 — Yerel Docker + Podman — ✅ tamamlandı
- [x] `Runtime` trait'i genişletildi (`ping`); Podman implementasyonu: docker-compat
      socket üzerinden bollard delegasyonu (container/image/volume/network/events),
      socket keşfi (`PODMAN_HOST`/`CONTAINER_HOST` → rootless → rootfull).
- [x] `DOCKER_HOST`/context mantığı korundu; `events()` bollard'ın `&self`'e
      bağlı akışı kanal+spawn ile `'static` `EventStream`'e çevrildi.
- [x] UI: host seçici kenar çubuğu (yerel docker/podman, çevrimiçi noktası),
      container listesi tablosu, durum etiketleri, hata kartları.
- [x] SSE ile canlı liste güncelleme: `/api/events` (`containers-changed`),
      host başına olay iletici görevleri (kopma halinde yeniden bağlanma),
      htmx tarafında 30 saniyelik yedek yenileme.
- [x] Tek-binary ilkesi: `main.css` derleme zamanında gömüldü, `tower-http`
      bağımlılığı paneldden kaldırıldı.
- Not: `exec_create`/`exec_start` gibi container olayları da yenileme tetikler
  (gürültü); ileride action bazlı filtre eklenebilir.

### Faz 2 — Container işlemleri (yerel) — ✅ tamamlandı
- [x] start / stop / restart / pause / unpause / remove uçları
      (`POST /containers/{id}/{action}`) + duruma göre htmx butonları
      (çalışan: Durdur/Yeniden/Duraklat; duraklatılmış: Sürdür; durmuş:
      Başlat/Sil + onay diyaloğu), eylem sonrası tablo tazelenir.
- [x] Log görüntüleyici: `GET /api/containers/{id}/logs` (SSE, `log-line`
      olayları, zaman damgalı, `tail`/`follow` parametreleri, HTML kaçışlama),
      detay sayfasında canlı akan `<pre>` görünümü.
- [x] Stats (CPU/RAM) kartı: `container_stats` implementasyonu (precpu
      delta'sından CPU%, önbellek düşülerek bellek), 2 saniyede bir yenilenen
      partial.
- [x] Inspect özeti: `inspect_container` + `ContainerInspect` modeli (image,
      komut, oluşturulma, portlar, bağlamalar), detay sayfasında tablo.
- [x] Düzeltme: `htmx-ext-sse` script URL'i hatalıydı (`2.3.0` yok) —
      `2.2.4/dist/sse.min.js` yapıldı; SSE yenileme artık tarayıcıda da
      doğrulandı (30 saniyelik yedek yenileme korunuyor).

### Faz 3 — Pod'lar ve diğer kaynaklar — ✅ tamamlandı
- [x] Pod listesi + start/stop/remove (podman): `pdman-common::runtime::libpod`
      istemcisi (hyper + hyperlocal, bollard ile aynı yığın — `v4.0.0` API
      öneki, Podman 4.x ve 5.x uyumlu); `Podman::list_pods`/`pod_action`,
      TCP uç noktalarda net hata.
- [x] Images / volumes / networks listeleme + remove: trait'e `image_remove`/
      `volume_remove`/`network_remove`; panelde tablo sayfaları, eylem
      butonları (onay diyaloğuyla), 30 saniyelik otomatik yenileme.
- [x] UI: `base.html` ortak düzenine geçildi (hostlar + kaynak türü kenar
      çubuğu); docker hostunda pod'lar sayfası açıklayıcı mesaj gösterir.
- Not: libpod istemcisi yerelde podman olmadığından derleme + birim testlerle
  doğrulandı; gerçek podman sunucusunda uçtan uca test Faz 4 sonrası yapılacak.

### Faz 4 — pdman-agent ve uzak hostlar — ✅ tamamlandı
- [x] `pdman-agent`: `/api/v1/*` uçları — agent, panelin `Runtime`
      soyutlamasının HTTP yansımasını sunar (container/pod/image/volume/network
      listeleme + eylemler, inspect, stats, log ve olay akışları; `?runtime=`
      ile docker/podman seçimi). Statik token auth (tüm uçlar); token yoksa
      yalnızca geri-döngü bind'e izin (SSH tüneli senaryosu).
- [x] `pdman` → `agent.rs`: hyper tabanlı `AgentClient` — `Runtime` trait'inin
      HTTP implementasyonu (UI sıfır değişiklikle uzak hostları yönetir);
      parça-parça beslemeli SSE çözümleyici (+4 test); sağlık denetimi
      (5 sn zaman aşımı).
- [x] `pdman-common::protocol::api`: uç yolu üreticileri — agent ve panel
      tek doğruluk kaynağından beslenir; eylem ayrıştırma (`parse_container_action`
      / `parse_pod_action`) common'a taşındı.
- [x] UI'da uzak hostlar: `[[hosts]]` yapılandırması → sağlık yanıtındaki her
      kullanılabilir runtime için ayrı kayıt (`prod1-docker`, `prod1-podman`);
      agent çevrimdışıysa kırmızı nokta + neden.
- [x] Uzak log/event akışı: agent SSE (`log-line`, `runtime-event`) → panel
      istemcisi → panel SSE (`containers-changed`) köprüsü.
- [x] Uçtan uca doğrulama (yerel agent + colima docker): auth 401/200,
      container listesi, aksiyon, inspect, stats, loglar, olay akışı ve
      çevrimdışı senaryo — hepsi gerçek akış üzerinden test edildi.

### Faz 5 — Güvenlik, paketleme, yayın — ✅ tamamlandı
- [x] Opsiyonel TLS ve mTLS (rustls, yalnızca `ring` sağlayıcısı): agent
      `[tls]` bölümüyle TLS dinler; `ca_file` verilirse istemci sertifikası
      zorunlu (mTLS). Panel `[[hosts]]` altında `ca_file`/`cert_file`/
      `key_file` ile bağlanır (hyper-rustls, `https_or_http`). Token auth
      korunur (derinlemesine savunma). E2E doğrulandı: sertifikasız bağlantı
      reddi, sertifika + token ile tam akış (container listesi, loglar).
- [x] Tek statik binary hedefleri: `x86_64-unknown-linux-musl` ve
      `aarch64-unknown-linux-musl` (native runner'lar), her ikisi de
      binary için ayrı tar.gz (panel: binary + örnek config; agent: binary +
      systemd servisi + kurulum betiği). Release profili: thin LTO,
      codegen-units=1; strip CI'da.
- [x] systemd unit'leri (`deploy/pdman*.service`, sağlamlaştırma bayraklarıyla),
      agent için Dockerfile (`deploy/Dockerfile.agent`, distroless, yalnız
      agent binary'si derlenir) ve kurulum betiği
      (`deploy/install-agent.sh`).
- [x] README (mimari, hızlı başlangıç, güvenlik notları), örnek config'ler
      (`examples/`), LICENSE.
- [x] CI: test + agent guard; release iş akışı (`.github/workflows/release.yml`,
      `v*` tag'lerinde).
- Bilinen ortam notu: bu makinenin rustup'ında `rust-objcopy` bozuk
  (libLLVM eksik); release derlemedeki strip uyarısı yereldir, CI'da yoktur.

---

## 3. Kapsam dışı (bilinçli kararlar)

- Çok kullanıcılı kimlik doğrulama / RBAC (kişisel/homelab aracı).
- Swarm / Kubernetes yönetimi.
- Compose dosyası deploy'u (ileride düşünülebilir; önce container/pod düzeyi).
- UI framework değişikliği (React/Vue vb.) — htmx + askama yeterli.

## 4. Teknoloji özeti

| Katman | Seçim | Gerekçe |
|---|---|---|
| Panel | axum + askama + htmx + tailwind | mevcut, minimal, tek binary |
| Docker istemci | bollard | mevcut; podman docker-compat API ile de çalışır |
| Podman istemci | ince reqwest (libpod) | yalnız pod'lar için gerekli, podman-api crate'i arşivlenmiş durumda |
| Agent protokolü | HTTP/JSON + SSE, bearer token | tek web stack, htmx ile uyumlu, debug edilebilir |
| Ortak tipler | pdman-common (serde) | panel/agent tek doğruluk kaynağı |
| Yapılandırma | TOML | standart, okunabilir |
