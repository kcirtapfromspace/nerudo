# Security audit, 2026-10-13

## Landed on nerudo/nerudo

The 16 fixes below landed on this repo on 2026-10-13 as PRs #90 to #105 from the agent
`security` (task T-44), one PR per fix, each ported from branch `security-audit` of the
GitHub repo (commit ids in the findings are that branch's) onto this lineage, where the
crates are `cf/nerudo-*` and the rules file is `nerudo.toml`. This report is the PR after #105.

| Finding | PR | Landing | Subject |
|---|---|---|---|
| F1 | #90 | 90 | a change that adds a path git refuses to check out is ejected as bad_path |
| F6 | #91 | 91 | the router gives no Durable Object to a trunk name that no trunk can have |
| F7 | #92 | 92 | a reaction names a comment that is there, by the id shape stamp makes |
| F5 (the way out) | #93 | 93 | the admin can close a queued PR while the trunk is paused |
| F2, F3 | #94 | 94 | the git engine refuses a pack or a tree that would take the object down |
| F8 | #95 | 95 | a page takes a key only in the shape the sequencer makes |
| F9, F10 | #96 | 96 | sign-in only from the link itself; security headers on every answer; decoding that cannot fail |
| F13 | #97 | 97 | seqd: a request names a revision, not an option; the bench's git config reaches seqd's pushes |
| O3 (part) | #98 | 98 | evidence and results: no home paths, scratchpad paths or this machine's LAN address |
| F14 | #99 | 99 | a batch recorded after a crash reloads the rules, as a batch recorded at once does |
| F15 | #100 | 100 | a sign-in with no key bound is only the approvers listed without a key |
| F5 | #101 | 101 | a type check that gives no answer three runs in a row is the tree's fault, and the culprit search ejects it |
| F17, F18 | #102 | 102 | bounds and bindings the authorization review asked for |
| F11, F12 | #103 | 103 | the MCP tools keep to their own repo and their own ids; keys are owner-only from the first byte |
| F4 | #104 | 104 | what one fetch, one pack, one object and one change may bring into the object |
| F16 | #105 | 105 | the rules file is a seal boundary whatever the governance globs say |

Three ports differ from the branch commits, on purpose: F16 treats both rules names
(`nerudo.toml` and `landseq.toml`) as governance; F10 keeps this lineage's `cache-control:
private, no-store` and `Vary: Cookie` beside the new headers; the session cookie's decoding
already could not fail here (`lib/session-cookie.ts`), so that part of F10 is not needed.

Not landed: the search time budget of branch `security-audit-search` (C1). This lineage
answers text search from a SQLite index (`trunk/search_index.rs`, at most 16 MiB of bodies per
query, no `ui_store` read), so the finding as written is gone. What remains open on search
is in C1 to C6 below. Landing on this repo changes the code, not the deployed Worker.

Every test passed on the tree as landed: the Rust workspace, the worker's wasm check, the
web app's type check, tests and build, and seqd.


Scope: the whole repository at `branches-fix` ff1320d (the next deploy candidate: freeze
5ed344d + the UI-store fix + the Clef decision provider), plus the uncommitted hub.rs/app.rs
patch that runs live. The product is `cf/`: the landseq Worker (Trunk and Hub Durable
Objects), the web app (`landseq-app`), the CI Worker, the pure-Rust git engine
(`landseq-git`), the sequencer logic (`landseq-core`) and the CLI/MCP server. `seqd/`,
`bench/`, scripts and configuration were reviewed too, since the repository goes public at
submission.

**Live** below means: in the Worker that serves landseq.patrickdeutsch.workers.dev now
(freeze 52f21d9 plus the uncommitted patch). Fixes are one commit each on branch
`security-audit` (16 commits, d58f032..fa09844), so the user can take each onto whichever
branch ships. All 16 cherry-pick onto `ship-d` (the deploy candidate) with one trivial
conflict (the scenarios script, where ship-d removed the Clef scenario); the result is branch
**`ship-d-security`** (43821e6 = ship-d + the 16 fixes), every test passing (129). The
search fixes for the Codex line are on branch **`security-audit-search`** (0a3a280, off
aca5114). Both branches are local to this machine. Nothing in this
audit was deployed, pushed, merged, sealed or signed. The work is task T-44 on landseq/landseq.

**Severity-ranked summary.** High: a path git refuses could land on trunk (F1); a malformed
pack (F2) or a deep tree (F3) aborted the Trunk object; one hanging type check wedged a trunk
for good (F5); an anonymous search held a public trunk's object for seconds (C1, Codex line).
Medium: no byte budget per change (F4); reflected markup through `?k=` on public trunks (F8);
crash recovery kept stale rules (F14); keyless accounts matched any same-named approver
(F15); `landseq.toml` was a seal boundary only by convention (F16); unbounded forks (F17);
`seqd` option injection and the dropped credential-helper override (F13, local tool). Open:
a performance scan on every `state` poll (O7), prompt text in the decision model (O6).

Method: the lead read every authorization path (lib.rs, hub.rs, trunk.rs dispatch, agents.rs,
git.rs, app.rs, ui.rs keys, repo.rs, social.rs, branches.rs, import.rs, host.rs, names.rs,
transport.rs, ci.rs, the web app's session, login, proxy and worker entry, the CI Worker) and
the landing path (merge.rs, gates.rs, store.rs diff/write_tree); four reviewers then read, in
parallel, (1) the git parsers and core merge, (2) Trunk/Hub authorization and governance, (3)
the two web surfaces and the CLI/MCP, (4) seqd, bench, scripts, configuration and the
pre-publication scan. `cargo audit` ran on both lockfiles, `npm audit` on the web app, and a
secrets grep over the tree and the whole history of every branch.

## Findings

(Severity: Critical, High, Medium, Low, Info. Status: Fixed = a commit on `security-audit`;
Open = recommended, not done; Accepted = design, documented.)

### Fixed on `security-audit`

**F1. High. A change could land a path git refuses to check out.** Live: yes.
`landseq-core/src/merge.rs` `changes()` took every tree entry of an agent's commit as it
came; nothing on the landing path checked entry names or modes (the only check,
`valid_path` in trunk.rs, guarded seeds). An agent commit with a `.git/hooks/post-checkout`
entry, a `..` component, an empty component or a non-file mode would land on trunk. git
refuses such a path at checkout, so every clone of trunk fails from that landing on; a client
without that check runs the hook. Found by the lead, confirmed by the parsers reviewer with a
test against the fix. Fix d58f032: `merge::path_problem` and a mode allow-list in `changes()`,
which footprint, edits, blobs_needed and apply_change all go through; `.git` is matched as
git's own `verify_path` matches it (any case, NTFS trailing dots and spaces, HFS+ ignorable
code points, the `git~1` short name); receipt kind `bad_path`. ac503c1 adds a path over 4096
bytes, a name over 255 bytes and a path deeper than 256 directories. Test:
`a_change_that_adds_a_path_git_refuses_to_check_out_is_ejected`.

**F2. High. One malformed packfile aborts the Trunk object.** Live: yes. Reachable by a
malicious or buggy git host, by the admin's import pack, and (depending on whether the
host re-packs what an agent pushes, open question) by an agent.
`landseq-git/src/pack.rs` indexed the bytes of a pack entry without bounds (a 33-byte
ref-delta pack panicked in a slice), allocated the buffer for an entry from a size the pack
declared before it saw the data (a 41-byte pack allocated 200 MB), and took the header's
object count as a `Vec` capacity; `apply_delta` did the same with a delta's result size. A
panic or a failed allocation aborts the single-threaded object and every landing in flight on
that trunk. Confirmed by the parsers reviewer with proof-of-concept packs. Fix ac503c1: every
read is checked, varints are at most 10 bytes, an entry or a delta result is at most
`MAX_OBJECT_BYTES`, a pack declares at most `MAX_PACK_OBJECTS`, a delta chain resolves in at
most 512 passes, each refusal a corrupt-pack error the loop already handles. Tests in pack.rs
(`a_truncated_entry_is_an_error_not_a_panic`, `a_delta_that_lies_about_its_size_is_refused_before_it_grows`).

**F3. High. A tree that nests deep enough overflows the wasm stack and aborts the object.**
Live: yes. Reachable by an agent: the gate diffs the change's tree before any other check.
Every recursive walk in `landseq-git/src/store.rs` (flatten, diff, edit_tree, new_objects,
missing_from, write_tree) recursed once a level with no limit; a 50,000-deep tree overflowed
a 512 KB stack in the reviewer's test, and an overflow in WebAssembly cannot be caught. Fix
ac503c1: `MAX_TREE_DEPTH` (256) in every walk, an error that `merge::changes` turns into the
`bad_path` ejection. Test: `a_tree_that_nests_too_deep_is_corrupt_not_a_stack_overflow`.

**F4. Medium. Nothing bounded the bytes one change brought into the object.** Live: yes.
Reachable by an agent (a large committed file is ordinary git). A change's files were fetched
and inflated whole into the object's 128 MB with no cap, and a loop that failed on them ran
again on them every round. Fix 8a2ce2c: a fetched answer is at most 72 MiB (transport.rs), a
pack as sent at most 64 MiB (`MAX_PACK_BYTES`), a pack inflates to at most 96 MiB and one
object to at most 32 MiB (pack.rs), and the gate ejects a change whose edited files sum to
more than 32 MiB as `too_big` (`merge::change_bytes`). **These are new product limits:** one
file over 32 MiB cannot come through a git fetch, and one change cannot add more than 32 MiB
of files; a repo with such a file on trunk cannot land changes to it. Raise `MAX_OBJECT_BYTES`
and `MAX_CHANGE_BYTES` if that is wanted, with the object's 128 MB in mind. Residual: a pack
near the limits can still take the object near its memory limit; a streaming pack reader
would remove that. Test: `the_bytes_a_change_adds_are_counted_from_its_files`.

**F5. High. A check that never answers wedges the queue, and nobody could take the change
out.** Live: yes. Reachable by any agent on a trunk with `[ci] typecheck`. `ci.rs` fails a
check with no answer in 25 s as `ci_unavailable`; the loop then ran again with backoff without
end, on the same batch, so one TypeScript tree that makes the checker take too long stalled
every landing on the trunk; `close` refused a queued PR, so neither the admin nor the agent had
a way out (the authorization reviewer rated this High; the lead had found it as Medium). Fix in
two parts: 245ed6b lets the admin close a queued PR while the trunk is paused and no run is in
progress; c1f6795 (`checked`, `timed_out`) treats a timeout as a failing verdict once the loop
has stalled three runs in a row on checks that gave no answer, so head is checked first (a slow
trunk ejects nobody) and the culprit search ejects the change whose prefix gives no answer, as
`ci_timeout`. A CI Worker that is down still stalls and never ejects. Test:
`a_timeout_counts_as_a_failing_check_only_after_runs_in_a_row`. Not run: the Nix fault
harness (`cf; nix run .#faults`) has a `ci_hang` case that exercises this path live.

**F14. Medium. A batch recorded after a crash kept the rules of before the landing.** Live:
yes. The success path of a landing reloads `landseq.toml` when the batch changed it; the
recovery path (`reconcile`, an attempt record found after a crash between push and record)
recorded the batch without that, so an approver the change removed could still seal, a new one
was refused, and every later landing failed `verify` until another rules change landed. Found
by the authorization reviewer. Fix f7ebe43: `reconcile` reads the rules at the recovered tip
as `init` does and stores them when the digest differs.

**F15. Medium. A Hub account with no key was every same-named approver on every repo.**
Live: yes. `repo.rs` `person_ok` accepted a keyless Hub user on any repo whose `landseq.toml`
lists an approver of that name, whatever that approver's key: whoever held such a sign-in link
read a private repo, commented, picked attempts and pushed PRs as a same-named person
elsewhere (never sealed). Fix 02d5925: a user with no key matches only an approver listed with
no key; an approver with a key needs a user with that key.

**F6. Low. The router made a Durable Object for any trunk name.** Live: yes.
`/v1/trunks/{name}/...` and `/t/{name}/...` called `id_from_name` on whatever came, with no
token, so a request stream could make objects without end. Fix 954f1f7: a name that is not
1 to 100 of `[A-Za-z0-9._-]` (the rule a trunk id must meet to get a Hub name) answers 404 in
the Worker.

**F7. Low. A reaction's comment id went into a storage key as sent.** Live: yes. The
`comment` field of a reaction (API, app and page) became part of `rx:m.{id}:{emoji}:{who}` with
no check that a comment with that id exists; any text made a key. Fix be9ab8c:
`reaction_target` takes the thread's own target or a comment id of the shape `stamp` makes
that exists on that thread; anything else is 400.

**F8. Medium. The pages wrote any `?k=` into every link unescaped (reflected HTML
injection on a public trunk).** Live: yes. `Ctx::href` (ui.rs) put the request's key into
the href and action of every link and form on the page as it came; `ui()` accepted any key on
a public trunk. A crafted link put markup into the page: the CSP stops script, but not a
`<meta http-equiv=refresh>` to another site, or links and forms that are not ours. Found by
the web reviewer, decode confirmed by test. Fix 0f5536e: a key is taken only as 32 lowercase
hex characters (the shape every key has), on the pages and in the app API; `init` refuses a
trunk id outside `[A-Za-z0-9._-]`; an emoji typed as itself must have no ASCII in it.

**F9. Low. Login CSRF and logout CSRF on `/login`.** Live: yes. A top-level GET with someone
else's sign-in link signed the visitor in as them for 30 days; `?out=1` signed them out. Fix
e75b1c9: the route refuses a request whose `Sec-Fetch-Site` is not `none` (pasted, typed,
bookmarked) or `same-origin`. Not done: a `__Host-` cookie prefix (it would sign every user
out once) and a one-time token in the link.

**F10. Low. The app Worker sent no security headers.** Live: yes. Fix e75b1c9: every answer
carries `Referrer-Policy: no-referrer` (keys travel in `?k=`), `X-Content-Type-Options:
nosniff`, `X-Frame-Options: DENY` and HSTS; `decodeURIComponent` on a malformed cookie or path
segment no longer throws a 500. Not done: a Content-Security-Policy for the app (vinext's
inline scripts make that a larger change).

**F11. Low. MCP tool ids went into Worker URL paths as the model sent them.** Live: yes. The
receipt, context and claim tools spliced `change_id`/`task` into a path, so
`../../OTHER/queue?` read another trunk with the agent token; the `repo` argument let a tool
run git, and land a commit, in any directory on the machine. Fix 0a9b250: every id is 1 to 64
of `[A-Za-z0-9._-]`; `repo` must be the repo the server was started in or a directory under it.

**F12. Low. `keygen` wrote the private key before chmod; the context index file lived in a
shared temp dir.** Live: yes. Fix 0a9b250: `~/.landseq/key` is 0600 from the first byte; the
index file is in the repo's own git directory.

**F13. Medium (local tool, not deployed). `seqd` git option injection and a dropped
credential-helper override.** `seqd/` is the bench's local baseline (no authentication, by
design; the README now says so). A request's `commit` or `base` went onto git's command line
as it came, so `--output=<path>` made git create or truncate any file the seqd user can write
(the ops reviewer demonstrated the shape); seqd's `Git` set `GIT_CONFIG_COUNT=1` and so dropped
the bench's `credential.helper=` entry, which is how the mirror token reached
`~/.git-credentials` on Oct 7. Fix 5353f4f: requests are validated (40-hex
revisions, bounded change id, one-line subject), the diffs say `--end-of-options`, `?wait=`
is clamped, the parent's `GIT_CONFIG_*` entries stay, the bench redacts a failed command line,
and `.gitignore` covers `.env*`, `.dev.vars.*`, `.prod.vars.*`, `*.vars`.

