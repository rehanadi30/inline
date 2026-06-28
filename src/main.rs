//! inline — a tiny, self-hostable queue system.
//!
//! One small Rust binary serves a JSON API, a live-update stream (SSE), and
//! two static single-file web apps (admin + customer). State lives in memory
//! and is snapshotted to a JSON file. See README.md for the big picture.

mod broker;
mod config;
mod handlers;
mod store;

use crate::broker::Broker;
use crate::config::Config;
use crate::store::Store;
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;
use tokio::sync::RwLock;
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
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env if present (no-op when missing).
    dotenvy::dotenv().ok();

    let bind = env_or("INLINE_BIND", "0.0.0.0:8080");
    let public_url = std::env::var("INLINE_PUBLIC_URL").unwrap_or_default();
    let data_file = env_or("INLINE_DATA_FILE", "data.json");
    let config_path = env_or("INLINE_CONFIG", "config.json");
    let public_dir = env_or("INLINE_PUBLIC_DIR", "public");
    let admin_token = std::env::var("ADMIN_TOKEN").ok().filter(|s| !s.is_empty());

    let config = Config::load(&config_path);
    let store = Store::load(&data_file);

    let state = AppState {
        store: Arc::new(RwLock::new(store)),
        broker: Broker::default(),
        config: Arc::new(config),
        public_url: public_url.clone(),
        admin_token: admin_token.clone(),
    };

    // API routes (state baked in here so the outer router stays stateless).
    let api = Router::new()
        .route("/config", get(handlers::get_config))
        .route("/state", get(handlers::get_state))
        .route("/entries", get(handlers::list_entries).post(handlers::create_entry))
        .route("/entries/:id", get(handlers::get_entry))
        .route("/entries/:id/status", post(handlers::set_status))
        .route("/queue/:code/next", post(handlers::next_queue))
        .route("/queue/:code/reset", post(handlers::reset_type))
        .route("/reset", post(handlers::reset_all))
        .route("/events", get(handlers::events))
        .route("/qr", get(handlers::qr))
        .with_state(state);

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
    println!("  ├─ listening    http://{bind}");
    println!("  ├─ admin app    http://{bind}/admin.html");
    println!("  ├─ customer app http://{bind}/");
    println!(
        "  ├─ public URL   {}",
        if public_url.is_empty() { "(admin origin)".into() } else { public_url }
    );
    println!("  ├─ static dir   {public_dir}");
    match &admin_token {
        Some(_) => println!("  └─ operator auth ENABLED (ADMIN_TOKEN set)\n"),
        None => println!("  └─ operator auth DISABLED — set ADMIN_TOKEN before going live\n"),
    }

    axum::serve(listener, app).await?;
    Ok(())
}
