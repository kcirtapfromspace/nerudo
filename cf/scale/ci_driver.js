// A driver Worker for cf/scale/ci.md (ci_latency.py): the latency of a type check through the
// CI service binding against the size of the project; whether checks sent at the same time run
// at the same time; and whether a check runs while the invocation waits on I/O.
//
// Deploy it only for the measurement, with a service binding CI to a copy of nerudo-ci and a
// secret DRIVER_TOKEN, and delete it after. POST /measure with the bearer token:
//   {noop: true}                          answer at once (the round trip)
//   {files, checks, parallel, bad}        `checks` checks of a project with `files` modules,
//                                         one after the other or all at once
//   {files, wait_ms}                      one check while a timer of wait_ms runs (files -1:
//                                         the timer only)

function project(n, bad) {
  const files = {
    "tsconfig.json": JSON.stringify({ compilerOptions: { strict: true, target: "es2022", module: "es2022", lib: ["es2022"], skipLibCheck: true }, include: ["src"] }),
    "src/price.ts": "export function price(cents: number): string {\n  return (cents / 100).toFixed(2);\n}\n",
    "src/index.ts": "import { price } from './price';\nexport const total = price(250);\n",
  };
  for (let i = 0; i < n; i++) {
    const prev = i === 0 ? "" : `import { Item${i - 1}, total${i - 1} } from './m${i - 1}';\n`;
    const field = i === 0 ? "" : `  prev: Item${i - 1};\n`;
    const sum = i === 0 ? "0" : `total${i - 1}(items.map((x) => x.prev))`;
    files[`src/mod/m${i}.ts`] =
      `import { price } from '../price';\n${prev}` +
      `export interface Item${i} {\n  id: number;\n  name: string;\n  cents: number;\n  tags: string[];\n${field}}\n` +
      `export class Cart${i} {\n  private items: Item${i}[] = [];\n  add(item: Item${i}): this {\n    this.items.push(item);\n    return this;\n  }\n` +
      `  label(): string {\n    return this.items.map((x) => \`\${x.name}: \${price(x.cents)}\`).join(', ');\n  }\n}\n` +
      `export function total${i}(items: Item${i}[]): number {\n  return items.reduce((s, x) => s + x.cents, 0) + ${sum};\n}\n` +
      `export const tags${i} = (items: Item${i}[]): Set<string> => new Set(items.flatMap((x) => x.tags));\n`;
  }
  if (bad) files["src/bad.ts"] = "export const n: number = 'one';\n";
  return files;
}

async function check(env, body) {
  const t0 = Date.now();
  const resp = await env.CI.fetch("https://nerudo-ci/check", { method: "POST", headers: { "content-type": "application/json" }, body });
  const out = await resp.json();
  return { ms: Date.now() - t0, status: resp.status, outcome: out.outcome, errors: out.errors };
}

export default {
  async fetch(request, env) {
    if (request.headers.get("authorization") !== `Bearer ${env.DRIVER_TOKEN}`) return new Response("no", { status: 403 });
    const { files = 10, checks = 1, parallel = false, bad = false, noop = false, wait_ms = 0 } = await request.json();
    if (noop) return Response.json({ noop: true });
    if (wait_ms) {
      // A wait on I/O (a timer) and, with files >= 0, one check at the same time.
      const io = new Promise((r) => setTimeout(r, wait_ms));
      const body = JSON.stringify({ project: "tsconfig.json", files: project(files, bad) });
      const jobs = [io];
      if (files >= 0) jobs.push(check(env, body));
      await Promise.all(jobs);
      return Response.json({ ok: true });
    }
    const body = JSON.stringify({ project: "tsconfig.json", files: project(files, bad) });
    let out;
    if (parallel) {
      out = await Promise.all(Array.from({ length: checks }, () => check(env, body)));
    } else {
      out = [];
      for (let i = 0; i < checks; i++) out.push(await check(env, body));
    }
    return Response.json({ files, body_bytes: body.length, outcomes: out.map((c) => c.outcome), inner_ms: out.map((c) => c.ms) });
  },
};
