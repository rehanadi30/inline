//! HTTP handlers: the JSON API, the SSE stream, and the QR generator. Public
//! endpoints are unauthenticated; operator endpoints require the admin token
//! when one is configured. See README.md for the full endpoint table.

use crate::store::Status;
use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use qrcode::QrCode;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

/// Reusable result type: success carries JSON, failure carries a ready Response.
type ApiResult = Result<Json<Value>, Response>;

/// Reject the request unless it carries the operator token. When no
/// `ADMIN_TOKEN` is configured, auth is disabled and everything passes.
fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    let Some(expected) = &state.admin_token else {
        return Ok(());
    };
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| headers.get("x-admin-token").and_then(|v| v.to_str().ok()));

    if provided == Some(expected.as_str()) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "invalid or missing admin token").into_response())
    }
}

/// Tell every connected browser "something changed; refresh your view".
fn notify(state: &AppState) {
    state.broker.publish(r#"{"type":"update"}"#);
}

// Public endpoints (no auth).

/// Branding + queue definition, consumed by both the admin and customer apps.
pub async fn get_config(State(state): State<AppState>) -> Json<Value> {
    // Serialize the shared Config by reference, then attach a couple of
    // runtime-only fields.
    let mut value = serde_json::to_value(&*state.config).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("public_url".into(), json!(state.public_url));
        map.insert("auth_required".into(), json!(state.admin_token.is_some()));
        map.insert("ticket_ttl".into(), json!(state.ticket_ttl_secs));
    }
    Json(value)
}

/// The public "now serving" board for every queue type.
pub async fn get_state(State(state): State<AppState>) -> Json<Value> {
    let store = state.store.read().await;
    Json(json!({ "state": store.state(&state.config) }))
}

/// A single guest's own status — safe to expose, contains no personal data.
pub async fn get_entry(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let store = state.store.read().await;
    match store.public_view(&id, &state.config, state.ticket_ttl_secs) {
        Some(view) => Ok(Json(serde_json::to_value(view).unwrap_or(Value::Null))),
        None => Err((StatusCode::NOT_FOUND, "queue entry not found").into_response()),
    }
}

/// The SSE stream. One lightweight, auto-reconnecting HTTP connection per
/// browser; we push a tiny "update" nudge whenever the queue changes.
pub async fn events(State(state): State<AppState>) -> impl IntoResponse {
    let rx = state.broker.subscribe();
    let stream = BroadcastStream::new(rx)
        .filter_map(|res| res.ok())
        .map(|msg| Ok::<Event, Infallible>(Event::default().data(msg)));

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(25))
            .text("keep-alive"),
    )
}

#[derive(Deserialize)]
pub struct QrQuery {
    data: String,
}

/// Render any text/URL as a QR code (SVG). Used by the admin app to show a
/// scannable code for each guest's personal link.
pub async fn qr(Query(q): Query<QrQuery>) -> Response {
    match QrCode::new(q.data.as_bytes()) {
        Ok(code) => {
            let svg = code
                .render::<qrcode::render::svg::Color>()
                .min_dimensions(220, 220)
                .quiet_zone(true)
                .build();
            ([(header::CONTENT_TYPE, "image/svg+xml")], svg).into_response()
        }
        Err(_) => (StatusCode::BAD_REQUEST, "could not encode QR").into_response(),
    }
}

// Operator endpoints (require the admin token when configured).

/// Full list of guests, including the details the operator entered. Protected.
pub async fn list_entries(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    authorize(&state, &headers)?;
    let store = state.store.read().await;
    Ok(Json(json!({
        "entries": serde_json::to_value(&store.entries).unwrap_or(Value::Null),
        "state": store.state(&state.config),
    })))
}

#[derive(Deserialize)]
pub struct CreateReq {
    type_code: String,
    #[serde(default)]
    fields: Map<String, Value>,
}

/// Add a guest to a queue type. Returns the new entry plus the link/QR the
/// operator hands to the guest.
pub async fn create_entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateReq>,
) -> ApiResult {
    authorize(&state, &headers)?;

    if !state.config.has_type(&req.type_code) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unknown queue type '{}'", req.type_code),
        )
            .into_response());
    }

    let entry = {
        let mut store = state.store.write().await;
        store.create(&req.type_code, req.fields)
    };
    notify(&state);

    let path = format!("/?id={}", entry.id);
    let customer_url = if state.public_url.is_empty() {
        Value::Null
    } else {
        json!(format!("{}{}", state.public_url.trim_end_matches('/'), path))
    };

    Ok(Json(json!({
        "entry": entry,
        "customer_path": path,
        "customer_url": customer_url,
    })))
}

#[derive(Deserialize)]
pub struct StatusReq {
    status: Status,
}

/// Set a specific guest's status (skip, recall to waiting, serve, mark done…).
pub async fn set_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<StatusReq>,
) -> ApiResult {
    authorize(&state, &headers)?;
    let changed = {
        let mut store = state.store.write().await;
        store.set_status(&id, req.status)
    };
    if !changed {
        return Err((StatusCode::NOT_FOUND, "queue entry not found").into_response());
    }
    notify(&state);
    Ok(Json(json!({ "ok": true })))
}

/// Finish whoever is being served in this type and call the next guest.
pub async fn next_queue(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> ApiResult {
    authorize(&state, &headers)?;
    if !state.config.has_type(&code) {
        return Err((StatusCode::BAD_REQUEST, "unknown queue type").into_response());
    }
    let called = {
        let mut store = state.store.write().await;
        store.call_next(&code, state.ticket_ttl_secs)
    };
    notify(&state);
    Ok(Json(json!({ "called": called })))
}

/// Clear a single queue type and reset its counter (e.g. start a new day).
pub async fn reset_type(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> ApiResult {
    authorize(&state, &headers)?;
    {
        let mut store = state.store.write().await;
        store.reset(Some(&code));
    }
    notify(&state);
    Ok(Json(json!({ "ok": true })))
}

/// Clear every queue type.
pub async fn reset_all(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    authorize(&state, &headers)?;
    {
        let mut store = state.store.write().await;
        store.reset(None);
    }
    notify(&state);
    Ok(Json(json!({ "ok": true })))
}

/// Cheap liveness check — used by the customer app to detect server downtime.
pub async fn health() -> Json<Value> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Json(json!({ "ok": true, "time": now }))
}

/// Download the full queue state as a JSON backup file. Protected.
pub async fn export_data(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = authorize(&state, &headers) {
        return resp;
    }
    let body = {
        let store = state.store.read().await;
        store.export_json()
    };
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"inline-backup.json\""),
        ],
        body,
    )
        .into_response()
}

/// Restore the queue state from a backup file (replaces everything). Protected.
/// Body is the raw JSON produced by `export_data`.
pub async fn import_data(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> ApiResult {
    authorize(&state, &headers)?;
    {
        let mut store = state.store.write().await;
        store
            .import_json(&body)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid backup: {e}")).into_response())?;
    }
    notify(&state);
    Ok(Json(json!({ "ok": true })))
}
