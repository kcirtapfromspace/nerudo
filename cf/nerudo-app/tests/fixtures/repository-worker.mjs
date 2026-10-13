/** Read-only local service fixture. It never creates or modifies a real repository. */
const commit = "0000000000000000000000000000000000000001";
const trunk = "nerudo-ux-fixture";
const source = `import { Hono } from "hono";

// Keep each request easy to understand.
const app = new Hono();

app.get("/", (context) => {
  return context.json({ message: "Good work takes form.", ready: true });
});

export default app;
`;
const readme = `# A considered workspace

This is a **read-only local UX fixture**, used to verify repository navigation, README rendering, and code readability. It is not a hosted project.

## Start with the project

An introduction belongs beside the files it explains. Read the context, inspect the [source](src/index.ts), then choose the next step.

\`\`\`ts
import { Hono } from "hono";

const app = new Hono();
app.get("/", (context) => context.json({ ready: true }));

export default app;
\`\`\`

## Quick reference

| Surface | Purpose |
| --- | --- |
| Code | Understand the project and read its files |
| Graph | Follow contributions and their order |
| Pull requests | Examine the approaches |

- [x] Read the introduction
- [ ] Examine the source

## Setup

\`\`\`sh
npm install
npm run dev
\`\`\`

[Return to the introduction](#start-with-the-project)
`;
const files = { "README.md": readme, "src/index.ts": source, "package.json": '{\n  "name": "local-ux-fixture",\n  "private": true\n}\n' };
const json = (data, status = 200) => new Response(JSON.stringify(data), { status, headers: { "Content-Type": "application/json", "Cache-Control": "no-store" } });
export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/v1/hub/repos/nerudo/readme-preview") return request.method === "GET" ? json({ org: "nerudo", repo: "readme-preview", trunk, description: "Read-only local UX fixture", upstream: null, public: true, created_ms: 0, summary: null }) : json({ error: "Read-only fixture" }, 405);
    if (!url.pathname.startsWith(`/t/${trunk}/api/`)) return env.REAL_NERUDO.fetch(request);
    if (request.method !== "GET") return json({ error: "Read-only fixture" }, 405);
    const method = url.pathname.split("/").at(-1);
    if (method === "state") return json({ trunk, branch: "refs/heads/main", head: commit, seq: 0, me: null, people: [], teams: [], agents: [], stats: {}, pending: [], landings: [], ejected: [], tasks: [], needs_me: null, events: [], releases: [] });
    if (method === "branches") return json({ branch: "refs/heads/main", head: commit, seq: 0, forks: [] });
    if (method === "history") return json({ path: url.searchParams.get("path"), landings: [], more: false, next_before: 0 });
    if (method === "tree") {
      const path = url.searchParams.get("path") || "";
      if (Object.hasOwn(files, path)) return json({ kind: "file", path, text: files[path], size: new TextEncoder().encode(files[path]).length, commit, at: null });
      const entries = path === "" ? [{ name: "src", dir: true }, { name: "README.md", dir: false }, { name: "package.json", dir: false }] : path === "src" ? [{ name: "index.ts", dir: false }] : null;
      return entries ? json({ kind: "dir", path, entries, commit, at: null }) : json({ error: "no such path" }, 404);
    }
    return json({ error: "Not available in this fixture" }, 404);
  },
};
