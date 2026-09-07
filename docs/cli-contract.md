# CLI contract

This document defines version 1 of Coterie's command surface, programmatic
output, retry behavior, authentication, and process exit codes.

## Commands

Running `coterie` without a subcommand starts a supervisor when necessary and
launches or reconnects to the foreground agent. During M2, foreground and
background sessions use the deterministic fake provider; this exercises the
complete command and persistence boundary without model access.

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
| `coterie send <agent> <message>` | Persist a message for an agent ID or run-local name. |
| `coterie inbox [--after <cursor>]` | Read the authenticated agent's messages after a monotonic cursor. |
| `coterie logs <agent>` | Read the latest provider transcript visible to the caller. |
| `coterie events [--after <cursor>] [--limit <n>]` | Read typed run events after a monotonic cursor. |
| `coterie stop` | Stop the active run and wait for its index and socket to retire. |

`task create` also accepts `--description`, `--group`, and repeated `--after`
task IDs. Generated help is authoritative for argument spelling and defaults.
Commands other than the foreground launch require an active run and never
create one as a side effect.

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
response after allocation returns it. A programmatic caller retries an
uncertain mutation with the same ID. Read-only commands neither accept nor
return an operation ID.

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
