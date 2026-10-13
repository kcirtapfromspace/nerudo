# Real coding-model workflow evidence

The October 12, 2026 recordings below used three real, overlapping coding-model
sessions against one shared repository. They complement the synthetic load drivers
in [BENCHMARKS.md](../BENCHMARKS.md); they do not establish server saturation or
100,000-agent capacity. The fresh October 13 partial attempt is reported separately below.

These recordings predate the public NERUDO rename and the later revision, security,
recovery and scale work. Original repository names, model IDs, commit/context hashes,
dates, receipts and verifier output are preserved in the JSON. They are historical
observations, not fresh verification of the current source or deployment. The original
fixture URLs may now require authorized access; use the JSON evidence and the current
public demo linked in [SUBMISSION.md](../SUBMISSION.md).

| Recorded run | Completed providers | Three-session overlap | Landings / batches | Independent checks |
|---|---|---|---|---|
| `20261012-054654` | Claude Code, Codex, Antigravity | 381.749 s | 3 / 3 | 16 Node runtime tests, trusted final acceptance, matching fetched/live heads, context pins and hash-chain verification |
| `20261012-070045` | Claude Code, Codex, Hermes | 376.543 s | 3 / 3 | 13 Node runtime tests, trusted final acceptance, matching heads, context pins and hash-chain verification |

- [Claude/Codex/Antigravity evidence](evidence/llm-concurrency-20261012.json)
- [Claude/Codex/Hermes evidence](evidence/hermes-concurrency-20261012.json)
- [Unsuccessful Ollama attempts](evidence/ollama-attempts-20261012.json)
- [Import provenance and SHA-256 checksums](evidence/workflow-provenance-20261013.json)

Each primary session had an independent Git clone and sequencer identity. The
sequencer alone wrote main. Agents implemented changes, tested locally, submitted
commits and repaired integrated type-check failures using actual receipts and pinned
context. No fabricated patch, model response or successful landing is counted.

The first-submission barrier is artificial. Claude and Codex reached timeout before
all first candidates were ready, so the workflows overlapped but did not land together
in one batch. Model preload/setup are outside the workflow intervals. The final runtime
oracle runs locally against an independently fetched main; it is not a runtime test
suite enforced by the Worker. The Worker enforces integrated TypeScript checking.

Native delegation is reported only when observed. Claude executed two native children.
Codex's native spawn attempts failed with missing-parent-thread errors. Hermes completed
a planner and no-op probe; its requested reviewer did not execute after its two-child
budget was consumed. No separate Antigravity child execution was attested. The JSON
records this rather than treating configuration as evidence of delegation.

The Ollama 1.7B primary reached its 60-turn limit; the separate 3B attempt reached an
output-token limit before writing or landing. Neither is included in the successful
three-provider result. Gemini was not authenticated and is not counted. Codex and
Antigravity model IDs were not emitted in the successful transcripts; their configured
defaults must not be presented as observed model IDs.

## Fresh initial attempt (2026-10-13): partial

The [immutable first-attempt record](evidence/submission-provider-attempt-20261013.json)
reports run `20261013-151647` on the isolated submission backend
`79c259ad-080f-4f14-9fa1-18ec64830ab3`. Claude, Codex and Hermes overlapped, but only
**two of three workflows completed and landed**: Claude and Codex. Hermes reached its
900-second timeout before submitting a change. This is a partial result.

Independent read-only verification found two landings exactly once, matching live/fetched
heads, two immutable context pins and an intact log hash chain. Eleven external Node
runtime tests passed. **Full fixture acceptance failed because `src/receipt.ts` was
missing.** Passing those tests does not mean the complete requested application was done.

The [coordination trace](evidence/submission-coordination-20261013.json) captures an actual
dependency repair: after Claude made `price` require a currency argument, Codex revision 1
failed integrated CI with TS2554. Codex read Claude's immutable pinned context at
15:22:06 UTC and repaired revision 2 landed at 15:22:59. Independent public reads of
revision 1, that context and revision 2 all returned HTTP 200; the context copy is pinned
at `acef3c4016a313546b45b5026dc8cb0b83ff8764`. This is observed coordination in the partial
initial run, separate from the later Hermes recovery.

Claude's stream reported `claude-opus-5-5` and attested two completed native Agent
children, a planner and reviewer. Codex emitted neither a model identity nor evidence of
native child execution. Configuration and self-description are not child-execution proof.
The record retains transcript SHA-256 values and observed/requested model distinctions.

