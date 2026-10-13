# Concurrent workflow brief

This dedicated repository demonstrates several real coding agents improving the
same small pricing library. Preserve each other agent's completed behavior while
implementing the task assigned to your provider in TASKS.json.

- Work through the configured NERUDO MCP tools: inspect main and work in flight,
  implement in your own checkout and fork, submit, and read the landing receipt.
  Never push to main directly.
- Read the current source before editing. Several tasks share the price API. If
  NERUDO reports a conflict or integrated type-check failure, read its receipt,
  inspect the new main and related preserved context, then repair and resubmit
  with a new revision. Do not discard another agent's requirements to make your
  own task pass.
- Preserve your actual plan, relevant decisions, test results, and any recovery
  in NERUDO context for every submitted revision. Describe what you did; do not
  claim checks or subagent work that did not run.
- Add or update tests for your task and run `npm run run_checks` before submission.
  The acceptance oracle in `checks/acceptance.mjs` is immutable task evidence.
  Do not edit, delete, skip, or weaken anything in checks/, or change the task
  requirements in TASKS.json.
- Changes to AGENTS.md, TASKS.json, nerudo.toml, tsconfig.json, package.json, or
  checks/ need a real maintainer approval. Do not modify them for these feature
  tasks.
- Local authentication, tokens, private keys, logs, provider configuration, and
  MCP connection settings must never be committed. Do not print secret values.

The Worker checks TypeScript on the integrated tree. Local Node runtime tests and
the final oracle are separate checks; never describe them as Worker-enforced CI.

Ollama may delegate to subagents. Give each a scoped task and a separate model
conversation. Read-only planners/reviewers may inspect the primary's checkout;
any subagent that writes needs an independent checkout. Record its provider/model,
contribution, and real execution in context.
