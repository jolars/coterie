# CLI commands

Run `coterie --help` or `coterie <command> --help` for authoritative option spelling and defaults. This page covers every public command and the important behavior behind it. Commands other than foreground launch, `doctor`, `config`, and `run` require an active run. The [detailed CLI contract](https://github.com/jolars/coterie/blob/main/docs/cli-contract.md) records pagination, authorization, failure, and recovery rules.

Most subcommands accept global `--json` for a versioned response. Mutations accept `--operation-id <co-ULID>`; retry an uncertain result with the same ID and identical arguments. The interactive foreground launch cannot use `--json` because Codex owns its terminal streams. See [JSON and exit codes](./protocol).

## Launch and runs

### `coterie`

Start or reconnect to the current project's run and open the foreground Codex TUI. Closing that TUI leaves an active run and its workers intact. Launch accepts configuration overrides, including `--archetype`, ceilings, and role restrictions within trusted bounds. `--operation-id` is available to retry an uncertain launch.

### `coterie run list`

List retained runs attached to the current project, including stopped runs. `--json` returns recorded identities, status, roots, and task counts. An `active` record is desired state, not proof that a supervisor is alive.

### `coterie run recover`

`coterie run recover <run-id> --reason <text>` reactivates a stopped run as the operator. It verifies saved policy, project leases, process exits, and retained ownership. Run `coterie` afterward for a fresh foreground session. See [Recovery](/guide/recovery).

### `coterie stop`

Request bounded shutdown of the active run. Coterie preserves unfinished tasks, transcripts, and workspaces. A timeout leaves the run available for inspection and retry with the same operation ID.

## Configuration and diagnostics

### `coterie config check`

Validate resolved configuration and verify an existing `coterie.lock` without starting a run or provider. Use `--json` for a fingerprint and lock status.

### `coterie config show`

Inspect configuration. `--effective` selects the resolved policy, and `--provenance` adds the source of each value. `--json` uses the common success envelope.

### `coterie config schema`

Print a generated JSON Schema. `--target project|global|lock|effective` selects its type; project is the default. Without `--json`, the schema is printed directly for saving to a file.

### `coterie config lock`

Explicitly create or replace the current project's portable `coterie.lock`. `check` and `show` never change it. This local file operation has no orchestration operation ID.

### `coterie status`

Summarize the active run, agents, projects, and task counts. Full status is operator-only.

### `coterie doctor`

Inspect provider compatibility, supervisor and runtime state, configuration, ownership, and recovery concerns without modifying them. A report can exit 0 while individual checks say `warning`, `error`, or `unavailable`; inspect the check statuses. See [Troubleshooting](/guide/troubleshooting).

## Projects and tasks

### `coterie project list`

List the active run's attached projects, aliases, canonical roots, and access. Attachment does not expand provider filesystem permissions.

### `coterie project attach`

`coterie project attach <path> [--alias <name>]` adds a canonical root to the active run. The operator can attach explicitly; agents need `project:attach` and a trusted allowed root. Current attachment does not enable the unfinished cross-project task workflow.

### `coterie task create`

`coterie task create <title>` records a task. `--description` adds detail, `--project` records a target alias, `--group` groups related work, and repeated `--after <task-id>` values add dependencies. Dependencies become ready only after their tasks close. Assignment targeting across attached projects is still under development.

### `coterie task ready`

List open tasks with no unresolved dependencies or active claim.

### `coterie task show`

`coterie task show <task-id>` reads the full stored task document in revision-checked text pages. Continue with `--after <next_cursor>` and the same `--revision`, then concatenate `data.text` before decoding JSON. Recorded reports are not fresh validation probes.

### `coterie task close`

`coterie task close <task-id> --summary <text>` accepts a submitted task after validation. Git worktree submissions require recorded integration. The operator-only `--override` form records an explicit external integration with both full commit IDs, a reason, and validation evidence; see the [detailed contract](https://github.com/jolars/coterie/blob/main/docs/cli-contract.md#operator-closure-override).

### `coterie task resubmit`

Correct an unintegrated Git submission through an authorized coordinator. Supply `--assignment`, `--expected-result`, `--result`, `--summary`, and `--reason`. The corrected commit must be the clean owned worktree tip and descend from the recorded result. Integration and closure still follow.

### `coterie task submit-retained`

Submit a clean, independently reviewed commit after a verified worker exit without starting a replacement worker. Requires operator authority or `task:submit-retained`. Supply `--assignment`, `--result` (full lowercase commit ID), `--summary` (validation outcomes and blocked checks), `--reason`, `--review`, and `--review-source`. The original assignment must still own its claim, and its clean worktree tip must match the reviewed commit. Source files and index stay intact. Integration, target validation, and explicit closure still follow. See [Recovery](/guide/recovery#submit-a-retained-commit).

### `coterie task recover`

`coterie task recover --assignment <id> --reason <text>` retires an exited, unsubmitted Git assignment and reopens its task. `--report <JSON>` optionally records sourced validation evidence and unfinished steps. For a lost worker with no observed exit, the local operator may add `--acknowledge-lost`; Coterie requires a fresh process-absence check and records the missing evidence. Source files, commits, and references remain preserved; a continuation gets a new worktree. See [Recovery](/guide/recovery).

### `coterie assignment show`

`coterie assignment show <assignment-id>` returns the full report, workspace identity, and recovery handoffs in revision-checked JSON document pages. Its `--after`, `--revision`, and `--limit` controls work like `task show`.

## Agents and workspaces

### `coterie whoami`

Report whether the authenticated caller is the operator or an agent, with its durable identity.

### `coterie prime`

Return compact current context: identity, peers, projects, tasks, ready IDs, and authorized commands. `--after-task` and `--limit` page tasks. Full task and assignment detail is available through `show`; `prime` intentionally bounds previews.

### `coterie progress`

Read compact lifecycle changes. Save `next_cursor`, continue while `has_more` is true, and use `--wait <0..5>` for bounded waiting. A provider exit, assignment completion, and task closure are separate changes. Progress does not acknowledge inbox messages.

### `coterie spawn`

`coterie spawn <role> --task <task-id>` atomically claims a ready task and starts a configured background role. The built-in archetype offers writable `worker` and read-only `reviewer` roles, subject to policy and limits.

### `coterie workspace integrate`

`coterie workspace integrate --assignment <id>` applies an accepted submitted worktree result. Rebase is the default strategy; `--strategy merge` selects a merge. Dirty, moved, ambiguous, or conflicting targets are refused. Integration does not clean up the assignment worktree.

### `coterie finish`

`coterie finish --status completed|failed --summary <text>` is for the assigned agent. Completion submits a clean result; failure reopens the task and preserves work. It does not integrate or accept the task.

## Messages and inspection

### `coterie send`

`coterie send <agent-id-or-name> <message>` stores a durable message before live delivery. Agent send authority depends on its configured capabilities.

### `coterie inbox`

Read the authenticated agent's durable messages after an optional `--after <cursor>`. Reading does not acknowledge them; the operator has no agent inbox.

### `coterie inbox ack`

`coterie inbox ack <cursor>` idempotently acknowledges every message through a cursor previously returned to that agent.

### `coterie logs`

`coterie logs <agent-id-or-name>` reads a provider transcript. `--tail` reaches recent output, `--after` resumes a byte cursor, `--session` pins a session, and `--follow` streams later pages. `command_event_coverage` is `provider_emitted_only` for background jobs: Codex may omit a `command_execution` item for a command run through its code tool. An absent item does not prove the command never ran. Foreground sessions report `not_captured` because Codex owns their terminal streams. Transcript text is evidence of output, not proof of task acceptance.

### `coterie events`

Read typed run events in sequence. `--after` resumes a cursor, `--limit` bounds a page, and `--follow` waits for more. Full event inspection is operator-only.
