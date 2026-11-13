# pdman

Docker ve Podman container'larını — yerel makinede veya uzak sunucularda —
tek bir minimal panelden yönetmek için araç. Rust ile yazılmıştır; panel ve
agent tek statik binary olarak dağıtılır.

## Özellikler

- **Docker + Podman**: Podman'ın Docker-uyumlu API'si üzerinden container,
  image, volume ve network yönetimi; pod'lar için Podman'ın kendi API'si
  (libpod).
- **Yerel + uzak hostlar**: Panel, çalıştığı makinedeki runtime'lara socket
  üzerinden doğrudan; uzak sunuculardaki runtime'lara `pdman-agent` üzerinden
  bağlanır.
- **Canlı arayüz**: htmx + SSE ile container/pod olaylarında anında yenilenen
  tablolar, canlı log akışı, CPU/RAM istatistikleri.
- **Container işlemleri**: başlat/durdur/yeniden başlat/duraklat/sil; pod
  başlat/durdur/sil; image/volume/network silme.
- **Tek binary**: şablonlar ve stil sayfası binary'ye gömülür; çalışma zamanı
  bağımlılığı yoktur.
- **Güvenlik**: agent'da taşıyıcı token (zorunlu, dış bind için), opsiyonel
  TLS ve karşılıklı TLS (mTLS).
- **Sunucu yönetimi**: `/servers` sayfasından uzak sunucu ekleme/silme ve
  bağlantı sınama; kayıtlar PostgreSQL'de saklanır (yeniden başlatmada
  korunur). Ağ üzerinden erişilen sunucular için `https://` + token
  zorunludur.

## Mimari

```
┌──────────────────────────┐
│          pdman           │  ← tarayıcı → panel (axum + htmx)
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

- **`pdman`** — panel. Yerel runtime'lara doğrudan, uzak hostlara
  `pdman-agent` üzerinden bağlanır; her ikisi de aynı `Runtime` soyutlamasını
  kullanır.
- **`pdman-agent`** — uzak sunucularda çalışan hafif aracı; panelin
  soyutlamasının HTTP yansımasını (`/api/v1`) sunar.
- **`pdman-common`** — paylaşılan tipler, protokol ve Docker/Podman istemcileri.

## Hızlı başlangıç

### Panel (yerel Docker/Podman ile)

```sh
pdman --config pdman.toml
# http://127.0.0.1:3000
```

Yapılandırma örneği için [`examples/pdman.toml`](examples/pdman.toml).
Dosta yoksa panel varsayılan ayarlarla yerel runtime'ları arar.

### Uzak sunucu

1. Agent'ı kurun (ör. `deploy/install-agent.sh`) ve token'ı ayarlayın.
2. Sunucuyu panele ekleyin — iki yol:

   **Arayüzden** (önerilen): `pdman.toml`'da veritabanını ayarlayın, paneli
   açın ve **Sunucular** sayfasından ekleyin:

   ```toml
   [database]
   url = "postgres://pdman:sifre@localhost:5432/pdman"
   ```

   Formda ad, adres, token ve (mTLS için) sertifika yolları girilir; sunucu
   yalnızca bağlantı denetimini geçerse kaydedilir. Kayıtlar PostgreSQL'de
   saklanır.

   **Yapılandırmadan**: `[[hosts]]` girişi olarak elle ekleyin (arayüzden
   silinemez, dosyadan yönetilir):

   ```toml
   [[hosts]]
   name = "prod1"
   url = "https://10.0.0.5:7654"
   token_file = "/etc/pdman/prod1.token"
   ```

Agent yapılandırması için [`examples/pdman-agent.toml`](examples/pdman-agent.toml).

### mTLS (opsiyonel)

Agent tarafında `[tls]` bölümü ile sunucu sertifikası (ve istemci CA'sı)
verin; panel tarafında `[[hosts]]` altında `ca_file`/`cert_file`/`key_file`
ile karşılıklı sertifika doğrulaması yapın. Ayrıntılar örnek config'lerde.

## Derleme

```sh
cargo build --release -p pdman        # panel
cargo build --release -p pdman-agent  # agent (panel bağımlılığı içermez)
```

Agent binary'si yalnızca `pdman-common`'a bağımlıdır; panel kodu asla
agent'a derlenmez (CI'da `scripts/check-agent-deps.sh` ile garanti edilir).

Frontend stilini yeniden üretmek için (şablonlar değiştiyse):

```sh
cd crates/pdman && npx @tailwindcss/cli -i style/main.css -o public/main.css
```

## Güvenlik notları

- Panelde kimlik doğrulama **yoktur**; localhost'a bind edip ters vekil ya
  da SSH tüneli ile kullanın (kişisel yönetim aracı tasarımı).
- Agent, dış bir adrese bind edildiğinde token **zorunludur**; token yoksa
  yalnızca geri-döngü adreslerine bind edilmesine izin verilir.
- `/servers` formu ağ üzerinden erişilen sunucularda `https://` ve token
  olmadan ekleme yapmayı reddeder; düz HTTP yalnızca yerel adreslerde
  (SSH tüneli) kullanılabilir.
- Arayüzden eklenen sunucu bilgileri (token dahil) PostgreSQL'de saklanır;
  listeleme yanıtlarında token tarayıcıya geri gönderilmez. Veritabanı
  erişimini kısıtlayın (ör. `localhost` + güçlü parola).
- mTLS, agent'a yalnızca bilinen istemcilerin bağlanmasını garanti eder.

## Yol haritası ve tasarım kararları

Ayrıntılı plan ve mimari kararlar için [PLAN.md](PLAN.md)'e bakın.

## Lisans

MIT
