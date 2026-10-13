# Review of the documents for Simplified Technical English

This report tells how we reviewed the Markdown documents of this repo against ASD-STE100
Simplified Technical English (STE). It gives what we checked, the main kinds of changes,
and what we did not change on purpose. The review is from 2026-10-12, on branch `docs`.

## What we checked

The documents:

| Group | Files |
|---|---|
| The repo | `README.md` |
| Nerudo (formerly nerudo) on Cloudflare | `cf/README.md`, `cf/DEMO.md` |
| The fault tests | `cf/faults/README.md` |
| The scale campaign | `cf/scale/README.md` (new), `repos.md`, `10k.md`, `stages.md`, `hub.md`, `hub-next.md`, `mono.md`, `ci.md`, `resilience.md`, `freeze.md`, and this file |
| Documents from other branches | `cf/scale/ops.md` and `slo.md` (branch `ops`), `cf/scale/dogfood.md` and `release-v0.2.0.md` (branch `dogfood`). We copied them into this tree without change, and then reviewed them. |
| Vendored files | `cf/nerudo-ci/vendor/ts-rust/SOURCE.md` (ours) and `NOTICE.md` (from ts-rust) |

The rules:

1. One topic for each sentence.
2. Procedural sentences of 20 words or fewer. Descriptive sentences of 25 words or fewer.
3. Active voice where the actor is known. Imperative for instructions.
4. One instruction for each sentence, unless the actions occur together.
5. Simple tenses. No present perfect.
6. No "-ing" form of a verb as a noun or an adjective, except in a technical name.
7. No phrasal verbs and no contractions. One word for one meaning.
8. Paragraphs of 6 sentences or fewer. Vertical lists for steps. Tables for data.
9. Articles where English needs them. Units and numbers as they are.

The tools:

- A rough checker of the rules above (sentence length, paragraph length, "-ing" words,
  phrasal verbs, some words that STE does not approve, contractions). It cannot find two
  topics in one sentence or a missing article, so we also read each document in full.
- A link checker. It resolves each relative link to a file in the tree, and each `#anchor`
  to a heading of its target.
- `mermaid-cli` 11.4.2 (`mmdc`), which renders each Mermaid block to SVG. The SVGs are not
  in the repo.

Two helper agents did the first pass of the scale documents and of the fault test README.
We read each of their changes before the commit, and we corrected three of them.

A shorter sentence can lose a condition. Thus we also counted some qualifiers in each file,
before and after: "at once", "only", "never", "exactly", "at most", "at least", "always",
"each", "every", "must", "cannot", "still", "not", "no", "all", "if" and "unless". We
examined each count that decreased. One change had lost "at once", and we put it back. The
other decreases were rewrites with the same meaning, for example "every" to "each" and "can
not" to "cannot".

## Results

The checker counts before (at `5ed344d`) and after this review. The counts do not include
the technical names in "Left on purpose".

| File | Before | After |
|---|---|---|
| `README.md` | 12 | 0 |
| `cf/README.md` | 31 | 6 |
| `cf/DEMO.md` | 21 | 3 |
| `cf/faults/README.md` | 10 | 1 |
| `cf/scale/repos.md` | 8 | 0 |
| `cf/scale/10k.md` | 24 | 5 |
| `cf/scale/stages.md` | 10 | 3 |
| `cf/scale/hub.md` | 14 | 2 |
| `cf/scale/hub-next.md` | 30 | 3 |
| `cf/scale/mono.md` | 20 | 3 |
| `cf/scale/ci.md` | 36 | 4 |
| `cf/scale/resilience.md` | 24 | 1 |
| `cf/scale/freeze.md` | 12 | 1 |
| `cf/scale/ops.md` (from branch `ops`) | 16 | 7 |
| `cf/scale/slo.md` (from branch `ops`) | 11 | 2 |
| `cf/scale/dogfood.md` (from branch `dogfood`) | 4 | 1 |
| `cf/scale/release-v0.2.0.md` (from branch `dogfood`) | 13 | 0 |
| All | 296 | 42 |

