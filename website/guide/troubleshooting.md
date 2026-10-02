# Troubleshooting

Start with `coterie doctor`. It checks provider compatibility, configuration, supervisor reachability, durable state, worktree ownership, and uncertain sessions without changing them. A diagnostic report can exit successfully while individual checks still say `warning`, `error`, or `unavailable`; read those check statuses.

## Codex cannot launch

Run `codex --version` and confirm that it reports `codex-cli` 0.153.4 or later, below 1.0.0. Run Codex directly once to complete authentication. Then use `coterie doctor` to check the version and required CLI and MCP capabilities. Coterie refuses to weaken a requested sandbox or approval policy when the installed provider cannot enforce it.

## Configuration changed during a run

A run snapshots its effective policy. `invalid_configuration` at launch means current files or overrides conflict with that snapshot. Inspect `coterie config show --effective --provenance`, restore the original policy and overrides, or explicitly stop the run before starting with a new policy. `config check` verifies a lock without changing it; `config lock` writes a new lock only when you choose to do so.

## A worker submitted, but the task is not done

Submission is not acceptance. Inspect the task and assignment, review the Git result, run `workspace integrate` for an accepted worktree contribution, validate the target, and close the task. `events` and `logs` show what happened, but a provider exit does not prove success. See [Tasks and workspaces](./tasks-and-workspaces).

## The CLI and supervisor disagree after an upgrade

An existing supervisor keeps running the executable that started it. If the new CLI reports a protocol mismatch, use the matching old executable to inspect and stop that run when ready, then launch the new Coterie. The stopped run retains its tasks, transcripts, and workspaces. The [CLI contract](https://github.com/jolars/coterie/blob/main/docs/cli-contract.md) describes the Linux process lookup and recovery steps.

## A process or worktree is uncertain

Use `doctor`, `status`, `events --json`, and the relevant transcript. Coterie will not infer liveness from a PID file or delete a dirty, unintegrated, or ambiguously owned worktree. Resolve the reported condition or use the [recovery workflow](./recovery). Retry an uncertain mutation with its original operation ID and identical arguments.
