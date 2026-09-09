# CLI contract

This document defines version 1 of Coterie's command surface, programmatic
output, retry behavior, authentication, and process exit codes.

## Commands

Generated `--help` output is authoritative for argument spelling. The reference
below covers every implemented public command. Commands other than the foreground
launch, `doctor`, and `config` require an active run and never create one as a side
effect. Every subcommand accepts the global `--json` option; mutating run
commands also accept
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

### `coterie config check`

```console
coterie config check [--json]
```

Configuration commands run without an active run, runtime directories, or an
installed provider. They discover the project root and resolve compiled defaults,
trusted global configuration and includes, the selected archetype, and project
restrictions. `schema` does not discover a project or read configuration.
Launches, recovery, and `doctor` configuration compatibility still use the
existing compiled runtime policy until M5 snapshot integration is complete.

`check` validates configuration and verifies `coterie.lock` if present. Its JSON
data contains `archetype`, the portable `fingerprint`, and `lock` (`absent` or
`verified`).

### `coterie config show`

```console
coterie config show [--effective] [--provenance] [--json]
```

`show` returns `effective`, `fingerprint`, and `lock`; adding
`--provenance` includes a field-to-origin map with source layer, source file,
input field, and optional selector origin. It verifies an existing lock before
emitting output. Provider commands are visible in effective configuration, with
known credentials and Coterie tokens redacted before JSON encoding.

### `coterie config schema`

```console
coterie config schema [--target project|global|lock|effective] [--json]
```

`schema` defaults to the project input schema. Without `--json`, it prints the
schema directly as pretty JSON, suitable for redirecting to a file. With
`--json`, it places the schema under the common success envelope's `data` field.
Available schemas are generated from Rust types:

- [`config-project-v1.schema.json`](../schemas/config-project-v1.schema.json)
- [`config-global-v1.schema.json`](../schemas/config-global-v1.schema.json)
- [`config-lock-v1.schema.json`](../schemas/config-lock-v1.schema.json)
- [`config-effective-v1.schema.json`](../schemas/config-effective-v1.schema.json)

### `coterie config lock`

```console
coterie config lock [--json]
```