Most of the flags that stay are false. The checker joins two sentences when the second
starts with a name in lower case (`d120k`, `toxiproxy`). It splits a sentence at a colon.
It also counts a list of nouns in a table cell as one long sentence. The other flags are in
quoted text, or in the parts that we did not change on purpose.

These documents have 210 relative links. All of them resolve, except two in `NOTICE.md`
(see below). All 23 Mermaid blocks render.

## The main kinds of changes

| Kind | Example before | Example after |
|---|---|---|
| Two clauses in one sentence | "... and 2.6 to 4.2 s one after the other; at six checks, 7.3 to 8.0 s and 3.7 to 6.2 s." | "... and 2.6 to 4.2 s one after the other. Six checks of 400 modules took 7.3 to 8.0 s at the same time, and 3.7 to 6.2 s one after the other." |
| A phrasal verb replaced | "after CI comes back" | "after CI is available again" |
| A phrasal verb replaced | "The picked PR goes back into the queue." | "The picked PR returns to the queue." |
| Passive with a known actor | "14 fetches were tried again" | "the gate tried 14 fetches again" |
| Present perfect | "1,000 of the 2,000 trunks had never landed a change" | "1,000 of the 2,000 trunks landed no change before this run" |
| An "-ing" adjective | "a failing batch", "the conflicting lines", "each waiting PR" | "a batch that fails", "the lines in conflict", "each PR that waits" |
| A run of items in one sentence | The admin routes of the API in one sentence of 39 words | A vertical list, one route on each line |
| A procedure in prose | "Pause a trunk, send one change, set the stub to 503, resume, and read the queue" | Numbered steps, one action in each step |
| A contraction | "don't hand-edit the lockfile" | "Do not edit the lockfile by hand." |
| A long paragraph split | The agent token paragraph of `cf/README.md` (8 sentences) | Two paragraphs, one topic each |

Changes of content, made at the same time:

- `cf/README.md`: the "Measured" section has a chart of the bench and a table of the scale
  campaign. The sections on CI, the API and "Not implemented" now tell what the freeze
  build does. These are the search that halves a failed batch, the CI deadline, the
  backoff and the `stall` field.
- `cf/scale/README.md` is new: the overview of the scale campaign, with charts.
- The source documents of the scale campaign and the fault test README have the same charts
  as the overview. Each chart is next to the table that holds its numbers.
- `cf/scale/repos.md` and `cf/scale/README.md` cite the post of GitHub "The August 17
  outage, and the work ahead" for approximately 1,100 commits and 50 merged PRs each
  second. They say that "2 billion each day" has no published source.
- The first mention of the product in a document names "Nerudo (formerly nerudo)". The code
  names do not change: the `nerudo` CLI, the crates, the Workers and the URLs.
- `ops.md` and `slo.md` start with a note: the freeze build has the code of `ops`, but no
  `METRICS` binding, and nobody deployed the alert Worker. `slo.md` also says that the
  backoff of `resilience.md` is now in the build.
- `dogfood.md` says that `dogfood.py` is on branch `dogfood` only, and that its states are
  the states at the end of the run.

## The release notes of v0.2.0

A maintainer signs `cf/scale/release-v0.2.0.md` with
`nerudo release --trunk nerudo/nerudo --at 69 --notes scale/release-v0.2.0.md`. Thus we
kept its meaning and each reference exact. A script compared the two versions: each PR
number, task, agent name, count and other number is the same, in the same order. The file
has the same 67 lines. The first line (the title) did not change.