**F16. Medium. `landseq.toml` was a seal boundary only by convention.** Live: yes.
`seal_boundaries` gave a governance reason only for paths matching `seals.governance`; a rules
file that left itself out of its own globs let an agent rewrite approvers, quorums or
`[decide]` without a seal (every fixture lists it, so no live repo was exposed). Found by the
authorization reviewer. Fix fd49708: the rules file needs a governance seal whatever the globs
say. Test: `the_rules_file_is_a_seal_boundary_whatever_the_globs_say`.

**F17. Medium. Forks per trunk were unbounded.** Live: yes. The shared agent token made one
Artifacts repo per new name without end, every page listed them all, and the Branches view
read each fork's refs. Fix fa09844 (`MAX_AGENTS`): a trunk takes at most 500 forks, by API
and by first git push.

**F18. Low. Four smaller authorization gaps**, all live, fixed in the same commit as F17:
an agent key acted for a name that later became a person's (shared fork and inbox); allowance
ids went into storage keys as sent; a seal could expire between the gate and the landed
commit's time, so `verify` refused a landing the gate accepted (the gate now wants 3 minutes
left on a seal); `init` never tied the trunk id to the object's name, so two objects
initialised with one id shared keys and accepted each other's seals (the router now sends
`X-Landseq-Trunk` and `init` refuses a mismatch); a name moved to another trunk left the losing
trunk public when a later step failed, and failures to make a trunk private were ignored (the
losing trunk is private first; failures come back as `warnings`).

