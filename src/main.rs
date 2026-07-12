//! inline — a tiny, self-hostable queue system.
//!
//! One small Rust binary serves a JSON API, a live-update stream (SSE), and
//! two static single-file web apps (admin + customer). State lives in memory
//! and is snapshotted to a JSON file. See README.md for the big picture.

mod broker;
mod config;
mod handlers;
mod storage;
mod store;

use crate::broker::Broker;
use crate::config::Config;
use crate::storage::Storage;
use crate::store::Store;
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;
use tokio::sync::{watch, RwLock};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

/// Shared application state, cheaply clonable into every handler.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<RwLock<Store>>,
    pub broker: Broker,
    pub config: Arc<Config>,
    /// Public base URL of the customer app (from INLINE_PUBLIC_URL); used to
    /// build the link/QR handed to guests. May be empty.
    pub public_url: String,
    /// Operator token. `None` means auth is disabled.
    pub admin_token: Option<String>,
    /// Tickets older than this many seconds are treated as expired. 0 = never.
    pub ticket_ttl_secs: u64,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// A bind address like "0.0.0.0:8080" or "[::]:8080" tells the OS "listen on
/// every interface" — it isn't itself a URL you can open, and most browsers
/// refuse to navigate to a wildcard host directly. Swap it for "localhost"
/// when printing clickable links; the real bind address is still shown on
/// the "listening" line.
fn browsable_host(bind: &str) -> String {
    match bind.rsplit_once(':') {
        Some((host, port)) if host.is_empty() || host == "0.0.0.0" || host == "[::]" => {
            format!("localhost:{port}")
        }
        _ => bind.to_string(),
    }
}

/// The API router (everything under `/api`), with `state` baked in. Split out
/// from `main()` so handler tests can drive it directly with `tower::ServiceExt`
/// without needing a real listener.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/config", get(handlers::get_config))
        .route("/state", get(handlers::get_state))
        .route("/health", get(handlers::health))
        .route("/entries", get(handlers::list_entries).post(handlers::create_entry))
        .route("/entries/:id", get(handlers::get_entry))
        .route("/entries/:id/status", post(handlers::set_status))
        .route("/queue/:code/next", post(handlers::next_queue))
        .route("/queue/:code/reset", post(handlers::reset_type))
        .route("/reset", post(handlers::reset_all))
        .route("/admin/export", get(handlers::export_data))
        .route("/admin/import", post(handlers::import_data))
        .route("/events", get(handlers::events))
        .route("/qr", get(handlers::qr))
        .with_state(state)
}

/// Parse a ticket-TTL string like "1d", "12h", "30m", "3600" (bare = seconds),
/// or "0"/"off"/"never" (never expire) into a number of seconds.
fn parse_ttl(raw: &str) -> u64 {
    let s = raw.trim().to_lowercase();
    if s.is_empty() || s == "0" || s == "off" || s == "never" {
        return 0;
    }
    let (num, mult) = if let Some(v) = s.strip_suffix('d') {
        (v, 86_400)
    } else if let Some(v) = s.strip_suffix('h') {
        (v, 3_600)
    } else if let Some(v) = s.strip_suffix('m') {
        (v, 60)
    } else if let Some(v) = s.strip_suffix('s') {
        (v, 1)
    } else {
        (s.as_str(), 1)
    };
    num.trim().parse::<u64>().map(|n| n * mult).unwrap_or(86_400)
}

