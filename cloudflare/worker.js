// Cloudflare Worker: serve the two single-file apps from the edge and proxy
// /api/* (including the SSE stream) to the inline backend (BACKEND_URL).
// See CLOUDFLARE.md.

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (url.pathname.startsWith("/api/")) {
      if (!env.BACKEND_URL) {
        return new Response("BACKEND_URL is not configured", { status: 500 });
      }
      const backend = new URL(env.BACKEND_URL);
      url.protocol = backend.protocol;
      url.hostname = backend.hostname;
      url.port = backend.port;

      // Returning fetch() directly streams the response, so SSE flows through.
      const proxied = new Request(url.toString(), request);
      proxied.headers.set("host", backend.host);
      return fetch(proxied);
    }

    return env.ASSETS.fetch(request);
  },
};
