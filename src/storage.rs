//! Storage connectors — where the queue snapshot is persisted.
//!
//! The default is a plain JSON file: zero extra dependencies, the lightest
//! option, perfect for a single site. Other backends are **optional**, compiled
//! in only via Cargo features so the default binary stays tiny:
//!
//!   cargo build --release --features sqlite      # local single-file DB
//!   cargo build --release --features postgres    # external Postgres
//!   cargo build --release --features mongo       # external MongoDB
//!
//! Pick one at runtime:
//!   INLINE_STORAGE      = json | sqlite | postgres | mongo   (default: json)
//!   INLINE_DATABASE_URL = connection string (for the DB backends)
//!   INLINE_DB_NAME      = database name (mongo only; default "inline")
//!
//! Every backend simply loads/saves the whole `Snapshot` (a small JSON
//! document), so the model is identical across databases and adding a new one
//! is a few lines. See CUSTOMIZE.md.

use crate::store::Snapshot;

/// A connected storage backend.
pub enum Storage {
    Json(JsonBackend),
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    Sql(SqlBackend),
    #[cfg(feature = "mongo")]
    Mongo(MongoBackend),
}

impl Storage {
    /// Choose + connect a backend from the environment. Exits with a clear
    /// message if the selection needs a feature the binary wasn't built with,
    /// or if a required URL is missing / the database is unreachable.
    pub async fn from_env() -> Storage {
        let kind = std::env::var("INLINE_STORAGE").unwrap_or_else(|_| "json".into());
        match kind.trim().to_lowercase().as_str() {
            "json" | "file" | "" => {
                let path = std::env::var("INLINE_DATA_FILE").unwrap_or_else(|_| "data.json".into());
                Storage::Json(JsonBackend { path })
            }
            kind @ ("sqlite" | "postgres" | "postgresql" | "pg") => connect_sql(kind).await,
            "mongo" | "mongodb" => connect_mongo().await,
            other => fail(&format!(
                "unknown INLINE_STORAGE '{other}' (use json|sqlite|postgres|mongo)"
            )),
        }
    }

    pub async fn load(&self) -> Snapshot {
        match self {
            Storage::Json(b) => b.load(),
            #[cfg(any(feature = "sqlite", feature = "postgres"))]
            Storage::Sql(b) => b.load().await,
            #[cfg(feature = "mongo")]
            Storage::Mongo(b) => b.load().await,
        }
    }

    pub async fn save(&self, snap: &Snapshot) {
        match self {
            Storage::Json(b) => b.save(snap),
            #[cfg(any(feature = "sqlite", feature = "postgres"))]
            Storage::Sql(b) => b.save(snap).await,
            #[cfg(feature = "mongo")]
            Storage::Mongo(b) => b.save(snap).await,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Storage::Json(b) => format!("json file ({})", b.path),
            #[cfg(any(feature = "sqlite", feature = "postgres"))]
            Storage::Sql(b) => format!("{} (sqlx)", b.kind),
            #[cfg(feature = "mongo")]
            Storage::Mongo(_) => "mongodb".to_string(),
        }
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("[storage] {msg}");
    std::process::exit(1);
}

// JSON file backend (default, always available).

pub struct JsonBackend {
    pub path: String,
}

impl JsonBackend {
    fn load(&self) -> Snapshot {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| serde_json::from_str::<Snapshot>(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, snap: &Snapshot) {
        let json = match serde_json::to_string_pretty(snap) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[storage] serialize error: {e}");
                return;
            }
        };
        // Atomic: write a temp file then rename over the target.
        let tmp = format!("{}.tmp", self.path);
        if std::fs::write(&tmp, json)
            .and_then(|_| std::fs::rename(&tmp, &self.path))
            .is_err()
        {
            eprintln!("[storage] failed to write {}", self.path);
        }
    }
}

// SQL backend: SQLite / Postgres via sqlx (feature-gated).
//
// We store the whole snapshot as one JSON document in a single-row table. This
// keeps the model uniform across SQLite and Postgres and matches the project's
// lightweight ethos. (Want normalized tables for SQL reporting? That's a
// straightforward extension — see CUSTOMIZE.md.)

#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub struct SqlBackend {
    pool: sqlx::AnyPool,
    pub kind: String,
}