The [public fixture](https://nerudo-submission-web.patrickdeutsch.workers.dev/nerudo/concurrency-20261013-151647)
and any later repair can change after this initial snapshot. A separately bounded Hermes
recovery is a new attempt; it cannot turn the initial concurrent run into three completed
workflows. The initial artificial landing barrier, external runtime oracle and separate
isolated build remain part of the scope. Allowances were disabled, and this backend
predates the final allowance replay-key correction.

## Separate bounded Hermes recovery (2026-10-13)

The [final aggregate record](evidence/submission-providers-20261013.json) preserves the
initial attempt and adds one separate 600-second Hermes recovery. **The change landed,
but the native parent timed out again.** Final main is
`981d878484aa8d5363624603b2e9300713159234`: three changes landed exactly once, all three
context links resolve to immutable copies, the log verifies, 16 external runtime tests
pass and full fixture acceptance passes. This completes the application tree; it leaves
only two completed native workflows and does not satisfy the recorder's strict
all-clients-completed-and-concurrent condition.

Hermes emitted `Qwen3.8-Flash-Next-UD-IQ4_XS`. Its recovery stream attests two completed
native delegated children: inspect/plan and inspect/review/correctness. Their results
declare `completed` and `is_error=false`, but each summary is truncated at 5,003
characters. The initial timed-out attempt attests one attempted delegation and no
completed child result. Codex child execution/model identity remain unattested.
The record includes stream checksums and keeps these attempts separate. No further
provider runs were made; the task-owned local supervisor was stopped with configuration
and API identity unchanged.

## Inspect without credentials or inference

From a clean checkout:

```sh
python3 -B -m unittest discover -s cf/scripts -p 'test_llm_*.py'
python3 -B cf/scripts/llm_concurrency.py --help
python3 -B cf/scripts/test_llm_fixture.py
```

The unit suite uses mocked replies or temporary fake native clients. It proves helper
behavior, redaction and accounting; it does not perform inference or validate a live
landing. The fixture's final acceptance is designed for the completed price/cart/receipt
implementation, so the initial fixture does not yet pass that final oracle.

## Reproduce on an isolated deployment

The imported helper is the existing recorder adapted to NERUDO CLI, environment and
configuration names. The historical measurements above were not rerun by that adaptation.
Its mutating commands accept only a dedicated `nerudo-evidence-*` or `nerudo-scale-*`
Worker on `workers.dev`. It does not use production vars by default.

1. Follow [the deployment instructions](README.md#try-it) with three throwaway Workers,
   separate Durable Object storage and a separate Artifacts namespace. Bind the test CI
   Worker to the test sequencer. Do not send load to the public submission deployment.
2. Build `cargo build --manifest-path cf/Cargo.toml -p nerudo-cli`. The helper uses
   `NERUDO_CLI` when set, otherwise `CARGO_TARGET_DIR/debug/nerudo`, otherwise
   `cf/target/debug/nerudo`. For a release binary, set `NERUDO_CLI` to its absolute path.
3. Prepare a private vars file outside Git with `NERUDO_WORKER`, `NERUDO_ADMIN_TOKEN`
   and `NERUDO_AGENT_TOKEN` for that deployment; protect it with mode `0600`. The API
   and git routes must be reachable on that Worker URL. Do not place a token on a shell
   command line, in a prompt, or in a published manifest.
4. Authenticate at least three supported native clients with your existing authorized
   accounts. The default is Claude Code, Codex and Antigravity. Hermes and Ollama also
   require their configured local inference server; Gemini requires Google authentication.
5. Check readiness, then explicitly start the workflow:

```sh
python3 -B cf/scripts/llm_concurrency.py doctor
python3 -B cf/scripts/llm_concurrency.py run --vars /absolute/private/evidence.vars \
  --providers claude,codex,antigravity
```

`doctor` can query configured local model endpoints for readiness; it does not generate
code. `run` invokes paid or local inference, creates a dedicated **public fixture repo**,
and permits model edits only in its isolated clones. Use synthetic source with no private
project data. Every provider receives agent authority, while initialization uses admin
authority. A signed-in maintainer/key is needed for the setup's tasks and approver entry;
the original helper uses the fixture maintainer `kcirtapfromspace`.

The recorder keeps owner-only files under ignored `runs/llm-concurrency/`: manifest,
independent workspaces, events, native output and evidence. Keep manifests, tokens,
private keys and client authentication files private. Review redacted outputs before
publishing anything; only the sanitized portable summary belongs in `cf/evidence/`.

A retry is a new model session. Do not count a late retry or a completed feature as a
new concurrent run. The final evidence requires three completed overlapping workflows,
landed receipts, runtime tests, trusted acceptance, external verification and matching
heads. Use a fresh fixture for a new measurement and retain failures alongside successes.