### Open

**O1. Low. The CI-timeout ejection (F5) is verified by unit test, build and the stall path
only.** The harness's `ci_hang` case (45 s) proves the stall path still recovers; the
ejection (head passes, one prefix hangs three runs in a row) has no harness case. See
"Fault harness" for the command that reaches the three-stall path, and watch for a trunk
whose own tree takes over 25 s (it stalls, as designed; raise `TIMEOUT_MS` for it).

**O2. Low. `seqd` runs the CI and derived commands of `landseq.toml` with its whole
environment** (`seqd/src/git.rs` `command()` has no `env_clear`), so a change that edits
`ci/check.py` reads the operator's shell secrets. Local tool; a README sentence now says never
to expose it. If it ever listens off loopback: `env_clear()` with an allowlist, a bearer
token, and CI from trunk's copy of `ci/` as `bench/review_gates.py` does.

**O3. Low. Hygiene before the repository goes public.** No credential exists anywhere in the
tree or in the history of any branch (ops reviewer: every added line of all 362 commits).
What remains is the user's call: LAN addresses (the Forgejo host and one more, in `bench/run.py`,
`cf/scripts/scenarios.py`, `deploy/forgejo.yaml`), the kube context `admin@turingpi` and
host names in `README.md`, `bench/cleanup.sh`, `deploy/forgejo.yaml`, `bench/sweep.sh`,
`bench/report_template.html`, `results/report.html`, `bench/review_models.py`. Home paths,
Claude Code scratchpad paths and this Mac's LAN address in `cf/evidence/*.json`,
`results/sweep2.log` and `results/runs.jsonl` are scrubbed in this branch. Also decide which
refs to push: `git push --all` would ship the `codex/*` and `worktree-agent-*` branches.
`~/.cargo/advisory-db` is a broken clone that makes `cargo audit` fail; delete it.

