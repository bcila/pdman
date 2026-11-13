//! PostgreSQL katmanı: arayüzden eklenen sunucuların kalıcı saklanması.
//!
//! Faz 6 kapsamında yalnızca `servers` tablosu vardır; ileride kullanıcı
//! girişi gibi özellikler de aynı şemaya eklenecektir. Şema, açılışta
//! `CREATE TABLE IF NOT EXISTS` ile kurulur (ayrı göç aracı yok).

use sqlx::{PgPool, postgres::PgPoolOptions};

use pdman_common::{Error, Result};

/// Veritabanına kaydedilmiş bir sunucu kaydı.
#[derive(Debug, Clone)]
pub struct ServerRecord {
    /// Kısa kimlik (addan üretilen slug).
    pub id: String,
    /// Panelde görünen ad.
    pub name: String,
    /// Agent'ın adresi (`https://10.0.0.5:7654`).
    pub url: String,
    /// Taşıyıcı token'ı; boşsa kimlik doğrulama yok (geri-döngü senaryosu).
    pub token: String,
    /// Agent'ın sertifikasını imzalayan CA dosyası (PEM).
    pub ca_file: String,
    /// İstemci sertifikası (PEM); mTLS için.
    pub cert_file: String,
    /// İstemci özel anahtarı (PEM).
    pub key_file: String,
}

/// PostgreSQL bağlantısı; `[database]` yapılandırılmamışsa `None` döner.
pub struct Db {
    pool: PgPool,
}

impl Db {
    /// Bağlanır ve şemayı kurar.
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(|err| Error::config(format!("veritabanına bağlanılamadı: {err}")))?;

        let db = Self { pool };
        db.migrate().await?;
        Ok(db)
    }

    /// Şemayı kurar (idempotent).
    async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS servers (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                url        TEXT NOT NULL,
                token      TEXT NOT NULL DEFAULT '',
                ca_file    TEXT NOT NULL DEFAULT '',
                cert_file  TEXT NOT NULL DEFAULT '',
                key_file   TEXT NOT NULL DEFAULT '',
                created_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|err| Error::config(format!("şema kurulamadı: {err}")))?;

        Ok(())
    }

    /// Tüm kayıtlı sunucular.
    pub async fn list_servers(&self) -> Result<Vec<ServerRecord>> {
        let rows = sqlx::query_as::<_, ServerRow>(
            "SELECT id, name, url, token, ca_file, cert_file, key_file FROM servers ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| Error::runtime(format!("sunucular okunamadı: {err}")))?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Yeni sunucu ekler.
    pub async fn insert_server(&self, server: &ServerRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO servers (id, name, url, token, ca_file, cert_file, key_file) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&server.id)
        .bind(&server.name)
        .bind(&server.url)
        .bind(&server.token)
        .bind(&server.ca_file)
        .bind(&server.cert_file)
        .bind(&server.key_file)
        .execute(&self.pool)
        .await
        .map_err(|err| Error::runtime(format!("sunucu kaydedilemedi: {err}")))?;

        Ok(())
    }

    /// Sunucuyu siler; kayıt yoksa sessizce başarılı sayılır.
    pub async fn delete_server(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM servers WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|err| Error::runtime(format!("sunucu silinemedi: {err}")))?;

        Ok(())
    }
}

/// `servers` tablosunun satır tipi (boş olabilen alanlar `String`).
#[derive(Debug, sqlx::FromRow)]
struct ServerRow {
    id: String,
    name: String,
    url: String,
    token: String,
    ca_file: String,
    cert_file: String,
    key_file: String,
}

impl From<ServerRow> for ServerRecord {
    fn from(row: ServerRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            url: row.url,
            token: row.token,
            ca_file: row.ca_file,
            cert_file: row.cert_file,
            key_file: row.key_file,
        }
    }
}
