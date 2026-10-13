// nerudo-ci: the type check that a trunk asks for with `[ci] typecheck` in nerudo.toml.
// It runs tsc from ts-rust (a Rust port of the TypeScript 7 compiler, built as
// WebAssembly) on files in memory: no container, no file system. The Trunk Durable Object
// calls it through a service binding with the files of one tree.
//
// POST {project: "tsconfig.json", files: {"src/a.ts": "..."}}  (paths relative to the repo)
// -> {outcome: "passed" | "failed", exit_code, errors, diagnostics: [{file, line, code, text}]}
// -> {outcome: "crashed", error, stderr}  when the checker itself fails (trap, stack, memory)

import wasm from "../vendor/ts-rust/ts_rust.wasm";
import { memoryFileSystem, runTsc } from "../vendor/ts-rust/core.js";

const ROOT = "/repo";
const MAX_DIAGNOSTICS = 20;

export default {
    async fetch(request) {
        if (request.method !== "POST") {
            return new Response("POST {project, files}", { status: 405 });
        }
        const { project, files } = await request.json().catch(() => ({}));
        if (typeof project !== "string" || typeof files !== "object" || files === null) {
            return new Response("project and files are required", { status: 400 });
        }
        return Response.json(check(project, files));
    },
};

// runTsc is synchronous and makes a fresh instance, so two checks never share state and
// never overlap in one isolate.
function check(project, files) {
    const fs = memoryFileSystem(Object.fromEntries(Object.entries(files).map(([path, text]) => [`${ROOT}/${path}`, text])));
    let result;
    try {
        result = runTsc(wasm, {
            args: ["-p", `${ROOT}/${project}`, "--noEmit"],
            cwd: ROOT,
            fs,
            diagnosticsJson: true,
            env: { NO_COLOR: "1" },
        });
    } catch (error) {
        return { outcome: "crashed", error: String(error), stderr: String(error.stderr ?? "").slice(-2000) };
    }
    const errors = (result.diagnostics ?? []).filter(d => d.category === 1);
    return {
        outcome: result.exitCode === 0 ? "passed" : "failed",
        exit_code: result.exitCode,
        errors: errors.length,
        diagnostics: errors.slice(0, MAX_DIAGNOSTICS).map(d => ({
            file: d.fileName?.startsWith(`${ROOT}/`) ? d.fileName.slice(ROOT.length + 1) : (d.fileName ?? null),
            line: d.startPosition ? d.startPosition.line + 1 : null,
            code: `${d.source ?? "TS"}${d.code}`,
            text: d.text,
        })),
    };
}