**O4. Low. Glob matching is exponential in the worst case** (`landseq-core/src/glob.rs`:
`**` and `*` each try every split) and a non-append commutative merge is quadratic
(`merge.rs`). Patterns are admin-written; paths and content are agent-written. Not reachable
with the fixture rules; a linear matcher, or refusing adjacent `**` in `Rules::parse`, closes it.

**O6. Medium. Agent-written text enters the decision model's prompt.** `trunk/decide.rs`
sends the commit subject, body, paths and diff to the model; an agent can write "routine,
merge" into them. The model can only add a reason (review or risk), never remove a path rule's
reason or approve anything, so the ceiling is: no added friction on a change that no path rule
gates. Keep sensitive paths under `seals.approval`/`governance`, which the model cannot touch;
Clef (a classifier, on `branches-fix`) is less steerable than a chat model.

**O7. Medium (performance). The `state` and `agent` views scan every change record on each
call** (`app.rs`: all `c:` to find 30 ejected, all `cm:`, all `rr:`), and the shell polls
`state` every 2 s per tab; enough tabs starve the landing alarm and trip the overload guard
(503 to agents). An ejected index written at eject time and comment counts on the change
record fix it; the `reads-split` branch moves reads off the Trunk object altogether.

**O8. Low. A newer revision does not retire an older queued one** (both can land, or the
newer closes as a conflict). Each revision is gated and sealed on its own, so not a bypass;
branch `t8-t9-split` has the supersede logic (T-8/T-9), rolled back with freeze2.

