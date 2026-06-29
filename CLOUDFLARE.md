# Running inline with Cloudflare

inline's backend is a tiny stateful Rust server (it holds the live queue and
streams updates over SSE), so it isn't a pure serverless function. There are
two clean, well-supported ways to put it on Cloudflare — pick one.

> Both give you **HTTPS**, which the browser-notifications feature requires.

---

## Option A — Cloudflare Tunnel (simplest, recommended)

Expose your self-hosted inline (Docker or the native binary) to the internet
over HTTPS **without opening any ports or having a public IP**. The Rust server
keeps serving everything (apps + API + SSE); Cloudflare just provides the
secure public hostname.

1. Run inline locally (e.g. `docker compose up -d`, listening on `:8080`).
2. Install `cloudflared` and authenticate:
   ```bash
   cloudflared tunnel login
   cloudflared tunnel create inline
   ```
3. Route a hostname to the tunnel and point it at your local server:
   ```bash
   cloudflared tunnel route dns inline queue.example.com
   cloudflared tunnel run --url http://localhost:8080 inline
   ```
   (Or add an ingress rule in `~/.cloudflared/config.yml`.)
4. Set `INLINE_PUBLIC_URL=https://queue.example.com` in your `.env` so the
   links/QR codes the operator hands out point to the public hostname.

That's it — `https://queue.example.com/` is the customer app and
`/admin.html` is the operator console, globally reachable and on HTTPS.

**Quick test in Docker Compose** — add a sidecar (needs a tunnel token):
```yaml
  cloudflared:
    image: cloudflare/cloudflared:latest
    command: tunnel --no-autoupdate run --token ${CF_TUNNEL_TOKEN}
    restart: unless-stopped
    depends_on: [inline]
```

---

## Option B — Cloudflare Worker (edge apps + API proxy)

Serve the two HTML apps from Cloudflare's edge (global CDN) and proxy `/api/*`
(including the SSE stream) to your backend. Files live in [`cloudflare/`](cloudflare/):
`worker.js` + `wrangler.toml`.

You still need the Rust backend reachable over HTTPS — typically via **Option A**
(a Tunnel) — and you set that URL as `BACKEND_URL`.

```bash
npm install -g wrangler          # or: npx wrangler ...
cd cloudflare

# point the Worker at your backend (the tunnel hostname from Option A)
wrangler deploy --var BACKEND_URL:https://queue-backend.example.com

# local dev:
wrangler dev --var BACKEND_URL:https://queue-backend.example.com
```

`wrangler.toml` already binds the repo's `../public` folder as static assets,
so the Worker serves `index.html`, `admin.html`, and `sw.js`, and forwards the
API to your backend. SSE streams through because the Worker returns the proxied
`fetch()` response directly.

Set `INLINE_PUBLIC_URL` (on the backend) to the **Worker's** URL so generated
links/QRs resolve to the edge.

---

## What about a pure Workers (no backend) version?

Doing the whole thing on Workers means moving the live state and the SSE fan-out
into a **Durable Object** (and generating QR codes inside the Worker). That's a
separate implementation with different operational trade-offs (and Durable
Objects need a paid Workers plan). It's a great contribution if you want to
build it — the HTTP contract to mirror is documented in [AGENTS.md](AGENTS.md)
and the API table in [README.md](README.md). For most self-hosters, Option A or
B above is simpler and free.
