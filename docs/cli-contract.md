# CLI contract

This document defines version 1 of Coterie's command surface, programmatic
output, retry behavior, authentication, and process exit codes.

## Commands

Generated `--help` output is authoritative for argument spelling. The reference
below covers every public command in the MVP. Commands other than the foreground
launch require an active run and never create one as a side effect. Every
subcommand accepts the global `--json` option; mutating commands also accept
`--operation-id <co-ULID>` as shown below.

### `coterie`

```console
coterie [--operation-id <co-ULID>]
```

Discover the current project, start or reconnect to its supervisor, and launch
the foreground Codex TUI. Codex inherits the working directory and terminal
streams, so Coterie prints no wrapper response while the TUI owns the terminal.
Coterie injects its orchestration bootstrap through Codex's
`developer_instructions` setting, while leaving normal project instruction
discovery—including `AGENTS.md`—intact.

The foreground launch does not accept `--json`. A foreground operation ID is
only for retrying an uncertain launch and cannot be combined with a subcommand.
Closing the TUI does not stop the run or its workers, and `SIGINT` is forwarded
to Codex rather than interpreted as `coterie stop`.

### `coterie status`

```console
coterie status
```

Summarize the active run, project, agents, and task-state counts. Full status is
operator-only.

### `coterie whoami`

```console
coterie whoami
```

Report whether the authenticated caller uses the operator or agent channel and,
for an agent, return its durable identity. Both callers may use this command.

### `coterie prime`

```console
coterie prime
```

Reconstruct the caller's identity, project, peers, tasks, ready work, active
assignment, and authorized command list. Agents use this durable context after
a fresh session or context compaction.

### `coterie task create`

```console
coterie task create <title> [--description <text>] [--project <alias>]
  [--group <name>] [--after <task-id>]... [--operation-id <co-ULID>]
```

Create a durable task. The description defaults to the title, and the target
project defaults to `primary`. Each repeated `--after` task must close before
the new task becomes ready. `--group` records a run-local task-group name. The
operator or an agent with `task:create` may call this mutation.

### `coterie task ready`

```console
coterie task ready
```

List tasks that are open, have no unresolved dependency, and have no active
claim. The operator or an agent with `task:read` may call this command.

### `coterie task close`

```console
coterie task close <task-id> --summary <text>
  [--operation-id <co-ULID>]
```

Close a submitted task after explicit validation. A submitted worktree task
must have a recorded successful integration. The summary and exact assignment
and target commits remain in the closed result. If a precondition fails, correct
it and use a new operation ID because the rejected attempt is itself durable.
The operator or an agent with `task:close` may call this mutation.

### `coterie spawn`

```console
coterie spawn <role> --task <task-id> [--operation-id <co-ULID>]
```

Instantiate a configured background role and atomically claim one ready task.
The built-in MVP roles available for spawning are `worker` and `reviewer`,
subject to their role and run limits. A Git-backed `worker` receives an isolated
worktree; a `reviewer` receives an enforceable read-only workspace. The operator
or an agent with `spawn:<role>` may call this mutation.

Workers run as supervised `codex exec --json` jobs. Validated JSONL frames are
appended to the session transcript as they arrive; malformed or oversized
frames quarantine the session instead of being treated as successful work.

### `coterie workspace integrate`

```console
coterie workspace integrate --assignment <assignment-id>
  [--operation-id <co-ULID>]
```

Apply a successfully completed worktree assignment whose task is `submitted`.
Before recording durable intent, Coterie verifies the assignment worktree and
target repository identities, requires both worktrees to be clean, requires the
recorded result to be the assignment tip with a linear history from its base,
captures the target branch and tip, and preflights any merge without changing
the target. Applying the plan uses a compare-and-set reference update, so a
changed target, conflict, or ambiguous history is refused without resolution.

The success response records the target reference, base commit, result commit,
target commit before integration, and resulting target commit. Integration does
not remove the worktree or its owned reference. The operator or an agent with
`workspace:integrate` may call this mutation.

### `coterie finish`

```console
coterie finish --status <completed|failed> --summary <text>
  [--operation-id <co-ULID>]
```

Finish the authenticated agent's active assignment. `completed` records its
result commit when applicable and moves the task to `submitted`; it does not
close the task. `failed` releases the assignment and reopens the task. Only an
assigned agent may call this mutation.

### `coterie send`

```console
coterie send <agent-id-or-name> <message> [--operation-id <co-ULID>]
```

Persist a message and its event for an agent before any live delivery attempt.
The Codex MVP does not claim live steering, so the recipient obtains the message
from its durable inbox. The operator may address any agent; agent-to-agent
delivery is bounded by the sender's `send:*` capabilities.

### `coterie inbox`

```console
coterie inbox [--after <cursor>]
```

Read the authenticated agent's durable messages after a recipient-local cursor,
which defaults to 0. The response returns `next_cursor`; reading does not
acknowledge any message. The operator has no agent inbox.

