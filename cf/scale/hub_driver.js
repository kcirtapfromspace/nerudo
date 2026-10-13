// Load driver for cf/scale/hub.md. It runs inside Cloudflare, so its load does not pass the
// edge's per-IP protection, which blocks one computer at about 2000 requests/s. Each request
// to the driver makes `n` calls at once to the Worker under test, through a service binding
// (TARGET), and answers with their status codes, their times and a few error bodies.
//
// Bindings: TARGET (service), DRIVER_KEY and AGENT (secrets: the driver's own key, and the
// target's agent token), REPOS (var: JSON [{name, trunk}]), ORG (var).
//
//   GET /?key=K&path=base|a|r|b|c|d|e&n=40

export default {
  async fetch(req, env) {
    const u = new URL(req.url);
    if (!env.DRIVER_KEY || u.searchParams.get("key") !== env.DRIVER_KEY) {
      return new Response("forbidden\n", { status: 403 });
    }
    const path = u.searchParams.get("path");
    const n = Math.min(Math.max(parseInt(u.searchParams.get("n") || "10", 10) || 10, 1), 100);
    const repos = JSON.parse(env.REPOS);
    // The same paths as hub_load.py, each call on the next of the 8 repos.
    const start = Math.floor(Math.random() * repos.length);
    const paths = [];
    for (let i = 0; i < n; i++) {
      const r = repos[(start + i) % repos.length];
      const p = {
        base: "/", a: `/v1/repos/${env.ORG}/nope/stats`, b: `/v1/trunks/${r.trunk}/stats`, c: `/v1/repos/${r.name}/stats`,
        // A new unknown name for each call.
        r: `/v1/repos/${env.ORG}/nope-${Date.now()}-${Math.floor(Math.random() * 1e9)}/stats`,
        // The web app's pages of a repo, by name and by trunk id.
        d: `/v1/hub/repos/${r.name}`, e: `/v1/hub/trunks/${r.trunk}`,
      }[path];
      if (!p) return new Response("path: base, a, r, b, c, d or e\n", { status: 400 });
      paths.push(p);
    }
    // The app paths have no key, as a page view of someone not signed in.
    const headers = path === "d" || path === "e" ? { "user-agent": "nerudo-scale-driver/0.1" }
      : { authorization: `Bearer ${env.AGENT}`, "user-agent": "nerudo-scale-driver/0.1" };
    const out = await Promise.all(paths.map(async (p) => {
      const t = Date.now();
      try {
        const r = await env.TARGET.fetch(`https://target.internal${p}`, { headers });
        const body = await r.text();
        return { s: r.status, ms: Date.now() - t, body: r.status === 200 ? "" : body.slice(0, 300) };
      } catch (e) {
        // The target threw: the call did not get an HTTP answer.
        return { s: 0, ms: Date.now() - t, body: String((e && e.message) || e).slice(0, 300) };
      }
    }));
    const codes = {};
    for (const o of out) codes[o.s] = (codes[o.s] || 0) + 1;
    return Response.json({
      codes,
      ms: out.map((o) => o.ms),
      errors: out.filter((o) => o.body).slice(0, 2).map((o) => [o.s, o.body]),
    });
  },
};