#[cfg(any(feature = "sqlite", feature = "postgres"))]
async fn connect_sql(kind: &str) -> Storage {
    use sqlx::any::AnyPoolOptions;
    sqlx::any::install_default_drivers();

    let url = std::env::var("INLINE_DATABASE_URL").unwrap_or_else(|_| {
        fail("INLINE_STORAGE is a SQL backend but INLINE_DATABASE_URL is not set")
    });
    let pool = match AnyPoolOptions::new().max_connections(5).connect(&url).await {
        Ok(p) => p,
        Err(e) => fail(&format!("could not connect to database: {e}")),
    };
    if let Err(e) =
        sqlx::query("CREATE TABLE IF NOT EXISTS inline_state (id INTEGER PRIMARY KEY, data TEXT NOT NULL)")
            .execute(&pool)
            .await
    {
        fail(&format!("could not create table: {e}"));
    }
    Storage::Sql(SqlBackend { pool, kind: kind.to_string() })
}

#[cfg(not(any(feature = "sqlite", feature = "postgres")))]
async fn connect_sql(_kind: &str) -> Storage {
    fail("this binary was built without SQL support — rebuild with `--features sqlite` or `--features postgres`")
}

#[cfg(any(feature = "sqlite", feature = "postgres"))]
impl SqlBackend {
    async fn load(&self) -> Snapshot {
        match sqlx::query_scalar::<_, String>("SELECT data FROM inline_state WHERE id = 1")
            .fetch_optional(&self.pool)
            .await
        {
            Ok(Some(s)) => serde_json::from_str(&s).unwrap_or_default(),
            Ok(None) => Snapshot::default(),
            Err(e) => {
                eprintln!("[storage] load failed: {e}");
                Snapshot::default()
            }
        }
    }

    async fn save(&self, snap: &Snapshot) {
        let data = match serde_json::to_string(snap) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[storage] serialize error: {e}");
                return;
            }
        };
        let res = sqlx::query(
            "INSERT INTO inline_state (id, data) VALUES (1, ?) \
             ON CONFLICT (id) DO UPDATE SET data = excluded.data",
        )
        .bind(data)
        .execute(&self.pool)
        .await;
        if let Err(e) = res {
            eprintln!("[storage] save failed: {e}");
        }
    }
}

// MongoDB backend (feature-gated).

#[cfg(feature = "mongo")]
pub struct MongoBackend {
    coll: mongodb::Collection<mongodb::bson::Document>,
}

#[cfg(feature = "mongo")]
async fn connect_mongo() -> Storage {
    let url = std::env::var("INLINE_DATABASE_URL")
        .unwrap_or_else(|_| fail("INLINE_STORAGE=mongo but INLINE_DATABASE_URL is not set"));
    let db_name = std::env::var("INLINE_DB_NAME").unwrap_or_else(|_| "inline".into());
    let client = match mongodb::Client::with_uri_str(&url).await {
        Ok(c) => c,
        Err(e) => fail(&format!("could not connect to MongoDB: {e}")),
    };
    let coll = client.database(&db_name).collection("inline_state");
    Storage::Mongo(MongoBackend { coll })
}

#[cfg(not(feature = "mongo"))]
async fn connect_mongo() -> Storage {
    fail("this binary was built without MongoDB support — rebuild with `--features mongo`")
}

#[cfg(feature = "mongo")]
impl MongoBackend {
    async fn load(&self) -> Snapshot {
        use mongodb::bson::doc;
        match self.coll.find_one(doc! { "_id": "state" }).await {
            Ok(Some(d)) => d
                .get_str("data")
                .ok()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default(),
            Ok(None) => Snapshot::default(),
            Err(e) => {
                eprintln!("[storage] load failed: {e}");
                Snapshot::default()
            }
        }
    }

    async fn save(&self, snap: &Snapshot) {
        use mongodb::bson::doc;
        let data = match serde_json::to_string(snap) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[storage] serialize error: {e}");
                return;
            }
        };
        let res = self
            .coll
            .replace_one(doc! { "_id": "state" }, doc! { "_id": "state", "data": data })
            .upsert(true)
            .await;
        if let Err(e) = res {
            eprintln!("[storage] save failed: {e}");
        }
    }
}
