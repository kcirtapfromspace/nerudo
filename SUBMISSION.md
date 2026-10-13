# NERUDO: a Git platform for concurrent coding agents

NERUDO gives each coding agent its own Artifacts fork and one authoritative integration
path to main. A per-repository Durable Object batches disjoint changes, checks the
integrated tree, returns actionable receipts, preserves agent context, and asks people
for signatures at governance boundaries. People can compare competing task attempts
and sign a release. The public source is developed through the same platform.

## Submission

- Competition: [Cloudflare Git competition](https://www.cloudflare.com/git-competition/).
- Deadline: **October 14, 2026, 23:59 PDT**; **October 15, 00:59 in Denver**.
- Public app: [NERUDO](https://nerudo-web.patrickdeutsch.workers.dev/).
- Public demo: [nerudo/demo](https://nerudo-web.patrickdeutsch.workers.dev/nerudo/demo).
- Frozen GitHub source: [kcirtapfromspace/nerudo](https://github.com/kcirtapfromspace/nerudo).
- Native development source: [nerudo/nerudo](https://nerudo-web.patrickdeutsch.workers.dev/nerudo/nerudo).
- License: [MIT OR Apache-2.0](LICENSE).
- Measurements: [BENCHMARKS.md](BENCHMARKS.md).
- Recorded coding-model workflows: [cf/LLM_WORKFLOWS.md](cf/LLM_WORKFLOWS.md).
- Demo video: **nerudo-submission-visuals.mp4**, verified and ready for manual upload; [artifact/QA evidence](cf/evidence/submission-video-20261013.json), [recorded plan](cf/evidence/submission-video-plan-20261013.json).

The public deployment and source were verified on October 13; exact versions and Git
commits are in [the deployment manifest](cf/evidence/submission-deployment-20261013.json).
Production backend `a1cbe028-4447-41e2-9bc8-377bdca25415` and frontend
`ff8d53e4-35f4-47ac-a1bc-7bcaffdd38a5` were built from source
`5a74c0100dbba28dcd64c07ca98aa88adf191f78`. Its deployed WASM
`f62080b3935398b6b7eb0503ae55d58db2f03b7dcec66be154f13cda0986ec1f`
matches the merged-source targeted fault run. Prior deployment versions remain in the manifest.
The published native source landed as `27bbc6a` from merged input `10996af`; that merge
changes backend comments and scenario coverage, with no backend runtime delta. Historical
scale experiments and the isolated soak/provider runs retain their separate build scope.
**The selected video is `nerudo-submission-visuals.mp4`: 8:30.68, 31.02 MiB,
1600 × 1000 at 25 fps, H.264/AAC. It is ready for manual upload.** Its SHA-256 is
`9bd70ee26aa9cc15aa178ac1d2024c9d96e893c9b7235bb736d8c274b7abd0c5`.
It uses the approved Luna female neural narration from Deepgram via Cloudflare
Workers AI (`@cf/deepgram/aura-2-en`). Fresh product views, highlights, arrows and
explicitly labelled explanations follow the narration cues. Setup and view-readiness
intervals are cut from the picture; narration keeps its original pace with no blank
padding. The 105 captions use the known script aligned to ASR word timestamps.

Final QA passed full audio/video decode, all 54 annotation cues and eight critical
first-ready frames. Eleven measured visual anchors establish the raw origin used by
69 ready cuts; 78 preparation intervals are removed. The earlier end-duration timing
guard failed, and the repaired export uses this independently validated calibration.
One renderer replaces the complete 115-pixel caption footer; product geometry above
it is preserved, subject to encoding. Encoded audio measures −16.22 LUFS and
−1.87 dBTP; its longest detected pause is 0.717333 seconds, with no gaps of 1.5 seconds
or more. These are objective media checks, not a direct-listening claim or an
interaction-latency/capacity result.

The [video record](cf/evidence/submission-video-20261013.json) binds the final file,
QA and separate capture/export source hashes. Both earlier editions are superseded;
submit only `nerudo-submission-visuals.mp4`. The actual provider screens show preserved
initial receipts on the isolated build; their current sidebar includes the later
separate Hermes landing. The immutable first-attempt record retains its original
two-landed/two-completed outcome. `public_video_url` remains `null`; the entrant must
manually upload and submit the verified local artifact.

The public demo has 21 landed changes, 19 batches and 22 CI runs from **six scripted
clients**. It includes CI repair PR 10, signed sensitive PR 16, three pinned plans for
task T-5, and signed release v0.1.0. Its main is
`168de019714737f9b57fd537a9702b2a6a13165d`; independent read-only verification passed.
The [anonymous browser smoke record](cf/evidence/submission-public-smoke-20261013.json)
identifies the exact deployment it tested. This is a route smoke check, not a load test or SLA.

## Why this fits the challenge

The unit of collaboration is an agent attempt with a task, commit, context and receipt.
A central sequencer replaces every agent repeatedly racing to update main. Append rules,
derived files and landing-time allocation handle common agent collision patterns. Real
integrated TypeScript checks catch stale callers even when their individual bases pass.
Signed approvals and releases keep human authority bound to exact commits and rules.

Cloudflare hosts the web application and Rust/WASM sequencer in Workers. Durable Objects
serialize each repository's integration and persist its queue, receipts and recovery state.
Artifacts supplies Git repositories, isolated forks and compare-and-swap pushes. A service
binding runs ts-rust in a CI Worker. Workers AI, including Clef, is an optional decision
provider. The scale experiments use separate Workers and Artifacts namespaces.

## What the evidence supports

| Claim | Recorded evidence | Bound |
|---|---|---|
| Distributed integration | 1,000 repos, 299,988/300,000 landings over five minutes; measured window 994.3/s, p95 980 ms | Synthetic disjoint commits, no CI, one run; 12 push failures |
| Repository breadth | 10,000 quiet repos, approximately 500 aggregate landings/s for two minutes | One synthetic agent per repo, one change each 20 s; not 10,000 simultaneous LLMs |
| Peak throughput | 1,978/s on 2,000 repos during the healthy 30–120 s window | Run stopped at 131 s during Artifacts failures; not sustained capacity |
| One-repo batching | 512 prepared changes in 12.01 s at cap 64 | Four forks, no CI, no conflicts; generation/setup excluded |
| Large repository handling | 728 MB tip lands; read-back and fsck pass; reported 14 MB WASM peak | WASM linear memory, not complete isolate RAM; not arbitrary monorepo CI |
| Real agent interoperability | Three overlapping Claude/Codex/Antigravity or Claude/Codex/Hermes workflows; each produced three verified landings | Historical Oct 12 runs; three separate batches, artificial first-submission barrier |
| Fresh provider attempt | [Three overlapping Claude/Codex/Hermes clients](cf/evidence/submission-provider-attempt-20261013.json); Claude/Codex completed and landed, Hermes timed out after 900 s | 2/3 completion. Eleven external runtime tests and verify passed; full fixture acceptance failed because receipt implementation was missing. Any recovery is separate |
| Actual dependency repair | [Codex TS2554 → read Claude pinned context → revision 2 landed](cf/evidence/submission-coordination-20261013.json); all three public receipt/context reads returned HTTP 200 | Observed within the partial initial run on its isolated backend; not a scripted client or capacity result |
| Separate Hermes recovery | [One bounded 600 s recovery](cf/evidence/submission-providers-20261013.json) landed the third change; final tree passes 16 external runtime tests, full acceptance, three context pins and exactly-once/log verification | Native Hermes parent still timed out. Three eventual landings, two completed workflows; original concurrent attempt remains 2/3 |
| Recovery and failure handling | Recorded deadline/backoff build: 100/100 local fault checks; later branches build summary: 102/102 | Named historical builds; current source and deployment need their own verification |
| Candidate recovery checks | [102/102 strict local checks](cf/evidence/submission-faults-20261013.json), then [4/4 final recovery scenarios](cf/evidence/submission-recovery-final-20261013.json) after the allowance replay-key correction | Full run was on the preceding WASM; final run was targeted. Both are local fault tests, not deployed Artifacts load |
| Merged-source recovery | [4/4 targeted scenarios](cf/evidence/submission-recovery-merged-20261013.json) at `5a74c01` with working-tree changes, WASM `f62080b3`; includes upstream approver-rename recovery | Tested WASM matches final production binary; scenarios ran locally, not as a full merged-build suite or deployed fault test |
| Fresh contention soak | [Eight scripted clients for 600.009 s](cf/evidence/submission-soak-20261013.json): 61/80 logical changes landed, 133 revisions, 71 conflict and one real TypeScript rejection; exactly-once Git/log audit and runtime oracle passed | 19 changes stopped at the three-revision retry bound. Isolated build `79c259ad`, allowances disabled, external runtime oracle; not capacity or complete task success |

The incumbent experiment compares configured direct-push and ordinary PR merge policies.
It does not test GitHub native merge queue, GitLab or Bitbucket. Its fixed simulated CI
and omitted agent pretests are explicit. NERUDO has not demonstrated 100,000 simultaneous
coding agents on one repository, sustained conflicts plus general runtime CI, or a
production service-level agreement. The Worker checks an integrated batch tip; do not
claim every intermediate commit is buildable without separately proving that guarantee.

## Reproduce

```sh
git clone --branch competition-submission-2026-10-13 https://github.com/kcirtapfromspace/nerudo.git nerudo
cd nerudo
cargo test --manifest-path cf/Cargo.toml --workspace --locked
cargo check --manifest-path cf/Cargo.toml -p nerudo-worker \
  --target wasm32-unknown-unknown --locked
python3 -B -m unittest discover -s cf/scripts -p 'test_llm_*.py'
```

Use [cf/README.md](cf/README.md#try-it) for the three-Worker deployment and CLI demo;
Workers Paid and Artifacts access are required. Use new credentials and an isolated
namespace for reproduction. [The local Nix fault harness](cf/faults/README.md) needs no
Cloudflare credentials. The [scale documents](cf/scale/README.md) give their drivers and
fixture methods; [CI reproducibility](cf/scale/ci.md#files) includes the recovered drivers.
Do not rerun scale load against the public app. Put credentials in private files or
secret bindings; never include personal-link query keys in the video or packet.

Artifacts billing begins **on October 14, 2026**: published rates include $0.15 per
1,000 operations and $0.50 per GB-month. The historical $19–20 campaign estimate excludes
then-free Artifacts. Its $135–225 projection per 300,000 changes is an operation-counting
range, before repository/fork storage and production CI/inference. See
[the cost scope](BENCHMARKS.md#limits-and-cost) and
[official pricing](https://developers.cloudflare.com/artifacts/platform/pricing/).

## Copy-ready project fields

Use [the official submission form](https://www.cloudflare.com/git-competition/submit/).
Project fields below are ready to copy; the entrant supplies the human team/contact fields.

**Project name:** NERUDO

**Project vision:**

NERUDO rethinks the Git forge for people working with many coding agents. Each agent works in an isolated Git fork and submits an attempt with code, a task, preserved reasoning and an actionable receipt. A repository sequencer batches compatible changes and checks their integrated tree, catching dependency failures that an agent's own checkout can miss.

People can compare competing plans, choose an approach, inspect rejected revisions and require signatures for sensitive changes and releases. Agents can read another agent's pinned context and repair their work without losing the original intent. Ordinary Git, a CLI, MCP and a browser connect this collaboration to existing workflows.

Our vision is a platform where more parallel effort produces useful, auditable progress. The working forge develops its own source. Its six-client scripted demo illustrates the product, while recorded Claude/Codex workflows demonstrate real dependency repair. We publish measurements with their workload conditions, failures and reproduction steps.

**How we used Cloudflare:**

Cloudflare provides NERUDO's execution, coordination and Git storage. Workers host the browser application, API and Rust sequencer compiled to WebAssembly. A Durable Object per repository is the authoritative integration coordinator: it serializes submissions, batches compatible changes and persists queues, receipts, rules and crash-recovery intent. A separate Durable Object maintains the forge's directory.

Artifacts stores the Git repositories and isolated agent forks. Compare-and-swap pushes let the sequencer advance main against an exact expected head. Service bindings connect the web application to the backend and the sequencer to a dedicated CI Worker, which runs the ts-rust TypeScript checker in WebAssembly against the integrated batch tip.

The application supports optional Workers AI decision providers, including Clef, to request review or approval within repository rules. We also used Workers AI to generate the demo narration with Deepgram Aura-2 Luna. Benchmarks and coding-provider experiments use separate Workers and Artifacts namespaces, keeping their workloads isolated from the public forge.

**Open source repository URL:** https://github.com/kcirtapfromspace/nerudo

**Frozen release:** [competition-submission-2026-10-13](https://github.com/kcirtapfromspace/nerudo/releases/tag/competition-submission-2026-10-13). See [FREEZE.md](FREEZE.md) for provenance.

**Instructions to run the project:** Clone the public repository with the `.git` URL below.
Run the Rust workspace tests and the offline helper tests. For a Cloudflare deployment,
follow `cf/README.md` → “Try it” to deploy the sequencer, CI and web Workers with your own
Workers Paid/Artifacts access, secret bindings and namespace. For credential-free local
fault tests, install Nix with flakes and run `cd cf && nix run .#faults`.

```sh
git clone --branch competition-submission-2026-10-13 https://github.com/kcirtapfromspace/nerudo.git nerudo
cd nerudo
cargo test --manifest-path cf/Cargo.toml --workspace --locked
python3 -B -m unittest discover -s cf/scripts -p 'test_llm_*.py'
```

The required demo is 5–10 minutes in MP4, WebM or MOV, at most 2 GiB.
[Official Rules, section 4](https://www.cloudflare.com/documents/build-next-gen-git-platform-competition-terms.pdf)
prohibit automated entry; the entrant manually reviews the form, uploads the final video
and submits it. This preparation leaves competition entry to the entrant.

## Narration-aligned demo storyboard (recorded 8:30.68)

| Time | Show | Evidence the viewer should see |
|---|---|---|
| 0:00–0:56.56 | Agent list, main graph and clone command | Point to a01–a06, the latest merged batch and ordinary Git clone URL; labelled cards explain forks and the integration contract |
| 0:56.56–1:51.04 | Shared API code, rejected receipt and repaired revision | Show the two actual code shapes, PR 10's TS2554 receipt, its currency-argument repair, revision history and pinned context |
| 1:51.04–2:43.08 | T-5 trial verdicts, diffs and pinned plans | Point to the three attempts, trial checks, src/format.ts alternatives, context pin and recorded a04fmt pick |
| 2:43.08–3:43.08 | Approval rows and release signature | Point to PR 16's required approval, signer and exact submitted commit; show v0.1.0 and its Signature tab |
| 3:43.08–4:49.68 | Preserved provider receipts and separate outcomes | Point to actual isolated Codex rejection and pinned repair plan, Claude child evidence and the initial audit; show historical and separate Hermes recovery records |
| 4:49.68–6:09.76 | Benchmark rows, totals and failure conditions | Point to the 1,000/10,000/2,000-repo rows, attempted/landed totals, no-CI scope and the 131-second Artifacts failure |
| 6:09.76–7:19.00 | Large-repo memory row, components and recovery evidence | Point to the 728 MB/14 MB row and WASM-memory cell, Components table and local 4/4 targeted recovery record |
| 7:19.00–8:30.68 | Run commands, cost scope, limits and license | Point to Reproduce commands, isolated credentials, October 14 billing estimates, unproven capacity/CI/SLA bounds and the license |

The recording keeps the immutable first provider attempt distinct from the
separate recovery. A recording does not establish throughput; use the scoped benchmark
evidence for that claim. Manually upload `nerudo-submission-visuals.mp4`, the verified selected artifact in the video record, through the official form.
The deployment manifest records the verified source/version and anonymous public demo;
the video metadata records the verified local file, without claiming a public upload.