`lock` explicitly creates or replaces the project root's `coterie.lock`. Its
JSON data is the newly written lock. It resolves current configuration even when
the old lock is invalid or mismatched. Locks record the archetype, configuration
schema, compatible Coterie version range, enabled roles' provider mode and
permission requirements, and a SHA-256 fingerprint. Commands and arguments,
environment values, provenance paths, project identity, and installed provider
versions are excluded. The digest includes the complete selected archetype,
effective roles, limits, supervision policy, and provider requirements. The
[configuration design](../DESIGN.md#declarative-configuration) specifies canonical
encoding; the [example lock](../examples/config/coterie.lock) corresponds to the
global and project examples in that directory.

The lock is bounded to 1 MiB and published by syncing a private temporary file,
renaming it atomically, and syncing the directory. Existing symlinks, hard
links, and nonregular files are refused. A failed or interrupted attempt may
leave the old or complete new lock and a temporary file for inspection. Retrying
`config lock` with unchanged inputs writes the same content. This local file
command does not allocate an orchestration operation ID or mutate run state.

Invalid configuration, unreadable inputs, malformed locks, and mismatches use
`invalid_configuration` (exit 3). A mismatch lists the affected lock fields and
suggests restoring the configuration or reviewing its files and running
`coterie config lock`. A Coterie version mismatch also suggests using a compatible
release. `check` and `show` never modify a lock. Human validation and creation
messages go to standard output; human effective reports are pretty JSON.
Failures use standard error and leave standard output empty.

### `coterie status`

```console
coterie status
```

Summarize the active run, project, agents, and task-state counts. Full status is
operator-only.

### `coterie doctor`

```console
coterie doctor [--json]
```

Inspect supervisor reachability, the project lease and index, runtime file
ownership and permissions, database integrity and migrations, pending operations,
unfinished assignments, uncertain sessions, task cycles, transcript accessibility
and incomplete tails, and worktree ownership. Provider checks probe the installed
Codex version and required capabilities without launching a model session.
Configuration and lock files are reported as unverified when present until M5
runtime snapshot integration is complete.

Doctor is operator-only and never starts a supervisor, migrates a database,
changes permissions, signals a process, or removes work. If the supervisor is
unreachable, it opens an existing private database read-only. Insecure or
ambiguous paths remain untouched. A successful diagnostic report exits 0 even
when individual checks report `warning`, `error`, or `unavailable`; inspect the
`report.checks` statuses. An inability to run the command uses the ordinary error
contract. The [doctor report schema](../schemas/doctor-report-v1.schema.json) is
generated from its Rust type.

Launch `coterie` to attempt conservative recovery of an indexed run. Recovery
requires the exclusive project lease, an existing private run database, and
matching durable project identity. It replaces a socket only after a refused
connection proves that the socket is stale. A held lease, responsive mismatched
socket, missing database, or inconsistent index must remain for inspection.
Stopped runs retire only their coordination metadata; their durable work remains.

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
Checkout preserves ignored files, including files that collide with the result.
Assume-unchanged or skip-worktree index flags cause a conflict diagnostic because
they prevent proof of cleanliness. A redirected Git working directory or a
symlink substituted into an owned workspace path also blocks integration.

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
coterie logs <agent-id-or-name> [--session <session-id>] [--after <byte-offset>]
  [--limit <1..65536>] [--follow]
```

Read the latest provider transcript visible to the caller. The operator may
inspect any agent. An agent may inspect itself and any role allowed by its
`logs:*` capabilities. Reads return at most 65,536 bytes by default, plus up to three bytes to keep a
UTF-8 character whole, with a
`next_cursor` byte offset, `session_id`, `eof`, `terminal`, and `incomplete_tail`.
Resume using both the returned session and cursor to avoid switching to a newer
session. A cursor beyond the file length is refused, including after truncation.
Incomplete final JSONL frames remain visible as transcript data and do not imply
success. Invalid UTF-8 is displayed with replacement characters; cursors always
count stored bytes.

`--follow` pins the first returned session and emits pages until its terminal
observation and end of file. A terminal page may contain no new bytes. A missing
transcript at offset zero represents no captured output; `doctor` distinguishes
missing background output from inherited foreground terminal streams.

### `coterie events`

```console
coterie events [--after <cursor>] [--limit <1..1000>] [--follow]
```

Read immutable, typed run events in increasing run-local sequence order. The
cursor defaults to 0, and the limit defaults to 100. Each event carries its
applicable durable IDs, optional correlation and causation IDs, and a versioned
payload. Pages have a 900 KiB byte budget, so they may contain fewer records
than the requested limit. Mutations that would create a larger event return
`invalid_argument` before committing. Older events exceeding this budget are
returned individually. Each response returns `next_cursor`, which can be passed
to `--after` to resume without replaying earlier events. Full event-stream
inspection is operator-only.

`--follow` emits nonempty pages as they become available and drains the stopped
run through a final empty page before exiting. Operator followers reconnect to
the same run for up to five seconds after a transient disconnect, without
starting a supervisor or switching to a replacement run. If shutdown retires
the socket before the next poll,
operator followers read final immutable pages from the same stopped run. A
longer outage returns a diagnostic; resume with the last printed cursor. Agent transcript followers can resume explicitly after a
disconnect with their session and byte cursor.

### `coterie stop`

```console
coterie stop [--operation-id <co-ULID>]
```

Stop the active run safely. Coterie durably blocks foreground launches and
worker spawns, marks unfinished assignments `draining`, and interrupts every
controlled session. It sends `SIGTERM` after 250 milliseconds and `SIGKILL` to
verified survivors after 2.5 seconds. The foreground wrapper controls its own
child. Unknown processes are never signaled by PID alone.

The five-second deadline covers process control and terminal observation. A
timeout returns `unavailable` (exit 7), leaves the run active, and keeps launches
blocked. Inspect `events --json` and retry with the same operation ID to recheck
progress. Retries and supervisor restarts retain the original deadline. When
all processes are proved terminal, Coterie reconciles workspace observations,
marks the run stopped, and retires its socket and project index before releasing
the lease. It preserves unfinished tasks, claims, draining assignments,
transcripts, worktrees, and owned references. Only the operator may call this
mutation.

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
workspace whose durable creation intent was interrupted, resumes incomplete
spawn and integration operations, and rechecks session state. Each external
operation records its reconciliation state, attempts, last error, and last
attempt time. A vanished worker becomes `lost`; a process Coterie cannot prove
belongs to the recorded run and generation remains `unknown` and is neither adopted nor
killed. Tasks, dependencies, operations, messages, events, transcripts, and
recoverable workspaces remain available.

Sessions, assignments, workspaces, and integration intents retain their owning
run and generation. Replacing a session fences its old credentials, queued
requests, provider output, and exit observations. A new session cannot finish an
older generation's assignment or create or integrate its workspace. Such work
remains available for inspection. Recovery adopts a provider handle only when
its provider identity and complete session scope match the current durable
ownership; a PID alone does not establish that proof.

The compiled restart policy allows three launch attempts within 60 seconds,
with exponential retry delays starting at one second. Automatic retries require
proof that the failed attempt created no process. Exhausting this budget
quarantines the session and records `session.restart_limited`; repeated failures
before a session can be prepared also stop after three attempts. Three failed
foreground sessions in one window quarantine the latest session and block
replacement for 60 seconds. Quarantine and retry accounting survive supervisor
restarts. Workers that have already executed are left for the lead to inspect
and recover, preserving the original task and workspace ownership.

Provider probes time out after two seconds and cap each output stream at 1 MiB.
Session startup times out after 30 seconds; background jobs have a one-hour
execution limit. Foreground interactive sessions have no execution limit.
These limits do not treat a quiet provider as idle or successful. Session
timeouts use the interrupt, terminate, and kill phases described above and emit
`session.control_changed` events. Shutdown phases emit `run.shutdown_changed`.
Handshake waits are bounded to five seconds, and ordinary RPC responses to ten
seconds; the foreground process-control subscription remains a long poll.
External configuration of these compiled limits belongs to M5.

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

The current runtime operator policy is the sealed `builtin:standard@1` archetype.
Configuration inspection loads global and project files, but runtime adoption
remains separate M5 work. The runtime provider executable,
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
environment; the database stores a verifier. Coterie redacts the known token and passed `OPENAI_API_KEY` from controlled
transcripts, request text stored in tasks and messages, and diagnostics. Streaming
redaction retains possible credential prefixes across chunks and conceals an
unfinished prefix on terminal observation. Token-shaped values are also redacted
after a restart when the raw token is no longer in memory. Provider-managed
storage and inherited foreground terminal streams remain outside this filter.
Runtime directories must be owned by the current user with mode 0700; sockets,
lease and index files, SQLite files, and transcripts require mode 0600. Coterie
refuses symlinks, nonregular data files, hard-linked data files, and foreign
ownership. Existing owned application directories may be tightened at startup;
`doctor` reports their original permissions without changing them.

## JSON output

Programmatic commands selected with `--json` emit exactly one compact JSON
object followed by a newline, except `events --follow` and `logs --follow`, which
emit one such envelope per page. A successful response goes to standard output,
and standard error remains empty. A failed response goes to standard error,
and standard output remains empty for ordinary commands. If a follower fails
after printing pages, those pages remain on standard output and the final
diagnostic goes to standard error. Human-readable diagnostics also go to
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

Every mutating run command accepts the common
`--operation-id <co-ULID>` option. If it is omitted, the CLI generates an
operation ID before dispatch. The RPC request carries that ID, and every
Coterie-rendered response after allocation returns it. The foreground launch
uses its operation ID to prepare the durable session, but emits no wrapper
response while Codex owns the terminal. A programmatic caller retries an
uncertain mutation with the same ID. Read-only commands neither accept nor
return an operation ID. Local configuration lock creation also has no operation
ID; it uses explicit atomic file replacement as described above.

New mutations retain a fingerprint of the original request separately from
redacted request text, so retries survive provider credential changes. Older
operation records without a fingerprint retain their stored-request comparison.

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
