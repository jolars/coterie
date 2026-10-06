# Codex command events in worker transcripts

Coterie stores the validated JSONL frames that a background Codex process emits
on standard output. It does not inspect Codex's private session files or create
synthetic `command_execution` items from agent text, workspace changes, or a
process exit. `logs.command_event_coverage` reports `provider_emitted_only` for
these jobs. The absence of a command item cannot establish that a command did
not run. Foreground Codex owns its terminal streams, so its pages report
`not_captured`.

## Recorded investigation

The September 15, 2026 linked-worktree acceptance run retained a helper's own
result artifact because workers appeared to run shell commands through a code
tool without corresponding `command_execution` items. The run did not retain a
reviewable, complete pair of provider JSONL and command artifacts for that
specific claim. The observation remains open.

On October 6, 2026, an isolated `codex-cli 0.153.4` probe used local
authentication on NixOS. Both routes used `workspace-write`, network disabled,
and approvals set to `never`. The direct shell route executed a Python helper
that wrote `executed\n` to a test-owned artifact. Its complete `exec --json`
stream contained one matching completed `command_execution` item with exit code
zero. A second route enabled `code_mode` and `code_mode_only`, selected
`gpt-6-astra`, requested a nested `tools.exec_command` call, and produced the
same artifact and one matching completed item. A separate requested code-mode
cell ran two commands and produced both artifacts and two matching completed
items. These probes did **not** reproduce the reported omission. The CLI also
emitted an under-development feature warning; the prompts and feature flags do
not independently prove which internal tool path Codex selected.

The opt-in Coterie linked-worktree test was also run with Codex 0.153.4 and a
Coterie 0.2.0 development binary with SHA-256
`8308af65bbd949ea00364d438b49de0190b02e8cfba0a235ced07936f1a34daa`.
Its selected worker policy was `workspace-write`, network denied, and approvals
disabled. It timed out after 183 seconds while waiting for the worker handoff,
so that run supplies no command-event comparison or acceptance result. The
test's helper artifact and matching event count are printed when it reaches
the comparison point.

The opt-in raw-provider probe can be rerun with a Codex 0.153.4 executable first
on `PATH`, available local `auth.json` authentication, and model access:

```console
cargo test --test supervisor_runtime mcp::installed_codex_01534_command_event_probe -- --ignored --exact --nocapture
```

The [0.153.4 JSONL item definition](https://github.com/openai/codex/blob/rust-v0.153.4/codex-rs/exec/src/exec_events.rs)
defines `command_execution` items, but it does not promise an item for every
nested shell call. A complete diagnosis still needs a run that independently
proves a code-tool command result, pins the model, provider build, Coterie build,
and effective policy, and compares that result with the full provider stream
through process exit. Until then, an omitted item is an observability limit,
not evidence of nonexecution.