/// Standalone healthcheck for `docker healthcheck`/compose (`./inline
/// healthcheck`): opens a raw TCP connection to our own `/api/health`
/// endpoint — no HTTP client dependency needed — and succeeds only on a 200
/// response. The runtime image has no curl/wget, so this is how the
/// container reports liveness.
fn healthcheck_status() -> i32 {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let bind = env_or("INLINE_BIND", "0.0.0.0:8080");
    let port = bind.rsplit(':').next().unwrap_or("8080");
    let addr = format!("127.0.0.1:{port}");

    let Ok(mut stream) = TcpStream::connect(&addr) else {
        return 1;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    if stream
        .write_all(b"GET /api/health HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return 1;
    }
    let mut buf = [0u8; 32];
    match stream.read(&mut buf) {
        Ok(n) if n > 0 => {
            let head = String::from_utf8_lossy(&buf[..n]);
            if head.starts_with("HTTP/1.0 200") || head.starts_with("HTTP/1.1 200") {
                0
            } else {
                1
            }
        }
        _ => 1,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env if present (no-op when missing).
    dotenvy::dotenv().ok();

    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(healthcheck_status());
    }

    let bind = env_or("INLINE_BIND", "0.0.0.0:8080");
    let public_url = std::env::var("INLINE_PUBLIC_URL").unwrap_or_default();
    let config_path = env_or("INLINE_CONFIG", "config.json");
    let public_dir = env_or("INLINE_PUBLIC_DIR", "public");
    let admin_token = std::env::var("ADMIN_TOKEN").ok().filter(|s| !s.is_empty());
    let ticket_ttl_secs = parse_ttl(&env_or("INLINE_TICKET_TTL", "1d"));

    let config = Config::load(&config_path);

    // Choose + connect the storage backend (JSON file by default), load the
    // existing snapshot, and wire a background task that persists every change
    // off the request path.
    let storage = Storage::from_env().await;
    let storage_desc = storage.describe();
    let snapshot = storage.load().await;
    println!("[store] loaded {} entries from {storage_desc}", snapshot.entries.len());
    let mut store = Store::from_snapshot(snapshot);
    let (tx, mut rx) = watch::channel(store.snapshot());
    store.set_sender(tx);
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let snap = rx.borrow_and_update().clone();
            storage.save(&snap).await;
        }
    });

    let state = AppState {
        store: Arc::new(RwLock::new(store)),
        broker: Broker::default(),
        config: Arc::new(config),
        public_url: public_url.clone(),
        admin_token: admin_token.clone(),
        ticket_ttl_secs,
    };

    // API routes (state baked in here so the outer router stays stateless).
    let api = build_router(state);

    // Serve the two single-file apps (public/index.html, public/admin.html)
    // for everything that isn't an /api route.
    let static_files = ServeDir::new(&public_dir).append_index_html_on_directories(true);

    let app = Router::new()
        .nest("/api", api)
        .fallback_service(static_files)
        // Permissive CORS so the customer app can be hosted on a different
        // origin if you want. Tighten this for production — see CUSTOMIZE.md.
        .layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(&bind).await?;

    println!("\n  inline is running");
    let browsable = browsable_host(&bind);
    println!("  ├─ listening    http://{bind}");
    println!("  ├─ admin app    http://{browsable}/admin.html");
    println!("  ├─ customer app http://{browsable}/");
    println!("  ├─ display app  http://{browsable}/display.html");
    println!(
        "  ├─ public URL   {}",
        if public_url.is_empty() { "(admin origin)".into() } else { public_url }
    );
    println!("  ├─ static dir   {public_dir}");
    println!("  ├─ storage      {storage_desc}");
    println!(
        "  ├─ ticket TTL   {}",
        if ticket_ttl_secs == 0 { "disabled".into() } else { format!("{ticket_ttl_secs}s") }
    );
    match &admin_token {
        Some(_) => println!("  └─ operator auth ENABLED (ADMIN_TOKEN set)\n"),
        None => println!("  └─ operator auth DISABLED — set ADMIN_TOKEN before going live\n"),
    }

    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{browsable_host, parse_ttl};

    #[test]
    fn browsable_host_swaps_wildcard_for_localhost() {
        assert_eq!(browsable_host("0.0.0.0:8080"), "localhost:8080");
        assert_eq!(browsable_host("[::]:8080"), "localhost:8080");
        assert_eq!(browsable_host("127.0.0.1:8080"), "127.0.0.1:8080");
        assert_eq!(browsable_host("queue.example.com:8080"), "queue.example.com:8080");
    }

    #[test]
    fn parse_ttl_units() {
        assert_eq!(parse_ttl("1d"), 86_400);
        assert_eq!(parse_ttl("12h"), 43_200);
        assert_eq!(parse_ttl("30m"), 1_800);
        assert_eq!(parse_ttl("3600"), 3_600);
        assert_eq!(parse_ttl("45s"), 45);
    }

    #[test]
    fn parse_ttl_disabled_values() {
        assert_eq!(parse_ttl("0"), 0);
        assert_eq!(parse_ttl("off"), 0);
        assert_eq!(parse_ttl("never"), 0);
        assert_eq!(parse_ttl(""), 0);
    }

    #[test]
    fn parse_ttl_garbage_falls_back_to_default() {
        assert_eq!(parse_ttl("not-a-number"), 86_400);
    }
}
