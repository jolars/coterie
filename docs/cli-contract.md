# CLI contract

This document defines version 1 of Coterie's command surface, programmatic
output, retry behavior, authentication, and process exit codes.

## Commands

Running `coterie` without a subcommand starts a supervisor when necessary and
launches the foreground Codex TUI in the project directory. Codex inherits the
terminal streams, so Coterie does not print a wrapper response around the TUI.
Coterie supplies its orchestration bootstrap through Codex's
`developer_instructions` setting; Codex otherwise performs its normal project
instruction discovery, including the repository's `AGENTS.md`. Background
sessions continue to use the deterministic fake provider during this M3 slice.

The minimum delegation commands are:

| Command | Purpose |
| --- | --- |
| `coterie` | Launch or reconnect to the foreground agent and active run. |
| `coterie status` | Summarize the active run, attached projects, agents, and task states. |
| `coterie whoami` | Report the authenticated operator or agent identity. |
| `coterie prime` | Reconstruct identity, projects, peers, ready work, the active assignment, and available commands. |
| `coterie task create <title>` | Create a durable task in the primary project or the alias selected with `--project`. |
| `coterie task ready` | List open, unblocked, unclaimed tasks. |
| `coterie task close <task-id> --summary <text>` | Close a submitted task after explicit validation. |
| `coterie spawn <role> --task <task-id>` | Instantiate an authorized configured job role and atomically claim a ready task. |
| `coterie finish --status <completed\|failed> --summary <text>` | Submit or release the authenticated agent's active assignment. |
| `coterie workspace integrate --assignment <assignment-id>` | Explicitly apply a submitted worktree result after guarded Git preflight. |
| `coterie send <agent> <message>` | Persist a message for an agent ID or run-local name. |
| `coterie inbox [--after <cursor>]` | Read the authenticated agent's messages after a monotonic cursor. |
| `coterie inbox ack <cursor>` | Explicitly acknowledge every message through a returned inbox cursor. |
| `coterie logs <agent>` | Read the latest provider transcript visible to the caller. |
| `coterie events [--after <cursor>] [--limit <n>]` | Read typed run events after a monotonic cursor. |
| `coterie stop` | Stop the active run and wait for its index and socket to retire. |

`task create` also accepts `--description`, `--group`, and repeated `--after`
task IDs. Generated help is authoritative for argument spelling and defaults.
Commands other than the foreground launch require an active run and never
create one as a side effect.

`workspace integrate` requires a successfully completed assignment whose task is
`submitted`. Before recording durable intent, it verifies the assignment
worktree and target repository identities, requires both worktrees to be clean,
requires the recorded result to be the assignment tip with a linear history from
its recorded base, captures the target branch and tip, and preflights any merge
without changing the target. Applying the durable plan uses a compare-and-set
reference update, so a changed branch or tip is refused. Conflicts and ambiguous
histories are left untouched for the lead or operator to resolve explicitly.
The success response records the target reference, base commit, result commit,
target commit before integration, and resulting target commit. Coterie preserves
the assignment worktree and its owned reference; cleanup is not implicit in
integration or shutdown while work may remain recoverable.

`send` commits the message and its normalized event before any provider delivery
can be attempted. Inbox reads never acknowledge messages implicitly. A caller
passes the returned `next_cursor` to `inbox ack`; acknowledgements are
operation-ID idempotent, acknowledge every message through that cursor, and
never move the durable acknowledgement point backward. The M2 fake provider
does not advertise live steering, so agents receive these messages through the
durable inbox.

The operator connects through the local operator channel. An agent invocation
must carry all of `COTERIE_AGENT_ID`, `COTERIE_SESSION_ID`, and `COTERIE_TOKEN`;
`COTERIE_RUN_ID`, when present, must match the active run. A partial or invalid
agent environment is an authentication error and never falls back to operator
authority. Agent names do not affect authentication.

## JSON output