| Line | Before | After |
|---|---|---|
| 2 | "... names its task; its merge closed the task." | "... by the agent of its workstream. Each of these PRs names its task, and its merge closed the task." |
| 6 | "One name and one sign-in key for a person across orgs, teams and repos; approvers who have not joined; closing a PR that waits for a person; old names show as new" | "A person has one name and one sign-in key across orgs, teams and repos. A repo can list approvers who did not join yet. A PR that waits for a person can be closed. Old names show as the new names" |
| 7 | "Approve a parked PR from the coding agent ..." | "A person can approve a parked PR from the coding agent ..." |
| 8 | "... and the conflicting lines; Compare shows each plan" | "... and the lines in conflict. Compare shows each plan" |
| 9 | "... verify again: a canonical rules digest, and the older forms still count" | "... verify again. The rules digest is canonical, and the older forms of the digest still count" |
| 10 | "A key for each agent, issued with its first fork; the shared agent token ..." | "Each agent gets a key with its first fork. The shared agent token ..." |
| 14 | "Fault tests in Nix (CI down, CI hang, flaky git, kill -9, growth); a network error ... (fetch_retry); verify reads ..." | "Fault tests in Nix: CI down, CI hang, flaky git, kill -9 and growth. A network error ... (fetch_retry). verify reads ..." |
| 15 | "Deadlines for CI and git, an interval that doubles with jitter, a stall that people can read, push again after a lost reply, HTTP 503 with Retry-After on overload" | "Calls to CI and git have deadlines. The interval before the next try doubles, with jitter. People can read the stall. After a lost reply, the sequencer sends the same push again. An overload gives HTTP 503 with Retry-After" |
| 16 | "A fetch that comes back with no packfile is transient: the change stays queued" | "A fetch that returns no packfile is transient. The change stays queued" |
| 20 | "... 200 changes/s, linear in the number of trunks, with the p50 of one trunk" | "... 200 changes/s. The rate is linear in the number of trunks, and the p50 latency is that of one trunk" |
| 21 | "... in each isolate: the Hub limit of about 600 req/s no longer caps traffic by name" | "... in each isolate. Thus the Hub limit of approximately 600 req/s no longer limits traffic by name" |
| 22 | "The gate fetches each fork's queued commits in one request, 6 forks at once: a 512-change burst drains in 12-24 s, not 167 s; stage timings for each round (GET .../timings)" | "The gate fetches the queued commits of each fork in one request, 6 forks at the same time. A burst of 512 changes drains in 12 to 24 s, not 167 s. Each round records its stage timings (GET .../timings)" |
| 23 | "Hub records in an isolate memo and the colo cache: the repo page ..." | "A memo in each isolate and the colo cache keep the Hub records. Thus the repo page ..." |
| 24 | "Dispose the results of Artifacts binding calls (a Trunk object stayed billed 11 minutes and more); 10,000 trunks land 500 changes/s" | "The Trunk object disposes the results of Artifacts binding calls. Before, a Trunk object stayed billed for 11 minutes and more. The test of 10,000 trunks landed 500 changes/s" |
| 25 | "Find the culprit of a failed type check by halving the batch: CI runs 290 to 175 (cap 16) and 530 to 209 (cap 64) with 5% bad changes" | "The sequencer finds the culprit of a failed type check with a search that halves the batch. With 5% bad changes, the CI runs went from 290 to 175 (cap 16) and from 530 to 209 (cap 64)" |
| 29 | "... reads trunk's trees, not its history or file contents; a 728 MB tip with 70,026 files lands with 14 MB" | "... reads the trees of trunk, not its history or file contents. A tip of 728 MB with 70,026 files lands with 14 MB" |
| 31 | "A big trunk is read one object at a time: lazy trees through the Artifacts binding, the web UI, release notes, plain git push and import" | "The sequencer reads a big trunk one object at a time, with lazy trees through the Artifacts binding. The web UI, release notes, plain git push and import work on it" |
| 35 | "Metrics points for each round (no-op with no binding), SLOs and an alert Worker; a small request body ..." | "Metrics points for each round (they write nothing with no binding), SLOs and an alert Worker. A small request body ..." |
| 36 | "A request body sent to the Trunk object whole keeps redirect manual (web form posts got the target page)" | "A request body that goes to the Trunk object whole keeps redirect manual. Before this fix, web form posts got the target page" |
| 40 | "... how the conflicts were resolved, ..." | "... how the merges resolved the conflicts, ..." |

Not changed in the release notes, on purpose:

- The title on line 1 ("nerudo v0.2.0: ..."). It names the repo `nerudo/nerudo` and its
  release. A change to "Nerudo" in a signed release is a decision for the maintainer.