### `coterie inbox ack`

```console
coterie inbox ack <cursor> [--operation-id <co-ULID>]
```

Acknowledge every message through a cursor previously returned to the
authenticated agent. The acknowledgement is idempotent and never moves the
durable acknowledgement point backward. The operator has no agent inbox.

### `coterie logs`

```console
coterie logs <agent-id-or-name>
```

Read the latest provider transcript visible to the caller. The operator may
inspect any agent. An agent may inspect itself and any role allowed by its
`logs:*` capabilities.

### `coterie events`

```console
coterie events [--after <cursor>] [--limit <1..1000>]
```

Read immutable, typed run events in increasing run-local sequence order. The
cursor defaults to 0, and the limit defaults to 100. Each event carries its
applicable durable IDs, optional correlation and causation IDs, and a versioned
payload. Full event-stream inspection is operator-only.

### `coterie stop`

```console
coterie stop [--operation-id <co-ULID>]
```

Stop the active run safely. Coterie rejects new spawns, interrupts workers,
requests foreground termination, and explicitly terminates survivors after a
bounded grace period. It marks the run stopped and retires its socket and
project index only after every controlled process is terminal. If it cannot
prove that outcome before the timeout, the command fails and leaves the run
active. Shutdown does not delete assignment worktrees or owned references. Only
the operator may call this mutation.

## Recovery

The supervisor is disposable, but orchestration state is not. The run database,
transcript files, and worktrees live beneath
`$XDG_STATE_HOME/coterie/runs/<run-id>/`, falling back to
`$HOME/.local/state/coterie/runs/<run-id>/`. The Unix socket and project lease
live beneath `$XDG_RUNTIME_DIR/coterie/`; the project index under the state
directory is coordination metadata, not the source of truth.

Closing or interrupting the foreground Codex TUI leaves the run and background
workers active. A later `coterie` invocation finds the same run and starts a
fresh lead session; the Codex MVP does not promise transparent TUI reattachment
or provider-session resume. The injected bootstrap directs the lead to
`coterie prime`, which reconstructs its context from durable state.

After a supervisor crash, the next foreground launch reconnects when possible
or restarts the same run from its database, lease, and index. Startup removes a
stale owned socket, republishes the same run and project identities, repairs a
workspace whose durable creation intent was interrupted, and rechecks session
state. A vanished worker becomes `lost`; a process Coterie cannot prove belongs
to the recorded generation remains `unknown` and is neither adopted nor killed.
Tasks, dependencies, operations, messages, events, transcripts, and recoverable
workspaces remain available.

Coterie never automatically deletes a dirty, unintegrated, running, lost, or
ambiguously owned assignment worktree. Use `status`, `prime`, `logs`, and
`events --json` to inspect durable state, and retry an uncertain mutation with
the same operation ID. After `coterie stop` completes, a later foreground launch
creates a new active run; the stopped run's durable state is retained.

## Trust model

The MVP has two authority planes. Commands issued by a human in an attached
project use a local operator channel. Commands issued inside a provider session
use a distinct agent channel authenticated by a random token scoped to the run,
agent, session, and generation. Agent names never confer identity. A missing,
partial, stale, or invalid agent environment is an authentication failure and
does not fall back to operator authority.

The operator channel is not a hostile security boundary against another process
running as the same Unix user. A same-UID process may be able to inspect process
state or user-readable files. Coterie uses private runtime files and scoped
tokens to prevent accidental authority confusion; stronger isolation requires
separate operating-system identities or containers and is outside the MVP.

The current operator policy is the sealed `builtin:standard@1` archetype. The
MVP does not yet load global or project configuration. Its provider executable,
role definitions, permission profiles, capabilities, and limits are trusted
compiled policy. Repository contents, `AGENTS.md`, task and message text,
provider output, and agent behavior are untrusted data. Coterie passes provider
arguments as arrays, never through `sh -c`, and performs its own repository
operations with `git2`.

| Role | Workspace | Codex filesystem | Network tools | Approvals |
| --- | --- | --- | --- | --- |
| Foreground `lead` | Primary project | Workspace write | Provider default | On request |
| Background `worker` | Isolated task worktree | Workspace write | Disabled | Never |
| Background `reviewer` | Task project | Read-only | Disabled | Never |

Workspace isolation prevents concurrent Git changes from colliding, but it is
not a security boundary by itself. Coterie probes Codex's actual command-line
capabilities before relying on its filesystem, network, and approval controls
and refuses to launch when a required control cannot be enforced.

The foreground Codex process inherits the operator's terminal and ambient
environment. Background Codex jobs start from an empty environment and receive
only `PATH`, `HOME`, `CODEX_HOME`, and `OPENAI_API_KEY` when present, plus their
`COTERIE_*` identity values. The raw Coterie token exists only in the session
environment; the database stores a verifier. Coterie redacts that known token
from transcripts it controls, but cannot sanitize a provider's separate storage.

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
