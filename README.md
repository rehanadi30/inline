# inline

A tiny, self-hostable **queue system** with live updates. One small Rust
binary serves a JSON API, a real-time event stream, and two
**single-file web apps**:

- **Admin / operator app** — add guests with a flexible form, hand them a
  link or QR code (tap the QR to enlarge it for easy scanning), and run the
  line (call next, skip, recall…).
- **Customer app** — the guest sees their number, who's being served now, and
  how many are ahead — updating **live**, with no manual refresh. Guests can
  opt in to a **browser notification** for when it's their turn.

Both apps are **mobile-friendly** (responsive, touch-sized, notch-safe).

It also includes opt-in **browser notifications** (when it's your turn),
**ticket expiry** (links auto-expire after a configurable time, default 1 day),
one-click **backup & restore** from the admin, a **history** of called numbers
with times, **offline-tolerant** apps (service worker + health ping), and
**Cloudflare** deployment options.

It supports **multiple queue types** (e.g. `A` for 1–2 guests, `B` for 3–5),
each with its own running number — so you get tickets like `A02` and `B07`
depending on which kind of table is free.

> Designed to be easy to read, easy to host, and easy to restyle: the whole
> backend is a few hundred lines of Rust, and each app is a single HTML file
> you can edit by hand.

---

## How it works

```
       Operator (admin.html)                 Guest (index.html)
            │  add / next / skip                   │  watch my number
            ▼  REST + Bearer token                 ▼  REST (read-only)
   ┌────────────────────────────────────────────────────────────────┐
   │                  inline backend  (Rust + axum)                 │
   │     REST API   ·   QR code (SVG)   ·   serves both HTML apps   │
   │                                                                │
   │     in-memory store  ──persist──►  data.json (survives restart)│
   │            │ on every change                                   │
   │            ▼                                                   │
   │     message broker (pub/sub)  ──push──►  SSE  /api/events  ────┼──► phones
   └────────────────────────────────────────────────────────────────┘
```

### Live updates without polling or WebSockets

The customer app does **not** poll on a timer, and it does **not** open a
two-way WebSocket. Instead:

1. Anything that changes the queue publishes a tiny message to an in-process
   **broker** (a pub/sub channel).
2. Browsers hold a single, lightweight **Server-Sent Events** (SSE) stream at
   `GET /api/events`. SSE is one-way (server → client) plain HTTP, so it's far
   lighter than a WebSocket and auto-reconnects on its own.
3. When a browser gets the "something changed" nudge, it re-fetches just the
   small bit of data it's allowed to see. The nudge itself carries no personal
   data.

Want to run multiple backend instances behind a load balancer? Swap the
in-process broker for Redis Pub/Sub or NATS — see [CUSTOMIZE.md](CUSTOMIZE.md).
Everything else stays the same.

---

## Quick start (Docker — recommended)

You only need Docker. No Rust toolchain required.

```bash
git clone https://github.com/rehanadi30/inline inline
cd inline

cp .env.example .env        # then edit ADMIN_TOKEN and INLINE_PUBLIC_URL
docker compose up -d --build
```

Then open:

- **Operator console:** http://localhost:8080/admin.html
- **Customer view:**    http://localhost:8080/

> The first build downloads and compiles the Rust dependencies, so it can take
> a few minutes. Subsequent builds are cached and fast.

The `docker-compose.yml` mounts `./public`, `./config.json`, and `./data` from
the host, so you can **edit the HTML or the queue config and just refresh** —
no rebuild — and your queue state persists in `./data/data.json`.

### Run without Docker (needs Rust)

```bash
cp .env.example .env
cargo run            # http://localhost:8080
```

Install Rust from <https://rustup.rs> if you don't have it.

### Deploy on Cloudflare

Put inline on a global HTTPS edge — via **Cloudflare Tunnel** (no open ports) or
a **Cloudflare Worker** that serves the apps and proxies the API. HTTPS also
unlocks the browser-notifications feature. See [CLOUDFLARE.md](CLOUDFLARE.md).

---

## Configuration

### `.env` — deployment settings

| Variable             | Default            | Description                                                                 |
|----------------------|--------------------|-----------------------------------------------------------------------------|
| `INLINE_BIND`        | `0.0.0.0:8080`     | Address/port the server binds to.                                           |
| `INLINE_PUBLIC_URL`  | *(empty)*          | Public base URL of the **customer** app, used to build links + QR codes. e.g. `https://queue.example.com`. Empty = use the admin's current origin. |
| `ADMIN_TOKEN`        | *(empty)*          | Operator password. **Empty disables auth** (demo only). Set it before going live. |
| `INLINE_DATA_FILE`   | `data.json`        | Where queue state is snapshotted.                                           |
| `INLINE_CONFIG`      | `config.json`      | Path to the queue-definition file (below).                                  |
| `INLINE_TICKET_TTL`  | `1d`               | How long a ticket/link stays valid (`30m`, `12h`, `1d`, bare seconds, or `off`). After it, the customer sees "expired" and it won't be called. |
| `INLINE_STORAGE`     | `json`             | Storage backend: `json`, `sqlite`, `postgres`, or `mongo` (see [Storage backends](#storage-backends)). |
| `INLINE_DATABASE_URL`| *(empty)*          | Connection string for the `sqlite` / `postgres` / `mongo` backends.         |
| `INLINE_PUBLIC_DIR`  | `public`           | Folder containing `index.html` and `admin.html`.                            |

### `config.json` — what your queue looks like

This is the file a business edits. It defines the brand, the **queue types**,
and the **fields** on the operator's "add guest" form. Both apps read it at
runtime, so changes need only a restart (or just a refresh under Docker, since
it's mounted).

```jsonc
{
  "brand": "inline",
  "tagline": "Please wait for your number",

  // Each type has its own running number. "code" is the label prefix:
  // code "A" → A01, A02, A03 …   Add or remove types freely.
  "queue_types": [
    { "code": "A", "name": "Small table",  "description": "1–2 guests" },
    { "code": "B", "name": "Medium table", "description": "3–5 guests" },
    { "code": "C", "name": "Large table",  "description": "6+ guests" }
  ],

  // The "add guest" form builds itself from this list.
  // type can be: text | tel | number | email | textarea | select
  "fields": [
    { "key": "name",  "label": "Name",         "type": "text",     "required": true  },
    { "key": "phone", "label": "Phone number", "type": "tel",      "required": false },
    { "key": "notes", "label": "Notes",        "type": "textarea", "required": false }
  ]
}
```

That covers your example directly: with types `A` (1–2) and `B` (3–5), the
operator picks the type that matches the free table and the guest gets `A02`
or `B07` accordingly.

### Storage backends

By default inline stores everything in a JSON file — zero setup, perfect for a
single site. You can point it at a database instead; the other backends are
optional Cargo features, so the default binary stays small.

| Backend   | `INLINE_STORAGE` | Build with                                  |
|-----------|------------------|---------------------------------------------|
| JSON file | `json` (default) | — (always available)                        |
| SQLite    | `sqlite`         | `cargo build --release --features sqlite`   |
| Postgres  | `postgres`       | `cargo build --release --features postgres` |
| MongoDB   | `mongo`          | `cargo build --release --features mongo`    |

Set the connection string in `INLINE_DATABASE_URL` (e.g.
`postgres://user:pass@host:5432/inline`, `sqlite:inline.db?mode=rwc`, or
`mongodb://host:27017` with `INLINE_DB_NAME`). Each backend stores the queue as a
single JSON document. With Docker, enable a backend at build time with
`--build-arg FEATURES=postgres`. See [CUSTOMIZE.md](CUSTOMIZE.md#7-storage-backends).

---

## Using it

1. **Operator** opens `/admin.html`, picks a queue type, fills in the guest's
   details, and clicks **Add to queue**.
2. inline shows the new ticket label (e.g. `A02`) plus a **link and QR code**.
   The operator shares either with the guest.
3. The **guest** opens the link / scans the QR and watches their position
   update live.
4. The operator uses **Call next**, **Serve**, **Skip**, **Recall**, **Done**
   to run the line. Every action instantly updates all customer screens.

---

## API reference

Public (no auth):

| Method | Path                 | Description                                          |
|--------|----------------------|------------------------------------------------------|
| `GET`  | `/api/config`        | Branding + queue types + form fields.                |
| `GET`  | `/api/state`         | Public "now serving" board for every type.           |
| `GET`  | `/api/entries/:id`   | One guest's own status (no personal data).            |
| `GET`  | `/api/events`        | **SSE** live-update stream.                           |
| `GET`  | `/api/qr?data=...`   | QR code (SVG) for any text/URL.                       |
| `GET`  | `/api/health`        | Liveness check (the customer app pings this).        |

Operator (require `Authorization: Bearer <ADMIN_TOKEN>` when a token is set):

| Method | Path                        | Description                                   |
|--------|-----------------------------|-----------------------------------------------|
| `GET`  | `/api/entries`              | Full list incl. entered details.              |
| `POST` | `/api/entries`              | Add a guest. Body: `{ type_code, fields }`.   |
| `POST` | `/api/entries/:id/status`   | Set status. Body: `{ status }`.               |
| `POST` | `/api/queue/:code/next`     | Finish current + call next in a type.         |
| `POST` | `/api/queue/:code/reset`    | Clear one queue type and reset its counter.   |
| `POST` | `/api/reset`                | Clear everything.                             |
| `GET`  | `/api/admin/export`         | Download a full JSON backup.                  |
| `POST` | `/api/admin/import`         | Restore from a backup (replaces all data).    |

`status` is one of: `waiting`, `serving`, `done`, `skipped`, `no_show`.

---

## Project structure

```
inline/
├── src/
│   ├── main.rs       # config from env, router, static serving, startup
│   ├── config.rs     # loads config.json (queue types + fields + brand)
│   ├── store.rs      # in-memory state, queue logic, JSON persistence
│   ├── broker.rs     # pub/sub broker behind the SSE stream
│   └── handlers.rs   # the HTTP/JSON/SSE/QR handlers
├── public/
│   ├── index.html    # CUSTOMER app  (single file, themeable)
│   ├── admin.html    # ADMIN app     (single file, themeable)
│   └── sw.js         # service worker (notifications + offline)
├── config.json       # your queue definition
├── cloudflare/       # Cloudflare Worker (edge apps + API proxy) + wrangler.toml
├── .env.example      # deployment settings
├── Dockerfile
├── docker-compose.yml
├── README.md
├── CUSTOMIZE.md      # theming + extending guide
├── CLOUDFLARE.md     # deploy via Cloudflare Tunnel / Worker
└── AGENTS.md         # context for AI coding agents
```

---

## Security notes

- **Always set `ADMIN_TOKEN`** before exposing inline publicly, or anyone can
  run your queue. With no token, operator endpoints are open.
- Customer links use an unguessable id and only ever reveal that one guest's
  position — never other guests' details.
- Put inline behind a TLS-terminating reverse proxy (Caddy, nginx, Traefik) in
  production. See [CUSTOMIZE.md](CUSTOMIZE.md#production-hardening).
- CORS is permissive by default for easy setup; tighten it if the customer app
  is served from the same origin (the default).

---

## License

MIT — see [LICENSE](LICENSE). This is an open-source project; contributions and
forks are welcome.
