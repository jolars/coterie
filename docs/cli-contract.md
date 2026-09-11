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
coterie [--operation-id <co-ULID>] [--archetype <REFERENCE>] [CONFIGURATION OPTIONS]
```

Discover the current project, start or reconnect to its supervisor, and launch
the foreground Codex TUI. Codex inherits the working directory and terminal
streams, so Coterie prints no wrapper response while the TUI owns the terminal.
Coterie injects its orchestration bootstrap through Codex's
`developer_instructions` setting, while leaving normal project instruction
discovery—including `AGENTS.md`—intact. Configured role instructions are included
in that provider bootstrap.

Startup and recovery use a durable snapshot of the resolved run configuration.
An incompatible file or operator override produces `invalid_configuration`
(exit 3), identifying the run, snapshot fingerprint, and changed effective fields.
Restore the original configuration and overrides, or stop the active run before
starting with new policy. Provider command changes conflict even when a portable
lock still verifies; provenance-only changes are compatible. Existing run
commands and foreground process control remain available if files change.

A supervisor keeps running the executable that started it after Coterie is
upgraded. If its RPC protocol differs from the current CLI, startup and run
commands fail with `unavailable` (exit 7) and recovery guidance. Use the matching
Coterie executable to inspect the run with `status`, then explicitly stop it
with `stop` when ready. On Linux, `ps -eo pid,args` shows the executable and run
ID for each `coterie __supervisor` process. For Nix installations, this is normally
the original `/nix/store/.../bin/coterie` path. Invoke that
executable from the affected project directory. After stopping, launch the
current `coterie` to create a new run. The stopped run retains its tasks,
transcripts, and workspaces, including unfinished work. Coterie does not
automatically stop an incompatible supervisor or discard its socket and index.

Startup, `config`, and `doctor` accept these configuration options:

| Option | Effective setting |
| --- | --- |
| `--archetype REFERENCE` | A trusted versioned archetype. |
| `--max-concurrent-agents N` | Simultaneously active agent ceiling. |
| `--max-agents-per-run N` | Total agent ceiling. |
| `--max-spawns-per-minute N` | Explicit spawns in a rolling 60-second window. |
| `--role ROLE.enabled=true\|false` | Enable or disable a declared role. |
| `--role ROLE.max_instances=N` | Active instances of a declared role. |
| `--role ROLE.permission_profile=NAME` | A trusted permission profile. |

Repeat `--role` to set multiple fields; the last assignment to a field wins.
Overrides may restore project restrictions only within trusted global and
archetype bounds. Other commands reject these flags with `invalid_argument`
(exit 2). Known credentials and Coterie tokens in launch configuration are
rejected before creating state; pass provider credentials through its supported
authentication environment.

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
restrictions, followed by bounded operator overrides. `schema` does not discover
a project or read configuration.

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
environment values, allowed project roots, provenance paths, project identity,
and installed provider versions are excluded. The digest includes the complete selected archetype,
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
Configuration and lock files are resolved and verified. An active run adds a
`configuration_snapshot` check and a compatibility check against current effective
values. These checks report errors without replacing the snapshot.

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

### `coterie progress`

```console
coterie progress [--after <cursor>] [--limit <1..100>] [--wait <0..5>] [--json]
```

Read compact lifecycle changes for the current run. The operator or an
authenticated agent with `task:read` may use this view. It shares `prime`'s
run-wide task and agent visibility, including the caller's own lifecycle, and
does not depend on role names or provider live steering.

The response contains `run_id`, `changes`, `next_cursor`, `has_more`, and
`timed_out`. Each change contains a durable sequence and one of these `kind`
values:

| Kind | Fields |
| --- | --- |
| `task` | `task_id`, `project_id`, `status` |
| `assignment` | `assignment_id`, `task_id`, `agent_id`, `state` |
| `assignment_session` | `assignment_id`, `task_id`, `agent_id`, `session_id` |
| `agent` | `agent_id`, `generation`, `state` |
| `session` | `session_id`, `agent_id`, `generation`, `state` |

These are historical changes, not a current-state snapshot. Apply them in
sequence order. Task `submitted` and assignment `completed` describe a
submission; agent or session `exited` describes provider lifecycle independently.
Task acceptance is recorded by task `closed`. Task titles, descriptions,
results, message bodies, paths, provider details, and raw event payloads are
excluded. An example containing submission and exit records is
[`examples/progress.json`](../examples/progress.json).

Omitting `--after` starts at sequence zero. Save the opaque returned cursor and
pass it unchanged on the next invocation. It is bound to this run and caller,
so a different agent, the operator, or a replacement run cannot reuse it. A
renewed session for the same agent may reuse it after authenticating with its
new credentials. A malformed, mismatched, or future cursor returns
`invalid_argument` (exit 2). A cursor grants no authority. Deliberately reusing
one repeats the same historical changes while the durable log remains unchanged.

The default limit is 100 changes. Each page scans at most 256 event rows and
contains less than 64 KiB, regardless of task or event body size. Excluded
events advance the cursor without exposing their contents. Continue while
`has_more` is true, even if `changes` is empty. It means the scan has not reached
the durable high-water mark observed by this request.

`--wait` defaults to zero seconds. A positive value waits only when caught up,
returning on the next change, the availability of another page, or the requested
deadline. The supervisor remains able to process mutations and provider
observations during the wait, and every poll checks current authorization.
An expired wait succeeds with an empty page and `timed_out: true`. An ordinary
nonwaiting empty page has `timed_out: false`. Transport failure returns
`unavailable` (exit 7), with no acknowledgement or cursor mutation. Reconnect
explicitly to the same run and resume with the last printed cursor. Progress
does not start a supervisor or read an offline database on an agent's behalf.

```console
coterie progress --limit 20 --json
# Continue with the returned next_cursor until has_more is false.
coterie progress --after '<next_cursor>' --limit 20 --wait 5 --json
```

The response schema is generated from the typed CLI envelope and progress data:
[`cli-progress-v1.schema.json`](../schemas/cli-progress-v1.schema.json).
Regenerate the schema and example with
`cargo test cli::progress_tests::regenerate_progress_contract -- --ignored`.

### `coterie project list`

```console
coterie project list [--json]
```

List the run's projects with their `id`, unique `alias`, canonical `root`, and
`access`. JSON places this array in `data.projects`. Attachment membership does
not expand a provider's filesystem permissions. Status, project listing, and
other operator commands discover the same supervisor from any attached root.
Starting a foreground session from a secondary project reports the owning run
and its primary root; launch or recover the foreground from that primary root.

### `coterie project attach`

```console
coterie project attach <path> [--alias <name>] [--operation-id <id>] [--json]
```

Attach a canonical Git worktree or non-Git directory to an existing run. Relative
paths resolve from the caller's directory. Symlinks resolve before authorization;
linked Git worktrees have distinct identities. The alias defaults to the canonical
root's directory name and accepts ASCII letters, digits, underscores, and hyphens.
An identity can have only one alias within a run. Attachment preserves repository
files, including dirty work and `AGENTS.md`.

Agents need `project:attach` and a root beneath a trusted global
`allowed_project_roots` entry. This global-only array defaults to empty, accepts
absolute existing directories, and resolves symlinks when configuration loads.
Its canonical values are snapshotted with the run and excluded from portable
locks. Changing it requires resolving the run's configuration conflict.
An operator can explicitly attach outside the allowlist; the event records that
authorization and the resolved identity.

JSON returns the operation ID and `data.project`, using the same project fields
as `list`. Successful retries return the recorded result. Alias, identity,
configuration, and lease conflicts return `conflict` (exit 5); invalid paths or
aliases return `invalid_argument` (exit 2); missing agent authority returns
`permission_denied` (exit 6). Lease acquisition never waits. A foreign index is
preserved with a diagnostic identifying the run to recover or stop.

The current attachment foundation accepts absent project configuration or a
configuration and lock that agree with the run's effective policy. Differing
restrictions fail visibly until per-project overlays are implemented. Attachment
records durable intent before acquiring a lease and publishing an index.
Interrupted intents recover with the same identities; unresolved attempts remain
visible in `doctor` and events. Recovery reacquires all attached leases before
resuming work. Shutdown retires secondary indexes before the primary index and
then releases the leases. A partial shutdown preserves the primary recovery
entrypoint and any newer run's index.

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

For a Git worktree assignment, validate the work, commit any intended changes
successfully, and then run `finish --status completed`. A failed commit hook
leaves the changes uncommitted. Staged, unstaged, and non-ignored untracked
changes reject completion with `conflict` (exit 5) and a diagnostic listing the
affected paths. Unreadable paths, an unfinished Git operation, or index flags
that hide changes also prevent submission. Path diagnostics escape filenames,
show at most 20 paths, truncate long path displays, and count omitted paths.
Hidden-index diagnostics name the flagged paths and explain clearing the flags
before inspection and retry. Rejection preserves the active task, claim, and
assignment without recording a result or finish operation. Resolve the reported
changes and retry; the same operation ID remains usable. A successful operation
retry replays its recorded outcome even if the worktree later changes.

Ignored untracked files do not block completion, and a clean worktree needs no
new commit. Review and non-code assignments may therefore report successful
validation without making a commit. Project and read-only assignments keep
their existing submission behavior. `finish --status failed` preserves dirty
work and remains available when an assignment cannot be completed.

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
marks the run stopped, and retires its socket and project indexes before releasing
the leases. It preserves unfinished tasks, claims, draining assignments,
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

The default restart policy allows three launch attempts within 60 seconds,
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
Trusted global `supervision` settings override these defaults at run creation.
The resulting timeouts and restart bounds remain fixed in the run snapshot.

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

The default runtime archetype is the sealed `builtin:standard@1` definition.
Trusted global configuration may select other declared archetypes and provider
bindings. Project configuration can only select trusted definitions and reduce
authority or limits; explicit operator overrides remain within trusted bounds.
Each run snapshots its resolved policy before launching providers. The initial
foreground role uses the primary project with `project` or `read-only` workspace
policy; task assignments own isolated worktrees. A `read-only` workspace requires
a read-only filesystem profile. The following
table describes the built-in default roles. Repository contents, `AGENTS.md`, task and message text,
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