**O9. Info. Noted, not changed:** a `person_ok` answer is kept 60 s, so a deleted or re-keyed
Hub user keeps access up to a minute; the first `POST /forks` for a name owns it (squatting;
the real agent then needs the admin); after a token-made `info/refs`, the token may send one
receive-pack within 10 minutes as that agent; a new Hub repo is public when `public` is
omitted; any agent may attach a change to any task; `stall` and `fetch_retry` details carry up
to 300 characters of upstream error text on public repos (no token ever appears in them).

**O5. Info. `request_approval` can be asked again after a decline** (mcp.rs); a per-change
cooldown would stop a model from nagging. `git-credential` answers the raw token as the
password when git sends no `path=` (document `credential.useHttpPath`). `landseq-ci` has no
CPU/memory limits and fails closed. Task labels become CSS class names (cosmetic). Two latent
indexings (`log.rs:54`, `trunk.rs` `touched_rules` `parents[0]`) are unreachable today.

### Accepted (design)

**D1. One secret signs everything.** Share, personal, account and agent keys are HMACs under
`LANDSEQ_ADMIN_TOKEN`; rotating it revokes every link, cookie and agent key at once.

**D2. The shared agent token reads everything and makes unlimited first forks.** By design;
each fork is an Artifacts repo (`FORKS_IN_FLIGHT` bounds concurrency, not count).

