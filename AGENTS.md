# AGENTS.md — context for AI coding agents

Dense, accurate map of this repo so an agent can clone, understand, run, and
modify **inline** without reading every file. Humans: see [README.md](README.md).

## What this is
`inline` = a self-hostable **queue/waitlist system**. One small Rust binary
serves a JSON API, a live-update stream (SSE), and two static single-file web
apps. Pluggable storage (in-memory state persisted to a JSON file by default;
SQLite/Postgres/Mongo optional). No frontend build step (plain HTML/CSS/JS).

## Architecture
Operator uses **admin app** → REST (Bearer token) mutates the in-memory
`Store` → every mutation is published to a persister (the chosen storage
backend) and to an in-process **broker** (tokio broadcast) → fanned out to
browsers over **SSE**
(`GET /api/events`) → each browser refetches the small bit it may see. Guests
use the **customer app** (read-only, opaque per-ticket id, optional browser
notifications). Multiple **queue types** (A, B, …) each have their own running
number → labels like `A02`, `B07`.

## File map
```
src/main.rs       env/config load, router, static serving, startup banner, AppState
src/config.rs     loads config.json (queue_types + fields + brand); has defaults
src/store.rs      Store/Entry/Status/Snapshot, queue logic, expiry, export/import
src/storage.rs    storage connector: JSON (default), sqlite/postgres/mongo (features)
src/handlers.rs   all HTTP handlers (JSON + SSE + QR), auth helper
src/broker.rs     pub/sub behind SSE (swap for Redis/NATS to scale out)
public/index.html CUSTOMER app  (themeable :root vars; notifications; offline)
public/admin.html ADMIN app     (flexible form; QR lightbox; history; backup)
public/sw.js      service worker: notifications + offline caching
config.json       queue definition (edit this, not the HTML)
cloudflare/       Worker (edge static + API proxy) + wrangler.toml  (see CLOUDFLARE.md)
Dockerfile, docker-compose.yml
```

## Run it
- **Docker (no Rust needed):** `cp .env.example .env` → `docker compose up -d --build` → http://localhost:8080 (customer), `/admin.html` (operator).
- **Native:** needs Rust. `cargo run`. Release standalone binary: `cargo build --release` → `target/release/inline[.exe]` + copy `public/` and `config.json` beside it.
- **Windows + GNU toolchain gotcha:** `windows-gnu` needs MinGW-w64 on PATH
  (`dlltool`/`as`/`ld`); install via MSYS2 (`pacman -S mingw-w64-x86_64-gcc`)
  and add `C:\msys64\mingw64\bin` to PATH, or use the MSVC toolchain
  (`rustup default stable-msvc`) which needs VS C++ Build Tools. The produced
  exe is self-contained (only Windows system DLLs).
- **Verify without local Rust:** `docker run --rm -v "$PWD":/app -w /app rust:1-slim cargo check`.

## Config / env (all optional)
`.env`: `INLINE_BIND` (0.0.0.0:8080), `INLINE_PUBLIC_URL` (base URL for links/QR),
`ADMIN_TOKEN` (operator password; empty = auth OFF), `INLINE_DATA_FILE`
(data.json), `INLINE_CONFIG` (config.json), `INLINE_TICKET_TTL` (default `1d`;
accepts `30m`/`12h`/`1d`/bare seconds/`off`), `INLINE_STORAGE`
(json|sqlite|postgres|mongo; default json), `INLINE_DATABASE_URL` (DB backends),
`INLINE_PUBLIC_DIR` (public).
`config.json`: `brand`, `tagline`, `queue_types[{code,name,description}]`,
`fields[{key,label,type,required,options}]` (type: text|tel|number|email|textarea|select).

## API contract
Public: `GET /api/config`, `GET /api/state`, `GET /api/entries/:id` (no PII;
includes `expired`), `GET /api/events` (SSE; payload is just `{"type":"update"}`),
`GET /api/qr?data=...` (SVG), `GET /api/health`.
Operator (needs `Authorization: Bearer <ADMIN_TOKEN>` when set):
`GET /api/entries` (full), `POST /api/entries` `{type_code,fields}`,
`POST /api/entries/:id/status` `{status}`, `POST /api/queue/:code/next`,
`POST /api/queue/:code/reset`, `POST /api/reset`,
`GET /api/admin/export` (download backup), `POST /api/admin/import` (raw backup JSON body).
`status` ∈ `waiting|serving|done|skipped|no_show`.

## Conventions & invariants
- Persistence is a seam: backends live in `src/storage.rs` (a `Storage` enum
  with `load`/`save`); `Store` only emits `Snapshot`s. Add a backend there, not
  in `store.rs` (see CUSTOMIZE.md §7). Keep `Store` method signatures stable.
- The SSE payload carries **no PII** — it's only a nudge; clients refetch.
- Customer endpoints must never leak other guests' fields. `public_view` is the
  only customer projection; keep it minimal.
- Frontend has **no build step / no deps**. Don't add a bundler or npm to the
  apps. Theme via `:root` CSS vars; wording via the `TEXT`/`NOTIFY_TEXT` objects.
- Notifications + service worker require a **secure context** (HTTPS or
  `http://localhost`).

## Common tasks (where to touch)
- Add a queue type / form field → `config.json` only.
- Change behavior of "call next"/skip/expiry → `src/store.rs`.
- Add an endpoint → `src/handlers.rs` + register in `src/main.rs` router.
- Restyle / reword apps → `public/*.html` (`:root`, `TEXT`).
- Add/choose a storage backend → `src/storage.rs`; build DB features with
  `--features sqlite|postgres|mongo` (default build is JSON-only, no extra deps).
- Scale to multiple instances → replace broker internals in `src/broker.rs`
  (Redis/NATS); nothing else changes.

## After changes — verify
`cargo build` (or the docker `cargo check` above). For the apps, `node --check`
the inline `<script>` and `public/sw.js`. There are no unit tests; verify the
API by curling the endpoints in the table above.
