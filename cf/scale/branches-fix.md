# The branches view and the web UI's copy of trunk (branch `branches-fix`)

Two changes for the live hang of the page of cloudflare/agents on 2026-10-12. The evidence is
in `cf/evidence/ui-memory-20261012.json` and `cf/evidence/branches-fix-gates-20261013.json`.

## The branches view (eb30765)

The branches view read each fork one after the other, with one request to the git host for
each fork. On live, the 17 forks of cloudflare/agents took 11 s, and 27 s when each fork
token had to be renewed. The web app asks for the branches every 10 s from each open tab.

- The view reads up to 6 forks at the same time.
- What a read of a fork gives answers each request of the next 5 s. A request that comes
  during a read waits for that read.
- Cleanup reads the fork itself, and then forgets what was kept of it.
- On a lazy trunk, the branches view and the agent page do not run again for misses that
  other requests left in the UI's copy of trunk.

## The web UI's copy of trunk (6cba20a)

On a trunk that is not lazy, the UI's copy of trunk has all the history with all the files:
about 62 MB for cloudflare/agents. Each request took the copy out of the object. A request
that came during another request got an empty copy and read all of trunk again. Two copies
passed the isolate's limit of 128 MB, and the object started again and again.

- One request at a time has the copy. A request that comes while another request has it
  waits, at most 10 s, and then fails as busy. It does not read trunk again.
- The copy goes back when its guard drops, also when a request ends early.
- The state call does not take the copy. A landing whose record has no subject gets the
  subject from the copy only if the copy has that commit.

## Results

On a replay of cloudflare/agents (not lazy, 17 forks, 3 simulated tabs, 4 minutes):

| Build | Errors | Isolate restarts | Max memory |
|---|---|---|---|
| Live build, cold start | 53 of 195 | 4 | 135 MB |
| This branch, cold start | 0 of 527 | 0 | 62 MB |
| This branch, warm | 0 of 527 | 0 | 78 MB |

The app API gives the same bodies with the live build and with this branch: 24 of 24 on each
of two replays. One difference can occur on live: kcirtapfromspace/demo has 20 landings
without a recorded subject, from before the gate recorded subjects. After a cold start,
their subjects stay empty in the state call until a page reads the UI's copy.

The gates (at 6cba20a): `cargo test` for all crates, the wasm check, `nix run .#faults` (102
passed), the 17 Artifacts scenarios on the throwaway Worker, the body comparison, the cold and
warm tabs, and a test of the wait path. In the wait-path test, a request that takes the copy
twice fails after 10 s. A tree request while another request has the copy for 15 s fails
after 10 s (HTTP 500). The state call answers in 0.07 s during that time.
