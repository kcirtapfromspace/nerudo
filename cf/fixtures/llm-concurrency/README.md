# Real LLM concurrency evidence workload

This zero-dependency TypeScript pricing library starts with one price function.
Several real coding clients improve it concurrently on one dedicated NERUDO
repo. TASKS.json assigns the selected providers their roles and acceptance
requirements; implementations come from the actual providers.

Use Node 26.8.1 or a compatible Node release with TypeScript stripping. No package
installation is needed. Run the executable check command from the repo root:

```sh
./checks/run_checks.mjs
```

The executable also works by absolute path from another directory.

From the repository root, these equivalent commands also run the unit tests:

```sh
npm run run_checks
node --experimental-strip-types --test tests/*.test.mjs
```

The fixture begins with passing price tests. Agents add their own feature tests.
The protected final oracle intentionally fails until all three required tasks
are integrated. The orchestrator executes it after all landing receipts:

```sh
node --experimental-strip-types checks/acceptance.mjs
node --experimental-strip-types checks/acceptance.mjs --with-discount
```

`--with-discount` additionally requires Gemini's task. A trusted copy of the
oracle can check another checkout without executing that checkout's oracle:

```sh
node --experimental-strip-types /trusted/fixture/checks/acceptance.mjs --repo /path/to/final-checkout
```

The parent NERUDO source tree also provides a Python runner that explicitly
lists the test files, avoiding assumptions about Node's default discovery:

```sh
python3 cf/scripts/test_llm_fixture.py
python3 cf/scripts/test_llm_fixture.py --repo /path/to/final-checkout --acceptance
python3 cf/scripts/test_llm_fixture.py --repo /path/to/final-checkout --acceptance --with-discount
```

Each provider uses a separate clone/fork and its own agent key. A shared checkout
or shared Git worktree metadata would introduce local ref contention unrelated
to the sequencer. Save real plans, decisions, transcripts, provider/model
versions, execution intervals, checks, and conflict-recovery receipts. Never
commit local credentials or provider configuration.

The seeded nerudo.toml enables the Worker's integrated TypeScript check. The
orchestrator inserts a real public maintainer key before initializing the repo.
Unit tests and the final Node oracle run locally and are recorded as separate
evidence; the deployed Worker does not enforce these runtime tests.
