import app from "vinext/server/fetch-handler";

export * from "vinext/server/fetch-handler";

// git over HTTPS: /{org}/{repo}[.git]/info/refs, /git-upload-pack and /git-receive-pack.
const GIT = /^\/[^/]+\/[^/]+\/(info\/refs|git-upload-pack|git-receive-pack)$/;

// Headers every page and API answer carries. The share and personal keys travel in the URL
// (?k=), so no referrer leaves this site; nothing here is meant to be framed or sniffed.
const HEADERS: Record<string, string> = {
  "referrer-policy": "no-referrer",
  "x-content-type-options": "nosniff",
  "x-frame-options": "DENY",
  "strict-transport-security": "max-age=31536000",
};

/** The web app's Worker. git clients talk to the same origin as the browser: their
 *  requests go to the sequencer as they came (method, headers, body), before the page
 *  router sees them, and the answer streams back untouched. Clones are big. */
export default {
  ...app,
  async fetch(request: Request, env: Env, ctx: ExecutionContext) {
    if (GIT.test(new URL(request.url).pathname)) return env.NERUDO.fetch(request);
    const response = await app.fetch(request, env, ctx);
    const out = new Response(response.body, response);
    for (const [k, v] of Object.entries(HEADERS)) if (!out.headers.has(k)) out.headers.set(k, v);
    // Pages, RSC payloads and APIs can contain private repo metadata. Never reuse them
    // across viewers, even when the request had no cookie. Static assets keep their cache.
    const path = new URL(request.url).pathname;
    if (!path.startsWith("/assets/") && !path.startsWith("/_next/static/")) {
      out.headers.set("cache-control", "private, no-store");
      const vary = out.headers.get("vary");
      out.headers.set("vary", [vary, "Cookie"].filter(Boolean).join(", "));
    }
    return out;
  },
} satisfies ExportedHandler<Env>;