**D3. Any person with a personal link can delete another agent's stale refs** (`cleanup`).
Stale refs only (landed or superseded); documented on the page.

**D4. `git_read_token` on Forgejo falls back to the trunk write credential for upload-pack
proxying.** Not exploitable through upload-pack; Forgejo is the local stand-in.

## The Codex merges: NERUDO brand (351d8cc) and unified code discovery (aca5114)

These two commits were merged on the live landseq/landseq main during the audit. They branch
from `agent-surfaces` 3b28852, so they lack the freeze line's scale and resilience work (lazy
trees, the UI-store fix, deadlines, backoff); the search review covered them on their own
line. Fixes for them are on branch **`security-audit-search`** (off aca5114, in the worktree
`scratchpad/wt-codex` of this session), one commit, 0a3a280. No authorization bypass was
found: search and digest go through the same read gate as the tree, the Hub's cross-repo
search filters by the viewer and never names a repo the viewer cannot open, cursors are
fingerprinted per viewer, and `regex` builds for wasm32 with 0 advisories.

**C1. High (availability). A search held a public trunk's object for seconds, with no key.**
`trunk/search.rs` had byte and file caps (16 MiB, 20,000 files) but no time budget; a regex
over the cap took 1 to 2 s natively (the reviewer's bench; several times that in the object),
the path matcher folded every path once per query word for up to 256 words (1.2 s over
20,000 paths), and `/v1/hub/search` asked 8 trunks per page with the app continuing 4 pages
by itself, so one keystroke on the home page could start 32 trunk scans. While a search runs,
no change lands on that trunk and no page of it answers; an `<img src>` on any third-party
page can start one. Fix 0a3a280: a search stops at 1.5 s and answers with what it found
(`stats.timed_out`), scans at most 4 MiB and 5,000 files, folds a path once and ranks at most
16 words, the Hub asks 4 trunks per page, the app continues one page by itself.

**C2. Medium/High (availability, the reviewer's rating). Concurrent searches each take the
UI's copy of trunk.** On the Codex line `ui_store` is the pre-freeze version: concurrent
requests each refetch trunk (the cause of the live cloudflare/agents hang on Oct 12, fixed on
`branches-fix`/`ship-d` by "one request at a time has the UI's copy"). The Codex line needs
that fix merged before the search feature ships; `security-audit-search` does not carry it.

**C3. Medium. Signed-in Hub requests wake every private trunk.** `hub.rs` `Viewer::repos`
(16 concurrent, no timeout) and the global search's checks call `/t/{trunk}/api/access` on
every private repo for the home page, user and org views and search, so each page for an
account holder costs one object wakeup per private repo, and a private trunk that is slow or
down makes its repo 404 for its own members (errors map to "cannot see"). **This is also in
the live uncommitted patch.** Open: record membership on the Hub, or cache (user, trunk) in
the front Worker for a short time, and check private repos lazily.

**C4. Low/Medium. One cached digest, evicted by `?at=`.** Alternating `digest?at=1` and
`at=2` on a public repo had the whole tree walked again (every blob read, every package.json
parsed) on the object's thread each time. Fix 26043f7 (on `security-audit-search`): the last
four commits' digests are kept.

**C5. Low. The Hub search cursor is a SHA-256 fingerprint, not an HMAC.** A forged cursor only
moves `after`/`offset` inside the viewer's own authorized set, which is recomputed on every
request, so no access widens. Open: an HMAC under the admin secret if the cursor ever carries
more.

**C6. Low. README images load from any https host**, so a viewer's IP reaches an image host
named in a public README (the share key does not leak: `referrerPolicy="no-referrer"`), and
Markdown plus highlighting render on the web Worker per request within the 64 KiB and
256 KiB caps. Open: proxy images if viewer privacy matters.

The reviewer's open questions: the Codex line has no lazy object mode; if search and digest
are merged onto the freeze line, re-audit them there (each blob would be one binding read, up
to 5,000 per request); the Durable Object's CPU-limit reset behaviour is unverified; the two
worst search inputs should be timed on a preview deployment; the web test suite was not run.

## Fault harness

`cd cf && nix run .#faults` on the audited tree (all cases: parity scenarios, CI down on 1
and 8 trunks, CI hang, flaky git, hung git, SIGKILL ×3, queue growth 32/64/128): **102
checks passed, 0 known gaps, 0 failed**; `landseq verify` passed on every trunk; drain time
×2.0 per doubling (budget ×3.0). Evidence: `cf/evidence/faults-20261013-security-audit.json`.
The run started at c1f6795 and the last two commits (fd49708, fa09844) landed while it ran;
the evidence records `fa09844+dirty`, so which of the two trees the Worker was built from is
not certain. A second run of parity, CI down and CI hang on the final tree (evidence
`cf/evidence/faults-20261013-security-audit-final.json`, versions `0db937d`): **57 checks
passed, 0 known gaps, 0 failed**.

What the CI-hang case proves: with a 45 s hang the loop times out twice and recovers, so the
stall path still works and nobody is ejected. It does not reach the three stalls that make a
timeout a failing verdict (F5). To exercise that path: `nix run .#faults -- --cases ci_hang
--ci-hang-seconds 120`; the designed outcome is `trunk_red` (head hangs too), not an
ejection. The ejection itself (head passes, one prefix hangs) has no harness case; it is
covered by the unit test of `timed_out` and by reading.

## Checked and sound

What the reviewers examined and found right, so nobody re-reviews it:

- **Seals and quorums.** A seal binds change id, revision, commit, trunk, rules digest and the
  exact reasons; signatures are verified at submission; each approver counts once per kind
  and only with an eligible role; trailers list only counted seals; a change goes back on the
  queue only on a complete quorum; a seal arriving mid-gate is not parked on a stale tally.
  Allowances reserve, release and spend consistently across waiting, ejection, commit and
  recovery. A rules change lands last and alone; break-glass lands only a `landseq.toml`-only
  change from `AwaitingSeal` and is marked on trunk. The decision model only adds reasons.
- **Trailers.** The landed commit's trailers are read from the sequencer's last paragraph
  only; an agent's body text cannot pose as `Change-Id`, `Sealed-By` or `Break-Glass`;
  reasons with control characters eject; the sequencer's provenance keys replace an agent's
  own in context files.
- **Agent identity.** The shared token cannot land, close, comment, react, read notes, clean
  up, claim or propose for any agent with a fork or any person; revisions come only from
  revision 1's fork; an agent key is bound to trunk and generation; `Caller::trusted()` is
  used only after person or git-credential authentication; `X-Landseq-Repo` (and now
  `X-Landseq-Trunk`) are stripped from clients.
- **Hub.** The Hub object is unreachable from clients; `cached_view` asks the trunk before
  showing a public summary and 404s private records to anonymous viewers; writes by name never
  use the memo; `RESERVED` covers every app and Worker top-level path; org and user names are
  mutually exclusive; the live `Viewer` patch is fail-closed and `access` returns one boolean.
- **Parsers.** pkt-line, push commands, push reports and sideband are bounds-safe; commit,
  tag and tree parsing cannot panic; the log hash chain binds each entry to the previous one
  and its fields cannot carry an injected separator; inflate output is bounded by the pre-sized
  buffer; `base64_decode` and the fill/prefetch loops are bounded.
- **Pages and app.** Every interpolation in `ui.rs` goes through `esc()` except the key, now
  shaped; the `back` redirect stays on `/t/{trunk}`; CSP `form-action 'self'` and
  `referrer-policy: no-referrer` on the pages; the React app has no raw HTML sink, Markdown
  links need `https?://` and get `noopener noreferrer`, no images render, every href is
  encoded; the cookie is HttpOnly and SameSite=Lax, forwarded only for GET or same-origin
  POST and never with `?k=`; no GET mutates; no CORS, so cross-site JS cannot send the
  identity headers; cookies never travel past the sequencer and git proxies copy only the
  git headers upstream.
- **CLI.** git arguments are fixed or validated (40-hex revisions, `--` before paths);
  credentials go through `GIT_CONFIG_*` with `credential.helper` cleared and no terminal
  prompt; `git-credential` answers only its own hosts (http only for loopback); agent key files
  are 0600 in 0700 directories; keys print only in `agent-key`, `invite`, `share`, `user`;
  MCP input sizes are capped server-side; the approval elicitation signs only on the client's
  explicit accept.
- **Ops.** seqd uses parameterised SQL and a real compare-and-swap push; bench results and
  evidence are redacted and hold no token; the fault harness keeps random per-run tokens in a
  0700 temp dir; the GitHub workflow has read-only permissions and no secrets; `deploy/forgejo.yaml`
  has no password; wrangler configs hold no secrets; `landseq-ci` has no public route; the
  vendored ts-rust module records its upstream commit and checksum.

## Tooling

- `cargo audit` (RustSec advisory database, 1307 advisories): 0 advisories against the root
  `Cargo.lock` (118 crates) and `cf/Cargo.lock` (156 crates).
- `npm audit --package-lock-only` in `cf/nerudo-app`: 10 advisories (6 high, 4 moderate), all
  transitive under `vinext` 1.1.0 and `@vinext/cloudflare`: `braces`/`micromatch`/`fast-glob`
  (build-time glob libraries), `fflate` through `satori`/`@vercel/og`,
  `vite-plugin-commonjs`/`vite-plugin-dynamic-import`. None runs in the deployed Worker; they
  are build-time. Upgrade vinext when a release moves past them.
- Secrets: a grep for token, key, private-key and sign-in-link shapes over the tracked tree and
  over `git log --all -p` found only documentation placeholders and the test fixture in
  `cf/nerudo-cli/src/context.rs` (fake `ghp_`/`sk-` strings that the provenance test must
  redact). `.dev.vars`, `.prod.vars`, `deploy/.forgejo-*` and `.wrangler/` are gitignored and
  no tracked file matches `*.vars`, `*.pem`, `*.key`.

## Operations: what the user must do

1. **`TYPESAFE_API_KEY` is still a secret on the live Worker and in `cf/nerudo-worker/.dev.vars`**
   (checked with `wrangler secret list`). The Jev provider is gone from `branches-fix`; the live
   build still has it. Delete the secret (`wrangler secret delete TYPESAFE_API_KEY --name landseq`)
   once the live build no longer reads it, and rotate the key with the provider.
2. **The bench Forgejo credential is still in the macOS keychain** (`security
   find-internet-password -s <the Forgejo host's address>` finds it). `~/.git-credentials` no longer has it.
   Run `security delete-internet-password -s <that address>`.
3. **`~/.git-credentials` holds one landseq line**: the `lsa1.` agent key of agent
   `consolidator` on the throwaway trunk `stress-lab-20261012-144305`, saved by the `store`
   credential helper when a stress harness pushed with plain git without clearing the helper.
   Low (a per-trunk key on a test trunk), but remove the line, and make every harness that
   runs git set `GIT_CONFIG_*` with `credential.helper=` as `bench/agents.py` `NO_SIGN` does.
4. **The live Worker runs code that is in no commit** (freeze 52f21d9 plus the hub.rs/app.rs
   patch in the main checkout). Commit that patch before the repository goes public, so the
   public source matches what judges use.
5. **One secret signs everything.** Share links, personal links, account sign-in keys and agent
   keys are HMACs under `LANDSEQ_ADMIN_TOKEN`. Rotating the admin token revokes every link,
   every cookie and every agent key at once. That is the design; know it before rotating.