- The lines under "Also in this release" and "Approved changes". They are the subjects of
  PRs #45 to #49, as the sequencer wrote them in its draft.
- "Not in this release", "Contributors", "Full changelog" and the two links.

The proposal of agent `dogfood` in the inbox of the web app keeps the old text. The
signature covers the notes that the signer gives with `--notes`, so the signer signs this
version.

## Left on purpose

| What | Why |
|---|---|
| The technical names "landing", "landing loop", "binding", "coding agent", "competing attempt", "merging" (a task status), "working tree" and "working copy" (git terms), "sign in" and "sign out" | They are names in the product or in git. STE permits technical names. |
| Text in quotes: error messages, the Cloudflare documentation, the block page of the edge, and the lines that the presenter says in `cf/DEMO.md` | A quote stays as it is. |
| The section "The web app" of `cf/README.md` | The NERUDO branch (`codex/nerudo-brand-and-repo-ux`) rewrites it. |
| The parts of `cf/README.md` that branch `freeze2` changes: "Push a branch", "Branches and cleanup", the table of scenarios and the `land` route of the API | So that `freeze2` merges with no conflict. One sentence there stays in present perfect ("the token of a fork has expired"). |
| Headings | Links point to their anchors. The one heading that changed ("Gotchas found on this machine" in `README.md`) has no link to it. |
| `cf/nerudo-ci/vendor/ts-rust/NOTICE.md` | It is the licence notice of ts-rust, copied without change. Its links to `UPSTREAM.md` and `PROVENANCE.md` point to files that we did not vendor. |
| Lists of nouns in table cells, for example the Components table of `cf/README.md` | A list of names is not a sentence. |
| A passive where the actor is not known or not important, for example "No change was ejected" | STE permits this in descriptive text. |
| The words "need", "approximately", "licence", "req/s" and "×12" | We applied the main rules of STE, not the full dictionary. The spelling and the units stay as the repo has them. |
| Four sentences whose meaning was not clear to us: "a seat without a seed" (`10k.md`), "47 of 71 checks" (`ci.md`), "A search from main that is red could find `bad`" (`ci.md`), and "its error is the fetch of that change for this round" (`stages.md`) | A rewrite could change the meaning. The authors of these documents can make them clear. |

## Documents on other branches

Two documents describe work that is not live, so they stay on their branches. We reviewed
them there with `git show`, and we did not change them. The checker found 23 flags in
`reads.md` and 13 in `backlog.md`. These are the issues, with the line numbers on each
branch:

| Document | Issue | Where |
|---|---|---|
| `cf/scale/reads.md` (branch `reads-split`) | Descriptive sentences of 26 to 41 words | The summary (lines 17 to 37); design steps 4, 6 and 9 (line 101 on); "Limits of the View" (line 147); the results bullets (lines 229 to 240); lines 254 and 269 |
| | Phrasal verbs: "can be turned off", "To go back" | The consistency table, lines 142 and 143 |
| | Unapproved words: "just" (line 190), "very many" (line 288) | |
| | Long table cells (26 to 30 words) | Lines 68 and 202 |
| | A passive with a known actor: "a Worker without the class ... is refused" | Line 143 |
| `cf/scale/backlog.md` (branch `freeze2`) | Paragraphs of 7 and 9 sentences | The test of T-7 (line 70) and the PR of T-7 (line 84) |
| | Descriptive sentences of 26 to 29 words | Line 53 ("For each branch whose name is a PR, ..."), and rule 2 of T-8 (the sentence on the MCP `land` tool) |
| | "-ing" adjective: "a waiting change", "a waiting one" | Rule 4 (line 125) and "Status" (line 155) |
| | "just before the write" | The table of the change, line 164 |
| | A long table cell (27 words) | Line 167 |

The meaning of both documents is clear. No issue changes a fact.

## Not in this review

- `cf/STRESS_TESTING.md` and `cf/LLM_WORKFLOWS.md`. They are not in this tree.
- Code comments, and the text of the web app.
