# Customizing & extending inline

inline is built to be hacked on. This guide covers the common things you'll
want to change — from colours to swapping out the whole storage layer.

- [1. Restyle the apps](#1-restyle-the-apps)
- [2. Change wording](#2-change-wording)
- [3. Change the queue (types & fields)](#3-change-the-queue-types--fields)
- [4. Add your own components](#4-add-your-own-components)
- [5. Host the customer app separately](#5-host-the-customer-app-separately)
- [6. Swap the message broker (scale out)](#6-swap-the-message-broker-scale-out)
- [7. Storage backends](#7-storage-backends)
- [8. Production hardening](#8-production-hardening)
- [9. Customer notifications](#9-customer-notifications)

---

## 1. Restyle the apps

Each app is a single HTML file with a **theme block at the very top**. Open
`public/index.html` (customer) or `public/admin.html` (operator) and edit the
`:root { … }` variables. No build step — save and refresh.

```css
:root {
  --bg:           #0f172a;   /* page background        */
  --card:         #ffffff;   /* the ticket card        */
  --primary:      #6366f1;   /* brand / accent colour  */
  --radius:       22px;      /* corner roundness       */
  --font: system-ui, sans-serif;
  --bg-image:     none;      /* see below              */
}
```

**Background image** (customer app): paste a URL into `--bg-image`:

```css
--bg-image: url("https://images.example.com/restaurant.jpg");
```

**Logo / extra branding:** add any HTML inside the `.brand` block near the top
of the `<body>` in `public/index.html`, e.g.:

```html
<img src="/logo.png" alt="logo" style="height:48px;margin-bottom:12px;" />
```

(Drop `logo.png` into the `public/` folder so it's served alongside the apps.)

---

## 2. Change wording

The customer app keeps all its phrases in one small object near the bottom of
`public/index.html`:

```js
const TEXT = {
  next:    "You're next — get ready!",
  ahead:   (n) => `${n} ${n === 1 ? "group is" : "groups are"} ahead of you`,
  turn:    "It's your turn — please come to the counter",
  done:    "Thank you! Your visit is complete.",
  skipped: "Your number was skipped — please see the staff.",
  no_show: "Marked as no-show — please see the staff.",
};
```

Translate or reword these freely (great place to localize to your language).
The brand name and tagline come from `config.json` instead.

---

## 3. Change the queue (types & fields)

You almost never need to touch HTML for this — edit **`config.json`**. Both
apps rebuild their queue columns and their "add guest" form from it.

**Add a queue type** (gets its own running number + label prefix):

```jsonc
{ "code": "VIP", "name": "VIP", "description": "Members only" }
// → produces tickets VIP01, VIP02, …
```

**Add a form field.** Supported `type`s: `text`, `tel`, `number`, `email`,
`textarea`, `select`.

```jsonc
{ "key": "name",     "label": "Name",       "type": "text",   "required": true },
{ "key": "party",    "label": "Party size", "type": "number" },
{ "key": "seating",  "label": "Seating",    "type": "select",
  "options": ["Indoor", "Outdoor", "Bar"] }
```

Notes:
- `key` is the storage key (keep it stable; it's what's saved on each guest).
- The **first field** is used as the title in the operator's queue list, so put
  the most identifying field (usually name) first.
- Under Docker, `config.json` is mounted — edit and refresh, no rebuild.
  Running bare, restart the server to pick up changes.

---

## 4. Add your own components

Because each app is plain HTML/CSS/JS with no framework, you can drop in
anything: a clock, a logo carousel, ads on the customer screen, an extra stat,
etc.

The apps expose small render functions you can hook into. For example, to show
an extra stat on the customer card, add an element in the markup and set it
inside `render(v)` in `index.html`:

```js
function render(v) {
  /* …existing code… */
  document.getElementById("myStat").textContent = v.total_waiting;
}
```

The data available to the customer app (`/api/entries/:id`) is:

```json
{ "id", "label", "type_code", "type_name", "status",
  "ahead", "current_serving", "total_waiting", "created_at" }
```

---

## 5. Host the customer app separately

By default the backend serves both apps, so they share an origin and need no
extra config. If you'd rather host `index.html` on a CDN / different domain:

1. Near the top of the `<script>` in `index.html`, point it at the backend:

   ```js
   const API_BASE = "https://queue.example.com";
   ```

2. Set `INLINE_PUBLIC_URL` in `.env` to that customer app's URL so the QR codes
   and links the operator hands out point to the right place.

3. CORS is already permissive by default (`CorsLayer::permissive()` in
   `src/main.rs`), so cross-origin calls work out of the box. For production,
   restrict it to your domains — see [§8](#8-production-hardening).

---

## 6. Swap the message broker (scale out)

The default broker is an **in-process** `tokio::broadcast` channel
(`src/broker.rs`). It's perfect for a single instance. If you run **multiple
backend replicas** behind a load balancer, a guest connected to replica A won't
hear about a change made on replica B — so move the pub/sub to a shared bus.

The whole codebase only uses three methods — `publish`, `subscribe`, `new` — so
you only edit `src/broker.rs`. Sketch with Redis Pub/Sub:

```rust
// publish(): redis_conn.publish("inline", message).await;
// subscribe(): subscribe to the "inline" channel and forward messages into a
//              local channel that the SSE handler reads, exactly as today.
```

NATS or Postgres `LISTEN/NOTIFY` work the same way. Nothing in `handlers.rs`,
the SSE endpoint, or the front-ends needs to change.

---

## 7. Storage backends

By default the queue is persisted to a JSON file (`INLINE_DATA_FILE`) — no setup,
ideal for a single site. To use a database instead, set `INLINE_STORAGE` and
build with the matching feature:

| `INLINE_STORAGE` | Build with                                  | `INLINE_DATABASE_URL` example               |
|------------------|---------------------------------------------|---------------------------------------------|
| `json` (default) | —                                           | (uses `INLINE_DATA_FILE`)                   |
| `sqlite`         | `cargo build --release --features sqlite`   | `sqlite:inline.db?mode=rwc`                 |
| `postgres`       | `cargo build --release --features postgres` | `postgres://user:pass@host:5432/inline`     |
| `mongo`          | `cargo build --release --features mongo`    | `mongodb://host:27017` (+ `INLINE_DB_NAME`) |

Each backend stores the whole queue as a single JSON document and loads it on
startup, so the data model is identical everywhere and the default binary stays
dependency-free. The selection lives in `src/storage.rs`.

**Add another backend** (Redis, S3, …): implement `load`/`save` for a new
variant of the `Storage` enum in `src/storage.rs` and handle it in `from_env`.
Nothing in `store.rs` or the handlers changes.

**Want normalized SQL tables** (to query individual entries in Postgres)?
Replace the document `load`/`save` in `SqlBackend` with per-entry rows; `Entry`
already derives `Serialize`/`Deserialize`, so mapping is straightforward.

---

## 8. Production hardening

**Set an operator token.** In `.env`:

```env
ADMIN_TOKEN=a-long-random-string
```

Without it, anyone can add guests and call next. The admin app will prompt for
it and remember it in the browser.

**Terminate TLS with a reverse proxy.** inline speaks plain HTTP; put Caddy,
nginx, or Traefik in front. Caddy example (`Caddyfile`):

```
queue.example.com {
    reverse_proxy localhost:8080
}
```

Caddy handles HTTPS automatically and proxies SSE fine with no extra config.

**If you use nginx, disable buffering for the event stream**, or live updates
will be delayed/batched:

```nginx
location /api/events {
    proxy_pass http://localhost:8080;
    proxy_set_header Connection '';
    proxy_http_version 1.1;
    proxy_buffering off;          # critical for SSE
    proxy_read_timeout 1h;
}
location / {
    proxy_pass http://localhost:8080;
}
```

**Tighten CORS.** In `src/main.rs`, replace `CorsLayer::permissive()` with an
explicit allow-list:

```rust
use axum::http::{HeaderValue, Method};
use tower_http::cors::CorsLayer;

let cors = CorsLayer::new()
    .allow_origin("https://queue.example.com".parse::<HeaderValue>().unwrap())
    .allow_methods([Method::GET, Method::POST]);
```

(If the backend serves both apps from one origin — the default — you can drop
CORS entirely.)

**Back up `data.json`** (or your DB) if the live queue matters to you.

---

## 9. Customer notifications

The customer app can pop a browser notification when a guest is **next** and
when it's **their turn** — works in Chrome, Edge, Firefox, and Android Chrome
(which needs the included Service Worker, `public/sw.js`, already wired up).

For the guest:
- A **"Notify me when it's my turn"** button appears on their ticket.
- Tapping it asks for permission; once granted, the choice is remembered.
- Alerts are driven by the existing SSE stream — no polling.

**Requirements**
- A **secure context**: notifications + service workers only work over
  **HTTPS**, or `http://localhost` for local testing. Plain `http://<LAN-IP>`
  will silently not offer notifications — front it with TLS (see §8).
- The alert fires while the ticket page is open (including a backgrounded tab
  on desktop). Mobile browsers may suspend a fully backgrounded tab, so
  delivery there is best-effort.

**Alerts when the app is completely closed?**
That needs **Web Push** (VAPID): generate VAPID keys, subscribe the browser
(`registration.pushManager.subscribe`), store the subscription server-side, and
POST pushes to it. The `push` handler in `public/sw.js` is already in place —
you only add the server-side sender. Optional, beyond the default setup.

To reword the notifications, edit `NOTIFY_TEXT` near the top of the `<script>`
in `public/index.html`.