Programmatic commands selected with `--json` emit exactly one compact JSON
object followed by a newline. A successful response goes to standard output,
and standard error remains empty. A failed response goes to standard error,
and standard output remains empty. Human-readable diagnostics also go to
standard error, but do not share a stream with successful JSON.

The interactive foreground launch does not accept `--json`, because Codex owns
its standard streams for the lifetime of the TUI. Coterie rejects that
combination before creating a run.

Every response contains `"schema_version": 1`. A read-only success places its
command-specific result under `data`:

```json
{"schema_version":1,"data":{"status":"active"}}
```

A mutation success also contains the operation ID:

```json
{"schema_version":1,"operation_id":"co-01ARZ3NDEKTSV4RRFFQ69G5FAV","data":{"task_id":"ct-01ARZ3NDEKTSV4RRFFQ69G5FAV"}}
```

An error places its stable code, human-readable message, and any
error-specific fields under `error`. `details` is omitted when empty:

```json
{"schema_version":1,"error":{"code":"invalid_argument","message":"task ID is invalid","details":{"argument":"task_id"}}}
```

Once a mutation has an operation ID, its error response includes that ID at the
top level. Callers must branch on `error.code`, not on the message.

The generated JSON Schemas are:

- [`cli-success-v1.schema.json`](../schemas/cli-success-v1.schema.json)
- [`cli-mutation-success-v1.schema.json`](../schemas/cli-mutation-success-v1.schema.json)
- [`cli-error-v1.schema.json`](../schemas/cli-error-v1.schema.json)
- [`cli-mutation-error-v1.schema.json`](../schemas/cli-mutation-error-v1.schema.json)

## Operation IDs

Every mutating CLI command accepts the common
`--operation-id <co-ULID>` option. If it is omitted, the CLI generates an
operation ID before dispatch. The RPC request carries that ID, and every
Coterie-rendered response after allocation returns it. The foreground launch
uses its operation ID to prepare the durable session, but emits no wrapper
response while Codex owns the terminal. A programmatic caller retries an
uncertain mutation with the same ID. Read-only commands neither accept nor
return an operation ID.

An integration retry reuses its original preflight plan. It succeeds
idempotently if that plan already advanced the target, and it refuses a target
changed by another actor rather than silently replanning under the same
operation ID.

`events` returns immutable records in increasing run-local sequence order. Each
record includes its run ID, applicable project, agent, task, and operation IDs,
optional correlation and causation event IDs, and a payload with its own
`schema_version`. Retrying an already applied mutation does not append duplicate
events.

Errors detected before an operation ID can be parsed or allocated use the
ordinary error envelope without `operation_id`.

## Exit codes

Exit codes describe broad handling categories; `error.code` supplies the
specific machine-readable cause.

| Code | Category | Meaning |
| ---: | --- | --- |
| 0 | `success` | The command completed successfully. |
| 1 | `internal` | Coterie encountered an internal failure or corrupt state. |
| 2 | `usage` | A command-line argument or request value was invalid. |
| 3 | `configuration` | Configuration was invalid or incompatible. |
| 4 | `not_found` | A requested resource does not exist. |
| 5 | `conflict` | Current state does not satisfy an operation precondition. |
| 6 | `permission` | Authentication or authorization failed. |
| 7 | `unavailable` | A required service or provider cannot currently respond. |

The versioned machine-readable table is
[`cli-exit-codes-v1.json`](../tests/golden/cli-exit-codes-v1.json).

The version 1 error codes map as follows:

| Error code | Exit category |
| --- | --- |
| `invalid_argument` | `usage` |
| `invalid_configuration` | `configuration` |
| `not_found` | `not_found` |
| `conflict` | `conflict` |
| `unauthenticated` | `permission` |
| `permission_denied` | `permission` |
| `unavailable` | `unavailable` |
| `corrupt_state` | `internal` |
| `internal` | `internal` |
