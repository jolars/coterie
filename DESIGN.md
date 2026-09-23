# Coterie

## Status

This document describes the intended architecture and initial scope of Coterie.
It is a design target rather than a compatibility promise.

[`TODO.md`](TODO.md) separates delivery into milestones. Its `v0.1.0` MVP is a
deliberately smaller single-project vertical slice; the complete initial product
target described here follows after that MVP. Unless a passage explicitly names
the MVP, references to the initial product target mean the complete scope in the
section of that name below.

## Purpose

Coterie is a project-native CLI for orchestrating coding agents. From a user's
perspective, it should feel like launching an ordinary agent harness in the
current project:

```console
cd my-project
coterie
```

The user interacts with one foreground lead agent. Coterie supplies that agent
with a declaratively configured group of workers, an embedded durable task
graph, isolated workspaces, and a CLI protocol for coordination. A run begins in
one primary project and may attach other projects when a request spans
repositories.

Coterie is inspired by Gas Town, Gas City, and Firstmate, but differs in five
important respects:

1. Configuration is declarative and centered on reusable, versioned archetypes.
2. A run is anchored in the current project and may attach explicit additional
   projects; projects are not permanently registered in a separate city or
   fleet.
3. Coterie behaves as an agent harness and can run directly in a terminal
   integration such as sidekick.nvim.
4. Orchestration behavior and durable work tracking belong in the compiled
   binary rather than shell scripts or required companion CLIs.
5. Agent harnesses remain out-of-process providers. Coterie does not absorb
   their model clients, authentication, tools, or sandboxes.

The central product principle is:

> Running Coterie in a project should be as natural as running Codex or Claude
> Code there directly.

## Design principles

- **Project-native operation**: launching in a project is sufficient. Additional
  projects are attached only when a run needs them; there is no Coterie
  initialization or permanent project registry.
- **Libraries inside, protocols outside**: Coterie uses Rust libraries for
  state, Git operations, configuration, IPC, and process management. External
  processes exist only at deliberate provider boundaries.
- **Durable work, disposable sessions**: tasks, assignments, messages, and
  handoffs survive provider exits and supervisor restarts.
- **Configuration-defined behavior**: role names and delegation strategies are
  data. The binary contains no special cases for `lead`, `worker`, `reviewer`,
  or other archetype-defined names.
- **Mechanics in Rust, judgment in agents**: Coterie enforces policy, ownership,
  state transitions, and transport. Agents decide how to decompose work, what to
  delegate, and whether an outcome satisfies the user's request.
- **Desired-state reconciliation**: crashes and partial operations are repaired
  by comparing durable intent with observed state, not by assuming each
  multi-step operation completed.
- **Explicit uncertainty**: unknown provider or process state remains unknown.
  Coterie does not infer semantic idleness from CPU use, elapsed time, or
  terminal text.

Before adding a new core abstraction, ask whether it can be composed from
existing concepts, whether it remains useful as models improve, and whether it
would move judgment from an agent into Rust.

## Concepts

- **Project**: a canonical Git worktree, or a canonical directory for a non-Git
  project, attached to a run under a unique human-readable alias.
- **Primary project**: the project from which Coterie was launched. It anchors
  configuration, run discovery, and the lead's initial working directory.
- **Attached project**: an additional project root granted to an active run.
  Attachment is run-scoped and does not register the project permanently.
- **Archetype**: a reusable declarative description of roles, providers,
  permission profiles, workspace policies, and resource limits.
- **Role**: a configured type of agent. Roles have no built-in semantics.
- **Agent**: one instantiated role within a run.
- **Run**: one active orchestration spanning a primary project and zero or more
  attached projects.
- **Session**: one live or resumable provider execution associated with an
  agent.
- **Task**: a durable unit of work with exactly one writable target project and,
  when needed, explicit read-only input projects.
- **Assignment**: the durable association between a task, an agent, and a
  workspace.
- **Task group**: a lightweight grouping of related tasks created for one user
  request or delegation wave.
- **Event**: an immutable, sequenced record of something that happened in a run.

There is no persistent equivalent of a Gas Town town or Gas City city. An
archetype is instantiated directly in the current project as a run, and any
additional project membership lasts only for that run.

## User experience

The primary interface is the foreground lead agent:

```console
coterie                              # use the project or global default
coterie --archetype builtin:review@1 # select another archetype
coterie status                       # inspect the current project run
coterie logs worker-2                # stream a worker's transcript
coterie project list                 # inspect projects attached to the run
coterie project attach ../library-py # attach another project to the run
coterie task ready                   # inspect ready work
coterie events --follow              # follow the typed event stream
coterie doctor                       # diagnose recoverable inconsistencies
coterie stop                         # stop the run safely
```

No initialization or project registration is required. Coterie discovers the
primary project root, starts or connects to its run supervisor, and launches the
lead agent there. The operator or an authorized lead may attach another
canonical project root for the lifetime of the run.

An optional `coterie.toml` selects a versioned archetype and applies a narrow
set of safe project overrides. An optional `coterie.lock` verifies the portable,
non-secret effective configuration. Both files may be committed. Coterie must
also work without either one.

For sidekick.nvim, Coterie should require only a normal custom CLI entry:

```lua
opts = {
  cli = {
    tools = {
      coterie = {
        cmd = { "coterie" },
      },
    },
  },
}
```

Coterie must behave correctly as a foreground terminal program: preserve the
working directory, forward signals and terminal resize events, avoid unsolicited
terminal output while the provider TUI is active, and return meaningful exit
codes.

In the initial product target, the foreground Coterie process owns the lead TUI,
while the supervisor owns background workers. Closing the foreground process
ends that live TUI but does not discard the run or stop active workers. A later
invocation resumes a recorded provider session when the adapter supports
reliable resume; otherwise it starts a fresh lead session and reconstructs
orchestration context through `coterie prime`. The initial product target does
not promise transparent process reattachment.

`SIGINT` is forwarded to the foreground provider. It does not implicitly stop
the run. Stopping all agents and cleaning up eligible resources requires
`coterie stop`.

Terminal hangup (`SIGHUP`), `SIGTERM`, and `SIGQUIT` also bound foreground
cleanup using the run's saved shutdown policy. The wrapper forwards the signal,
sends `SIGTERM` after the interrupt grace if needed, and kills an unresponsive
child at the same deadline used by foreground run shutdown. Repeated signals
and concurrent stop requests do not extend these deadlines. The wrapper reaps
its child and records the exit before returning; the run and workers survive.

If the supervisor rejects or cannot acknowledge foreground startup after the
provider process is created, the wrapper terminates and reaps that child under
the same bounded policy. It reports the observed exit before returning the
original startup error, even if startup was never recorded. Failed process
observation remains unknown. Every report retains its session ownership and
generation checks.

## Native dependencies and external boundaries

Coterie should be a single compiled Rust binary apart from the agent harnesses
the operator chooses to run.

Core facilities use in-process Rust libraries:

- SQLite through `rusqlite`, or an equivalent narrowly scoped SQLite crate, for
  tasks and orchestration state;
- `git2` for repository discovery, status inspection, references, and worktree
  management;
- `serde`, `toml`, and `schemars` for typed configuration and generated schemas;
- `tokio` and focused Unix process, signal, socket, and PTY crates for runtime
  management.

Coterie owns the task domain and schema; SQLite and `rusqlite` supply storage
and transactions rather than task semantics. Dependencies are locked, use
narrowly selected features, and are reviewed like other trusted code. The `git2`
boundary remains behind a trait so Coterie can move to a pure-Rust Git
implementation when one supports the required worktree mutations reliably.

Coterie-owned code does not invoke `bd`, `git`, `sh -c`, or another
general-purpose CLI to implement its state machine. A provider process may
invoke tools such as Git as part of its own agent work; that behavior belongs to
the provider's sandbox and permission policy, not to Coterie's internal
implementation.

Agent harnesses remain external because the process boundary isolates
authentication, configuration, model APIs, release cadence, and failure. A
provider adapter communicates through the strongest machine-oriented interface
the harness offers, in this order:

1. A versioned structured protocol with lifecycle and event semantics.
2. A documented JSON or JSONL non-interactive mode.
3. A foreground terminal interface for interactive use.

Coterie does not link against internal Codex or other provider crates. Such
crates are implementation details of their products and would couple Coterie's
release cycle to theirs more tightly than a process protocol.

External task trackers may be added later as explicit interoperability adapters.
They are not required for normal operation and are not the authoritative store
for a run using the built-in tracker.

## Declarative configuration

Global configuration lives at `$XDG_CONFIG_HOME/coterie/config.toml`, falling
back to `~/.config/coterie/config.toml`.

The M5 loader, resolver, and inspection commands retain provenance for every
effective value and verify portable configuration locks. Launches and recovery
use an immutable snapshot of that resolved configuration. Provider bindings
currently select commands implementing the Codex adapter contract.

Provider binding names are configuration keys, not adapter identifiers.
Foreground observations match the saved role's provider binding and interactive
mode, foreground process ownership, and the current run, agent, session, and
generation. A command implementing the Codex contract may use any valid
configured provider name.

Only absolute `XDG_CONFIG_HOME` and `HOME` values participate in discovery.
An absent or relative `XDG_CONFIG_HOME` falls back to an absolute `HOME`; if
neither is available, global configuration is absent. The project file is
`coterie.toml` in the discovered project root, without an additional search
through parent or nested directories. Missing optional files use defaults;
existing unreadable files, dangling file symlinks, and missing explicit
includes are errors. Loading configuration does not create files or probe
providers.

When no global configuration exists, Coterie uses these compiled operator
defaults:

- select `builtin:standard@1`;
- bind the `codex` provider name to the command `["codex"]`;
- allow at most eight concurrent agents, 16 agents in one run, and eight spawns
  per minute.

Trusted global configuration may replace the provider binding and these
run-wide ceilings or select another archetype. The provider command and
run-wide ceilings are operator policy; they are not part of an archetype's
versioned semantics.

Configuration files use `schema_version = 1`, with omission also meaning
version 1. The global format contains `archetype`, `includes`, `providers`,
`limits`, `supervision`, `allowed_project_roots`, `permission_profiles`, and
`archetypes`. Includes use the same partial format: fields may be supplied across files, but required
definition fields must exist after merging. A provider table supplies a
`command` argument array whose first element is a nonempty executable.

Global archetypes are keyed by complete references, such as
`[archetypes."global:pair@1"]`, with positive integer versions and no leading
zeros. Each declares a `lead` selector and `roles` tables. Each role requires
`provider`, `mode`, `workspace`, `permission_profile`, and `capabilities`;
`instructions` and `max_instances` are optional. Profile references resolve
against complete definitions in the global `permission_profiles` table.
Archetypes do not inherit from other archetypes. Role, provider, profile, and
archetype names contain only ASCII letters, digits, underscores, or hyphens.
Capabilities use the existing `spawn`, `send`, `task`, `logs`, `project`, and
`workspace` namespaces, followed by a colon and an action name or `*`.

`limits` exposes `max_concurrent_agents`, `max_agents_per_run`, and
`max_spawns_per_minute`, each a positive 16-bit integer. `supervision` exposes
the existing restart window, launch attempts, restart backoff, startup and job
timeouts, interrupt grace, and shutdown timeout fields with their explicit
`_seconds` or `_ms` units. These bounds must be positive, fit millisecond
arithmetic, and leave an interrupt grace shorter than the shutdown timeout.
The idle shutdown bound, `idle_timeout_seconds`, also accepts zero to disable
automatic shutdown; its default for new runs is 60 seconds.
Supervision settings are trusted global policy; project restrictions and
operator overrides do not change them in this slice.

The [global example](examples/config/global.toml) and
[project example](examples/config/project.toml) are tested loader inputs.
The [global schema](schemas/config-global-v1.schema.json) and
[project schema](schemas/config-project-v1.schema.json) are generated from the
typed Rust inputs. They describe file structure; resolution additionally
checks references, required merged fields, and policy bounds.

Regenerate these schemas explicitly with
`cargo test config::resolution_tests::regenerate_configuration_schemas -- --ignored`.
Ordinary test runs verify the schemas without rewriting them.

The semantic definition of `builtin:standard@1` is compiled into Coterie. The
following TOML-like representation is normative data, not a global
configuration file:

```toml
reference = "builtin:standard@1"
lead = "lead"

[permission_profiles.interactive]
filesystem = "project-write"
network = "provider-default"
approvals = "interactive"

[permission_profiles.worker]
filesystem = "workspace-write"
network = "deny"
approvals = "never"

[permission_profiles.review]
filesystem = "read-only"
network = "deny"
approvals = "never"

[roles.lead]
provider = "codex"
mode = "interactive"
workspace = "project"
permission_profile = "interactive"
instructions = """
Coordinate work through Coterie. Delegate independent implementation and
review tasks when useful, and report consolidated outcomes to the user.
"""
capabilities = [
  "spawn:worker",
  "spawn:reviewer",
  "send:*",
  "task:*",
  "logs:*",
  "project:attach",
  "workspace:integrate",
]

[roles.worker]
provider = "codex"
mode = "job"
max_instances = 3
workspace = "worktree"
permission_profile = "worker"
capabilities = [
  "send:lead",
  "send:peer",
  "task:read",
  "task:claim",
  "task:comment",
]

[roles.reviewer]
provider = "codex"
mode = "job"
max_instances = 1
workspace = "read-only"
permission_profile = "review"
capabilities = ["send:lead", "task:read", "task:comment"]
```

The archetype version pins its designated lead, role names, provider identities
and modes, instructions, capabilities, workspace policies, permission-profile
values, and per-role capacities. The `lead` selector creates exactly one
initial foreground agent in the primary project directory; that role uses
`project` or `read-only` workspace policy because it has no task assignment.
`worktree` roles require an assigned background task. `max_instances` bounds
explicitly spawned role instances and does not request an idle pool. The supervisor rejects spawns that
exceed either the role capacity or the effective run-wide ceilings. Automatic
demand-based pool scaling is not part of the initial product target.

The profile names above are local references within the sealed built-in
definition. Trusted global configuration cannot shadow the built-in archetype
or mutate its profiles, roles, or other versioned fields. It may instead select
a different, globally defined archetype. Project restrictions may replace a
role's effective profile only with a trusted profile that is no more permissive;
that restriction does not alter `builtin:standard@1` itself.

An optional project configuration contains only selection and monotone-safe
overrides. A project may disable roles, reduce capacities, choose a globally
defined permission profile that is no more permissive, and tighten resource
limits. It cannot increase authority or resource ceilings.

Project files allow only `schema_version`, `archetype`, `limits`, and `roles`.
Role restrictions allow `enabled`, `max_instances`, and `permission_profile`.
Disabling a role preserves its declaration but prevents spawning it. The
designated foreground role cannot be disabled. A capacity of zero prevents
explicit spawns; like any `max_instances` setting, it does not change creation
of the one initial foreground agent. An omitted archetype role capacity has no
per-role ceiling, but run-wide limits still apply.

Permission restrictions compare all components. Read-only filesystem access
may replace either writable scope; project-write and workspace-write are
incomparable. Both writable scopes may replace unrestricted access. Network
denial may replace provider-default access, and never requesting approvals may
replace interactive approvals. Other increases or
incomparable replacements are errors. Effective role settings remain separate
from their unmodified archetype definition.

Permission profiles independently select `approval_reviewer = "user"` or
`"auto-review"`; omission means `"user"`. Interactive approvals use the selected
reviewer. Automatic review with `approvals = "never"` is invalid. Human and
automatic review are incomparable policies: a project may disable approvals,
but cannot switch reviewers. Codex maps automatic review to `on-request` and
`approvals_reviewer = "auto_review"`, preserving the selected sandbox.

Trusted profiles may select `filesystem = "unrestricted"`. Codex maps this to
`danger-full-access`, removing both filesystem and network sandbox boundaries,
so this profile requires `network = "provider-default"`. Approval policy and
reviewer remain independent. A read-only role cannot use unrestricted access.
If any enabled role has unrestricted access, its selected archetype must match
an explicit global default or the operator's `--archetype` selection. A project
cannot activate unrestricted operation merely by selecting a globally defined
archetype. No built-in archetype grants unrestricted access.

Migration 20 records the historical `user` reviewer in saved profiles and their
provenance. Portable locks omit the default reviewer, retaining historical
fingerprints; automatic review contributes to the fingerprint. Missing reviewer
fields in current-schema snapshots are errors. Internal RPC protocol 15 carries
the reviewer so older supervisors cannot silently omit the selected policy.

Policy intersection retains shared permissions, requires both role policies to
enable a role, and takes the smaller capacity or run limit. An omitted role
capacity is unbounded within the run-wide ceilings. Resolution rejects requests
whose intersection would remove requested authority or capacity, rather than
silently clamping them. Lattice tests cover idempotence, commutativity, and
associativity; combined restriction tests check accepted policies against their
selected trusted definitions and reject authority injection through project
tables.

```toml
archetype = "builtin:standard@1"

[roles.worker]
max_instances = 2
```

Configuration is resolved in this order:

1. Versioned compiled defaults and built-in archetypes.
2. Trusted global configuration and global local includes.
3. The selected global or built-in archetype.
4. Optional project restrictions.
5. Explicit operator command-line overrides, bounded by global policy.

Within the fields that a layer is authorized to set, later values replace
earlier scalar and array values, and tables merge recursively. Selecting a
built-in archetype resolves its sealed definition rather than merging global
archetype or permission-profile tables into it. Every effective value retains
provenance identifying its source layer and file.

Provenance distinguishes compiled defaults, sealed built-in definitions,
trusted global files (including individual includes), project restrictions,
and operator overrides. Each scalar, optional default, and complete array
records its input field and source. Arrays have one origin because layers
replace them atomically. Explicit assignments change the origin even when the
value equals an earlier value; empty tables do not replace child origins.
Omitted optional fields in global roles and implicit role enablement come
from compiled defaults.

The selected archetype reference retains both its definition origin and its
selector origin. A global reference's definition origin is the last file
contributing its table. Effective permission-profile components likewise retain
their individual definition origins and the role setting that selected the
profile. Selecting a built-in never attributes its sealed fields to global
definitions with the same names. File sources retain the path used to open
the file; compiled, built-in, and operator sources have no file. Pure resolution
without file loading reports layers and fields without inventing file paths.
Source metadata is separate from effective policy values and does not copy
command arguments, instructions, or other configuration values.

Golden tests cover complete provenance for compiled and layered configurations.
Regenerate these snapshots explicitly with
`cargo test config::provenance_tests::regenerate_provenance_snapshots -- --ignored`.

Archetype selection uses the last explicit selector: operator, project,
global, then compiled default. Project role restrictions apply to that final
selected archetype. Unknown selectors in lower layers still fail validation.
Explicit operator overrides may restore capacities, enabled roles, or profiles
restricted by the project, but cannot exceed the selected trusted archetype
or global run limits. They expose the same role restrictions and run limits
as the project layer, plus archetype selection. Invalid project requests are
reported before operator overrides are applied, rather than hidden by them.

Coterie provides the following inspection commands:

```console
coterie config check
coterie config show --effective --provenance
coterie config schema
coterie config schema --target global
coterie config schema --target lock
coterie config schema --target effective
coterie config lock
```

These commands run locally without creating a run, contacting a supervisor,
or probing a provider. `check` validates the resolved configuration and verifies
an existing lock. `show` displays effective policy by default; `--effective`
makes that selection explicit, and `--provenance` includes origins and selectors.
It also verifies an existing lock before emitting successful output. Known
provider credentials and Coterie tokens are redacted from inspection output.
`schema` defaults to the project input format and emits JSON Schema without
reading configuration. The `global`, `lock`, and `effective` targets expose the
other typed contracts. `--json` wraps command results in the version 1 CLI
success envelope; without it, schemas and effective configuration are pretty
JSON, and validation and lock creation return concise text.

Unknown fields and unsupported schema versions are errors. Includes are
global-only, non-recursive, cycle-checked, and resolved relative to the
including file.

Included files load in listed order, followed by the main global file, which
takes precedence. Every file is checked for unknown fields and unsupported
schemas before merging. Included files cannot contain an `includes` field,
even an empty one. Canonical file identities detect repeated files and cycles,
including aliases through symlinks. Repeated inclusion is an error. File
symlinks are otherwise allowed, and relative includes resolve against the
directory of the path used to open the including file.

Built-in archetypes use the reserved `builtin:` namespace and cannot be shadowed
by global configuration. Global archetypes use `global:`. If no configuration
exists, Coterie uses `builtin:standard@1`.

`coterie config lock` explicitly writes `coterie.lock`. The lock records the
selected archetype reference, configuration schema, compatible Coterie version
range, provider requirements, and a SHA-256 digest of the portable effective
configuration. It contains no secrets, executable paths, or host-specific
values. When a lock is present, a mismatch fails with an actionable diagnostic
rather than silently using a different archetype.

The version 1 lock is JSON with required `schema_version`, `archetype`,
`coterie_version`, `providers`, and `fingerprint` fields. Creation records a
caret-compatible range starting at the running Coterie version; verification
checks that range semantically. Provider requirements map each provider name
to its enabled roles' required modes and effective permission profiles. They
describe configuration demands, not observed provider versions or capabilities.
Provider capability probes remain the launch boundary's responsibility.

The fingerprint is SHA-256 over compact UTF-8 JSON with recursively sorted
object keys and preserved array order. Its projection contains configuration
schema version 1, the complete selected archetype definition, effective roles,
run limits, supervision policy, and provider requirements. Archetype instructions
and capabilities therefore participate in the digest without being copied into
the lock. Provider command arrays, unused provider bindings, provenance, source
and include paths, allowed project roots, project identity, environment values,
and installed executable versions are excluded. Moving files, changing host command bindings, or making
an equal explicit assignment leaves the fingerprint unchanged.

Lock verification rejects unknown fields, unsupported schemas, malformed
fingerprints or version requirements, and mismatches in the archetype, Coterie
compatibility, provider requirements, or fingerprint. Diagnostics identify
mismatched fields without echoing arbitrary lock content and suggest restoring
the intended configuration or explicitly regenerating the lock after review.
Inspection never repairs a lock. `config lock` may replace an existing invalid
or outdated regular file, using a private temporary file, file sync, atomic
rename, and directory sync. Locks are bounded to 1 MiB. Reads and writes refuse
symlinks, hard links, and nonregular files. Interrupted writes leave an absent,
old, or complete new lock and may retain a temporary file for inspection;
retries never adopt or remove another attempt's temporary file.

The [lock schema](schemas/config-lock-v1.schema.json),
[effective report schema](schemas/config-effective-v1.schema.json), and
[example lock](examples/config/coterie.lock) are generated and checked against
their typed definitions and example configuration.
Regenerate lock golden files and the example explicitly with
`cargo test config::lock::tests::regenerate_lock_goldens -- --ignored`.

Project configuration is untrusted. It cannot define provider executables,
instructions, hooks, host paths, environment-variable passthrough, capabilities,
or permission profiles. Effective project settings are intersected with trusted
global policy. Commands are represented as argument arrays and executed
directly; Coterie never evaluates configuration with `sh -c`.

The M6 attachment foundation provides `project attach` and `project list`,
canonical roots and unique aliases, exclusive leases, and discovery from every
attached project. Until the separate per-project overlay item passes, attachment
accepts absent project configuration or configuration and locks that match the
run's effective policy. Differing restrictions fail attachment explicitly.
Foreground invocation from a secondary project reports the owning run and primary
root; operator commands connect to the same supervisor from either root.

The primary project selects the run archetype. When another project is attached,
Coterie loads that project's restrictions and lock, applies them to work
targeting that project, and snapshots the result. An attached project's
archetype selector cannot replace the active run archetype; an incompatible
selector, lock, or restriction fails attachment with an actionable diagnostic.

The run-level effective configuration and its fingerprint are snapshotted when a
run starts; each additional project's effective restriction overlay is
snapshotted when it is attached. The initial product target does not hot-apply
configuration changes to an active run. Starting Coterie or attaching a project
with a conflicting archetype or configuration reports the active snapshot and
requires an explicit resolution.

Startup resolves configuration and verifies any lock before creating run state.
The supervisor records a versioned snapshot, portable fingerprint, and provenance
in the same transaction as the run and primary project. The private snapshot
includes host provider command arrays, which portable locks exclude. Recovery
and every runtime policy decision use this snapshot. Database migration 11 pins
the historical compiled policy for older runs without a snapshot; it never
adopts current files as those runs' original policy. Missing or invalid snapshots
in the current schema fail closed.

Before decoding an indexed run's snapshot, startup verifies that its migration
history is an unchanged, contiguous prefix of the supported schema. When
migrations are pending, the lease-owning supervisor applies them and validates
the upgraded snapshot before launching or reconciling external resources.
Read-only preflight must not reject a historical snapshot merely because a
pending migration adds a required field.

Compatibility compares all effective values, including command bindings, while
ignoring provenance changes. Moving a source file or explicitly reassigning an
equal value is compatible. Changing a binding is incompatible even if its
portable lock still verifies. New foreground invocations and supervisor recovery
reject conflicting policy before launching or reconciling external resources.
The diagnostic identifies the run, saved fingerprint, and changed fields. Restore
the configuration and operator overrides used for that run, or stop a reachable
run before launching with new policy. Status, logs, events, stop, and existing
foreground control connections remain usable while files conflict. Doctor checks
current configuration and lock validity against the saved snapshot without
migrating or rewriting state.

Operator startup, configuration inspection, and doctor accept `--archetype`,
`--max-concurrent-agents`, `--max-agents-per-run`, `--max-spawns-per-minute`, and
repeatable `--role ROLE.FIELD=VALUE` options. Role fields are `enabled`,
`max_instances`, and `permission_profile`. Repeated fields use the last value.
These requests remain bounded by trusted policy and appear as operator provenance.
Reconnecting with a different effective override reports a snapshot conflict.
Other commands reject configuration overrides because they use the active run's
snapshot. Known credential literals and Coterie tokens in configuration cannot
be persisted as launch policy; use the provider authentication environment.

The supervisor enforces effective role enablement, profiles, capacities,
run-wide agent ceilings, and a rolling 60-second explicit-spawn ceiling. Retrying
an operation does not consume another spawn. Role instructions join Coterie's
bootstrap at the provider boundary without changing repository instruction files.
Effective supervision policy controls launch admission, restart quarantine,
session deadlines, and foreground and worker shutdown escalation.

## Runtime architecture

Each run has a supervisor process that is started automatically on demand. The
foreground Coterie process connects to the supervisor before it launches the
provider TUI.

```text
sidekick.nvim or terminal
        |
        v
foreground Coterie process ---- foreground lead TUI
        |
        v
per-run supervisor
        |-- desired-state reconciler
        |-- agent and assignment registry
        |-- task, message, and event store
        |-- background provider sessions
        |-- attached-project leases
        `-- workspace ownership
```

The supervisor communicates through a Unix-domain socket under
`$XDG_RUNTIME_DIR/coterie/`. Durable state lives under
`$XDG_STATE_HOME/coterie/runs/<run-id>/`. A small local index beneath
`$XDG_STATE_HOME/coterie/projects/` records which active run, if any, holds a
project identity. This is disposable coordination metadata, not project
registration or configuration.

On Linux, the supervisor sets its socket descriptor to mode `0600` before
binding the runtime path. A recovery client must never observe a socket that
still needs its permissions tightened, and a crash immediately after binding
must leave a private socket that normal stale-socket recovery can inspect.

For a Git project, the identity includes both the canonical Git common directory
and the current worktree identity. Two linked worktrees from the same repository
therefore do not accidentally share one active run. A canonical directory
identity is used for non-Git projects. Symlinks are resolved, aliases must be
unique within the run, and both resolved identities and original paths are
stored for diagnostics.

An active run holds a nonblocking exclusive lease for every attached project
identity, including its primary project. Runtime locks and a supervisor socket
handshake enforce ownership; a PID file or durable index entry alone is never
treated as proof of liveness. Because attachment never waits for another project
lease, two runs attempting to cross-attach each other's projects fail visibly
rather than deadlock. Stale sockets, leases, index entries, and interrupted
attachment are repaired conservatively.

Launching Coterie from any project leased by a live run connects to that run or
reports its identity before performing a conflicting action. The initial product
target uses an exclusive lease; concurrent read-only attachment may be added
later if it can preserve comprehensible ownership.

The supervisor is the single writer for the run database. CLI processes issue
typed RPC requests rather than opening the database directly. This makes
ordering explicit and keeps project attachment, dependency, claim, and
assignment transactions local to one process.

The supervisor may outlive the foreground lead while workers are active. It
exits when the run is stopped or after a configurable idle period with no live
sessions or pending operations.

New runs default to `supervision.idle_timeout_seconds = 60`, set only in
trusted global configuration. Zero disables automatic shutdown. Runs created
before this setting existed retain disabled idle shutdown in their saved
policy; migration does not silently change an existing run's behavior.

The idle timer requires every recorded session to have an observed exit, no
pending or uncertain operation, no pending process control, and no unresolved
workspace creation. Any durable event restarts the timer; read-only polling
does not. Supervisor recovery starts a fresh full interval. The supervisor
checks eligibility and records shutdown intent in one transaction, then uses
the ordinary shutdown phases to mark the run stopped, retire every attached
project index and the socket, and release leases. Tasks, transcripts, and
workspaces remain available on disk. Launching after idle shutdown starts a
new run unless the operator explicitly recovers a retained run first. Unknown
process state never qualifies as idle.

### Explicit stopped-run recovery

`coterie run list` discovers retained runs attached to the current project,
including stopped runs that no longer have an active index. Discovery reads
existing state without starting supervisors or modifying databases. It reports
the run ID, primary root, lifecycle, task counts, and last stop time.

`coterie run recover <run-id> --reason TEXT` explicitly reactivates that same
run. It is an operator command, separate from reconnecting to an active run.
The operator then launches `coterie` for a fresh authenticated foreground
session. Recovery preserves task IDs, dependencies, accepted results, messages,
reports, transcript references, and assignment history in the original store.
It does not import work into a replacement run or resume old credentials.

Before reactivation, the supervisor validates the saved configuration against
current policy and locks, rechecks attached project identities and restrictions,
and acquires every exclusive project lease. An existing replacement run must
be explicitly stopped first, even when its supervisor is unreachable. Recovery
never replaces that run's indexes. All recorded sessions must have observed
exits with revoked credentials, and unresolved resource operations prevent
reactivation. An observed exit followed by fresh provider proof can finalize a
pending control record left by synchronous shutdown before the next control
poll. Missing exit evidence and mismatched control generations still refuse
recovery. Unfinished assignments retain their ownership and draining state;
Git ownership is checked without refreshing the source index or changing files.

The supervisor atomically records the prior shutdown, reason, operator, and
recovery operation in an append-only `run.recovered` event, clears the completed
shutdown marker, and reactivates the run. This durable intent precedes index
publication. Interrupted publication can be retried for the exact run and
operation ID. Exact retries return the original recovery result, including
after subsequent task acceptance or another shutdown; they never reactivate a
later stopped run again. Changed arguments conflict. No old session is relaunched.

The stop operation that preceded recovery also remains replayable. Its recorded
result describes the earlier shutdown and does not stop the continued run or
wait for its new indexes to retire. A new stop requires a new operation ID.

Unfinished assignments require the ordinary `task recover` checks before a
fresh worktree continuation can claim their task. Recovery does not grant
writable access to preserved worktrees or transfer changes automatically. The
operator or continuation selects and copies useful changes into the fresh
workspace while leaving source files, index, and references intact, then
validates, commits, submits, integrates, and explicitly closes the same task.
Dependencies remain blocked until accepted closure. The optional automated
transfer helper remains separate future work.

## Embedded task and state store

SQLite is the source of truth for both durable work and orchestration state. The
schema includes at least:

- runs and effective configuration snapshots;
- attached projects, aliases, identities, restrictions, and leases;
- agents, sessions, and lifecycle generations;
- tasks, dependencies, comments, and task groups;
- claims and assignments;
- messages, delivery cursors, and acknowledgements;
- workspace ownership and integration metadata;
- operations and reconciliation attempts;
- the append-only typed event stream.

Provider transcripts are stored separately as append-only files with database
references. This avoids large transcript blobs in ordinary state queries.

Task lifecycle initially supports `open`, `in_progress`, `submitted`, `closed`,
and `canceled`. A task has exactly one attached project as its writable target
and may name other attached projects as read-only inputs. Its project set cannot
change while claimed or assigned. Dependencies may cross project boundaries.
Blocking is derived from dependency state rather than stored as a second source
of truth. A task is ready when it is open, has no unresolved blocking
dependency, and has no active claim.

Claims are compare-and-set transitions performed in an immediate transaction.
Claiming a task and creating its assignment are one database operation. Each
initial job agent has at most one active assignment.

Durable identifiers use a lowercase Coterie type prefix, a hyphen, and a
canonical uppercase ULID rather than database row numbers:

| Identity   | Prefix |
| ---------- | ------ |
| Run        | `cr-`  |
| Project    | `cp-`  |
| Agent      | `cg-`  |
| Session    | `cs-`  |
| Task       | `ct-`  |
| Assignment | `ca-`  |
| Message    | `cm-`  |
| Operation  | `co-`  |
| Event      | `ce-`  |

Parsers require the exact lowercase prefix and delimiter. The ULID suffix is
case-insensitive on input and normalized to uppercase on output; malformed or
noncanonical values are rejected. These identifiers remain distinct types in
Rust and stable strings in CLI, protocol, transcript, and persistence
boundaries.

A completed assignment moves its task to `submitted`; it does not by itself
satisfy dependent tasks. Closure records that the task's acceptance condition
has been verified. For a worktree assignment, this normally requires an
integration record identifying the target project, base commit, result commit,
and resulting target commit. The lead may close work performed directly in a
target project, or non-code work, after explicit validation. An operator
override is recorded as such rather than fabricated as integration evidence.

An interrupted, unsubmitted Git worktree assignment can be retired with
`coterie task recover --assignment ID --reason TEXT`, by the operator or an
agent with `task:recover`. The named assignment must still own the task's active
claim. Its current session generation must have an observed process exit, a
recorded end time, revoked credentials, and no pending launch, process control,
workspace creation, or integration. A terminal label alone is insufficient.
Recovery also requires the normalized provider exit event and a fresh adapter
check of the recorded process identity. That check must prove an exact exited
session with exit details, or process absence following the recorded exit.
Absence alone never supplies the missing exit evidence. Provider uncertainty
refuses recovery without changing the original observation.
Unknown or lost process ownership requires inspection with `coterie doctor`
and never authorizes recovery. A stopped or draining run cannot recover tasks.

Recovery is one database transaction: release the old claim and assignment,
reopen the same task, and record the reason, actor, session, and preserved
workspace identity in an append-only `task.recovered` event and the operation
result. Original summaries, session history, transcripts, worktree files,
index, commits, and references remain intact. Retrying the same operation
replays its original result even after continuation or accepted closure.
Changed requests conflict, and stale callers remain fenced before replay.

The next ordinary `spawn` for that task must use a worktree role. It records an
explicit `assignment.continued` link to the retired assignment in its claim
transaction and creates a fresh isolated worktree under the existing durable
spawn protocol. `prime` reports the recovery source and continuation identities,
preserved path, base commit, and reason. The continuation inspects the preserved
source and ports the useful changes into its own workspace, then validates,
commits, and submits normally. Recovery does not automatically copy files or
grant writable access to the preserved workspace. This initial recovery path
does not transfer writable ownership or resume an old provider generation.
Repeated interruptions retain each recovery and continuation link. Dependencies
remain blocked until the same task's continued result is integrated, validated,
and explicitly closed. Submitted work uses `task resubmit` instead.

Recovery records a self-contained handoff. Before the retirement transaction,
the workspace backend verifies source ownership and reads HEAD and the Git
status without refreshing or writing the index. The snapshot lists dirty,
staged, unstaged, untracked, conflicted, and unreadable paths, plus index paths
whose assume-unchanged or skip-worktree flags prevent complete inspection.
Paths retain their native bytes. Dirty paths are the union of observed changes;
ignored files are excluded. Hidden or unreadable paths make completeness
explicitly unknown. The snapshot is an observation at recovery, not a claim
that the preserved worktree cannot subsequently change.

`task recover --report JSON` optionally supplies `validation_evidence` and
`unfinished_steps`, each an array of `{ "text": "...", "source": "..." }`.
Sources identify the original message, transcript session and byte cursor,
report, or artifact. The recovering operator or authorized agent selects this
context and is recorded as its reporter. Coterie stores those statements as
reported evidence, never as mechanically verified checks or executable actions.
An omitted or empty report means no evidence was supplied; it does not mean
validation passed or no work remains. This operation shares only the supplied
report under ordinary task visibility; it grants no access to another agent's
inbox or transcript. Agents with recovery authority obtain needed evidence
through their existing read authority or ask its owner to report it.

Migration 17 adds immutable recovery handoff documents. Retirement, the handoff,
and the operation result commit atomically; retries replay the original snapshot
and report. Historical recoveries have no snapshot and are labeled unavailable,
without inventing earlier Git observations. `prime` and recovery responses show
bounded counts and report previews with the recovery operation ID and source
assignment reference. `assignment show` for the source or its continuation
returns the complete snapshot and report through revision-checked document pages,
including after integration and closure. These are inspection data and grant no
writable ownership of the preserved source. The continuation must inspect and
port selected changes into its fresh workspace, validate there, submit, and
obtain explicit integration and validated closure.

An incorrect unintegrated Git submission can be superseded explicitly with
`coterie task resubmit --assignment ID --expected-result OLD --result NEW
--summary TEXT --reason TEXT`. The operator or an agent with `task:resubmit`
may invoke it. Workers without that capability ask an authorized coordinator.
The command compares the full recorded commit ID with `OLD` and requires `NEW`
to be the clean, owned worktree tip and a descendant of `OLD`. Rewritten or
unrelated histories require preserving the original work and creating a new
task. No reference, worktree, claim, session, or assignment ownership changes.
The task stays `submitted`, and dependencies still wait for validated closure.

Resubmission atomically records the previous task result, assignment summary,
commit identities, replacement result, reason, and actor in an append-only
`task.resubmitted` event and an idempotent operation result, then replaces only
the current task result, assignment summary, and workspace result commit.
Original submission operations and events remain unchanged, and descendant
ancestry keeps their commits reachable. Exact retries replay the recorded
response, including after a crash or subsequent integration. New resubmissions
refuse closed tasks, integrated workspaces, stale ownership, and any existing
integration intent, including one with an unknown outcome. The supervisor
serializes resubmission with integration admission; an intent already recorded
must be reconciled with `doctor` and the original integration operation rather
than superseded. Git validation is read-only and precedes the database-only
mutation, whose preconditions are checked again transactionally.

For work integrated outside Coterie, the operator may explicitly run
`coterie task close <task> --override --assignment <assignment>
--result-commit <full-oid> --target-commit <full-oid> --reason <text>
--summary <validation-evidence>`. All override fields are required together.
Only the operator channel may override acceptance; `task:close` never grants
this authority to an agent. The task must be submitted and the assignment must
be its latest completed, unintegrated Git worktree assignment. The backend
verifies repository and workspace ownership, clean worktrees, the recorded
result at the assignment tip, and the supplied target commit at an unambiguous
target branch HEAD. Both commit IDs are full object IDs. A moved or dirty target
requires fresh validation and an updated request, not weaker guards.

The operator judges whether the external change satisfies acceptance, including
when a cherry-pick has a different commit ID. Coterie does not infer equivalence
from patches or fabricate integration evidence. Closure atomically records an
`operator_override` result and lifecycle-event field identifying the assignment,
project, base, result, target branch and commit, reason, and validation evidence.
It preserves the original submission and all workspace metadata, files, and
references. In particular, it does not set the workspace's integration commit
or authorize cleanup. Dependencies become ready only after this closure commits.
Successful retries replay the recorded acceptance without rechecking later Git
changes; changing the request under the same operation ID is a conflict.

Dependencies wait for `closed`, not merely `submitted`. This gives cross-project
sequences precise semantics: a bindings task remains blocked until the upstream
library change has been integrated and validated in the library project. The
closed task exposes a compact result record---including its project, relevant
commits, summary, and test outcome---to downstream agents through
`coterie prime`.

The store uses foreign keys, explicit schema migrations, bounded busy timeouts,
and WAL mode where the platform supports it. Every mutating RPC accepts an
operation ID so retries are idempotent. Requests containing redactable text
store a fingerprint of the original typed request separately from the redacted
text. Credential changes do not change the identity of a recorded retry.

Database transactions cannot include process or filesystem side effects.
Operations that attach a project, create or integrate a worktree, or launch a
provider therefore follow a durable intent pattern:

1. Record the desired operation and ownership in SQLite.
2. Commit the transaction.
3. Perform the external side effect.
4. Record the observed result.
5. Let reconciliation repair an interrupted sequence.

The initial product target provides local durability across crashes and
sessions, not cross-machine task synchronization. Export, import, and external
tracker adapters may be designed later without changing the internal task
interface.

## Agent bootstrap and identity

Coterie injects a small provider-specific bootstrap instruction before an agent
begins work. For providers such as Codex, this uses a supported developer- or
system-instruction mechanism rather than modifying `AGENTS.md` or sending an
ordinary first chat message.

The bootstrap establishes only orchestration behavior:

```text
You are the lead agent for Coterie run 7b2f.
Use the Coterie MCP tools for delegation and communication.
Call `prime` now for current identity, peers, tasks, and tool guidance.
Follow the repository's AGENTS.md instructions for work in the project.
```

Repository instructions remain the source of project conventions. Coterie does
not generate, modify, shadow, or replace `AGENTS.md`. Each agent receives the
instructions that apply to its assigned project's working directory. The
bootstrap must avoid conflicting work instructions, while recognizing that the
provider's instruction hierarchy may place injected developer instructions above
repository files.

The bootstrap tells agents coordinating delegated work to keep coordinating
while work remains, unless the user pauses it. This is conditional guidance for
every configured role, not a runtime classification of role names or an automatic
assignment of coordination responsibility. The run's snapshotted capabilities
select command guidance: `task:read` permits continued
progress inspection through the MCP `poll` helper and fallback polling with
`wait_seconds=5`, `workspace:integrate` permits
explicit integration, and `task:close` permits closure after validation. Agents
without a needed capability report the blocker to the user or an authorized
coordinator instead of attempting the restricted command.

When `prime.notifications` is `automatic`, a coordinator may end its turn after
handling actionable results and messages while waiting for delegated work.
Otherwise it uses the polling fallback within its granted authority.
Coordinators use `poll` to drain progress pages and inspect pending inbox messages
with one replayable checkpoint containing separate cursors. Each call drains at
most 16 progress pages or 100 changes; callers continue while `has_more` is true.
Unhandled messages remain in later polls. The `inbox_handled` helper resolves
explicitly handled message IDs and acknowledges only a prefix without unhandled
gaps. Roles without `task:read` can poll only their inbox. The underlying
`progress`, `inbox`, and explicit cursor acknowledgement tools remain available.
Progress cursors neither read nor acknowledge messages. Submissions must be carried
through review, integration where needed, validation, and accepted task closure
within granted authority. Submission or provider exit alone is not acceptance.
Agents report blockers requiring user action rather than silently ending a turn
with work awaiting coordination.

The Codex adapter capability-probes `queue --help` and delivers automatic
foreground notifications through `codex queue`. Durable inbox messages and,
with `task:read`, external worker lifecycle changes trigger delivery. A role's
own mutations do not trigger a notification loop. No role name acquires special
runtime semantics.

The foreground wrapper retains the configured command, working directory,
provider environment, and unreaped child. The host MCP bridge binds Codex's
`_meta.threadId` to the current authenticated session generation. The supervisor
also verifies the Unix socket peer's Linux process identity: the bridge must
be the Coterie executable launched directly by the recorded foreground process.
Tool arguments, session names, working-directory searches, and agent RPC
credentials alone cannot choose a destination. Bindings are immutable.

The supervisor coalesces pending changes, commits a delivery attempt, and
records the wrapper's queue observation. At most one notice per foreground
session may await receipt. Provider acceptance alone does not release this
limit: a notice can wait behind a long-running turn even while the agent reads
updates through tools. Queue input contains only a fixed notice with the
run, session, generation, and delivery ID; worker content stays in authenticated tool results.
The recipient compares the notice with the authenticated scope in `prime.session`.
It calls `notification_received` with that delivery ID and a mutation operation
ID, then polls for current updates. Receipt atomically coalesces events and
messages through the current high-water marks, including updates that arrived
while the notice waited. Events after receipt can trigger the next notice.
Repeated receipts never advance those marks again. Ordinary `prime`, `poll`,
reconciliation, and turn completion do not release the outstanding notice or
queue another without a new eligible event. Receipt requires the authenticated
current foreground session and cannot choose a recipient or destination.
Delivery and receipt never acknowledge inbox messages or accept tasks.
The notice preserves earlier user restrictions, pauses, and stop instructions.
It cannot authorize new work or resume paused work.

The wrapper reauthenticates after supervisor recovery and may restore its
owned child's process observation. The MCP bridge reconnects using the same
agent credentials and operation IDs. Delivery stops for exited, stale,
unobserved, or stopping sessions. Queue attempts have a bounded timeout. Because
the queue CLI has no caller-supplied idempotency key, an uncertain attempt is
not retransmitted: automatic delivery becomes `uncertain`, and the operator is
directed to the inbox and polling fallback. A fresh foreground generation can
establish a new binding. See the [delivery contract and tests](docs/codex-queue.md).
Historical notices without delivery IDs have unknown receipt state after
migration. Their sessions use the uncertain-delivery fallback until a fresh
foreground generation establishes a binding. Coterie cannot retract notices
already accepted by the provider.

Dynamic context is obtained through `coterie prime` so agents can recover after
compaction, provider resume, or a fresh session. Every agent process receives an
identity-scoped environment:

```text
COTERIE_PROJECT_ROOT
COTERIE_PROJECT_ID
COTERIE_PRIMARY_PROJECT_ROOT
COTERIE_RUN_ID
COTERIE_AGENT_ID
COTERIE_SESSION_ID
COTERIE_ROLE
COTERIE_TASK_ID
COTERIE_SOCKET
COTERIE_TOKEN
COTERIE_BIN
```

`COTERIE_BIN` is the absolute executable path of the launching Coterie
process, supplied by Coterie rather than inherited from the environment. The
Codex adapter uses that executable to launch a required, session-specific MCP
server through the provider's supported stdio configuration. Bootstrap names
the server and selected permission profile, directs agents to discover deferred
tools with `tool_search`, and calls `prime` through MCP. It does not advertise
shell RPCs as usable when the provider sandbox denies the supervisor socket.
An unavailable bridge or rejected initialization is a launch failure. Agents
report the server and selected policy without printing credentials. Direct CLI
socket errors retain the stable `unavailable` diagnostic. Coterie never retries
with broader filesystem, network, or shell approval permissions.

Background Codex jobs use the documented `allow_login_shell=false` setting.
Non-login shell tools preserve the inherited toolchain PATH. NixOS shells also
need the inherited `__ETC_PROFILE_DONE` and `__NIXOS_SET_ENVIRONMENT_DONE`
initialization markers: without them, even non-login Bash and Fish can reload
the system environment and discard devenv paths. The adapter preserves these
two markers when present and never invents them for an uninitialized parent.
Login startup may still change the environment through user profiles. The worker
environment includes the operator's `USER`, `LOGNAME`, and `SHELL` when present
so NixOS can locate the per-user profile and the provider can select the user's
shell. These values are runtime inputs, not authentication or path authority.
Toolchains needing other environment inputs must be entered explicitly in the
assigned workspace using the repository's documented development command.
The adapter does not inherit arbitrary `NIX_*`, `CARGO_*`, or shell startup
variables to recreate a development shell.

Bootstrap directs agents to diagnose validation environment access separately
from Git permissions. Validation evidence identifies the command, working
directory, selected policy, and whether each step passed, failed, or was blocked,
with the actual diagnostic. An inherited executable path does not prove a full
development environment. Agents use the repository's documented environment
entry in their assigned workspace when required. Nix daemon access and writes
to `.devenv` are separate probes: a writable workspace may permit local state
but deny daemon connections or state paths resolving outside that workspace.
No environment failure grants authority to widen permissions, reuse a primary
checkout's writable state, or bypass required checks. An authorized coordinator
may perform blocked validation under their own existing policy and record its
scope and outcome through ordinary durable messages. Coterie does not execute
validation requests or infer that blocked checks passed. NixOS regression tests
are opt-in and pin the build, provider and environment versions, commands, paths,
and effective policy; ordinary CI does not require NixOS or model access.

For a job agent, `COTERIE_PROJECT_ROOT` and the process working directory
identify the task's target project or isolated worktree. For the lead, they
initially identify the primary project. `coterie prime` always reports every
attached project, its alias, the caller's access, and the target project of each
visible task; agents should not infer project identity from repository names or
relative paths.

The token is random, scoped to one run, agent, and session generation, and
rotated when the session is replaced. The supervisor stores a verifier rather
than the raw token. Known credentials are redacted from Coterie-controlled logs
and transcripts.

Startup injection is a declared provider capability. If an archetype requires
guaranteed bootstrap instructions and the selected provider cannot supply them,
Coterie fails clearly rather than silently degrading to a normal user message.

## Agent and operator protocol

Agents coordinate through the installed Coterie binary:

```console
coterie whoami --json
coterie prime
coterie progress --limit 20 --json
coterie peers --json

coterie project attach ~/projects/eunoia-py --alias eunoia-py
coterie project list --json

coterie send reviewer-1 "Review the public API and error handling."
coterie inbox --wait --json

coterie task ready --json
coterie task create "Implement API" --project primary --group feature-7
coterie task create "Update Python bindings" --project eunoia-py \
  --after ct-01KUPSTREAM --input-from ct-01KUPSTREAM --group feature-7
coterie spawn worker --task ct-01K...
coterie finish --status completed --summary "Implemented the parser and added tests."
coterie workspace integrate --assignment ca-01K...
coterie task close ct-01K... --summary "Integrated and verified."
```

This is the ordinary cross-project workflow, not a separate orchestration mode.
The lead attaches the second project, creates one task for the library and
another for the bindings, and makes the latter depend on the former.
`--input-from` makes the accepted upstream tree available as a pinned input
without granting write access to the upstream project. The supervisor launches
each worker with the correct target working directory. The bindings task becomes
ready only after the library task is integrated, validated, and closed.

Agent requests carry the session capability token. Supplying an agent name never
changes the caller's identity or authority. Missing agent credentials do not
automatically grant operator authority.

Operator requests use a distinct local operator channel established by the
foreground or operator CLI. In the default same-user deployment, this
distinguishes normal code paths and prevents accidental privilege confusion, but
it is not a security boundary against a deliberately hostile same-UID process.
The trust model is described below.

All programmatic commands provide versioned JSON output and documented exit
codes. Successful JSON contains a schema version and never mixes diagnostic
output into standard output. Diagnostics go to standard error. Mutating commands
accept a caller-supplied operation ID for retries or allocate one before
dispatch, and return that operation ID after allocation.

Ordinary communication and lifecycle control are separate planes:

```console
coterie send worker-1 "Please check the failing integration test."
coterie agent interrupt worker-1
coterie agent terminate worker-1
```

A text message can never be interpreted as a process-control command. Lifecycle
operations require explicit capabilities and verified postconditions.

Messages receive stable IDs and are written durably before delivery is
attempted. Inbox reads use a monotonic cursor; acknowledgement is explicit and
idempotent. Delivery to a provider's live-steering interface is an optimization,
not the durable acknowledgement. Agents check their inbox at startup, task
boundaries, and before finishing.

`coterie finish` records the assignment outcome, summary, final session state,
and result metadata in one transaction. For a completed implementation task, it
also records the reported base and result commits and moves the task to
`submitted`. Task closure is permitted only when the caller has `task:close` and
the task's acceptance condition is met; otherwise the submitted task remains
visibly awaiting integration, review, or lead action.

Before recording a completed Git worktree result, the workspace backend must
prove the index and working tree are clean. Staged changes, unstaged changes,
and non-ignored untracked files reject submission with a conflict diagnostic
identifying the affected paths. Unreadable paths, unfinished Git operations,
and index flags that hide changes also prevent proof of cleanliness. Path
diagnostics escape filenames, bound the displayed list and each path, and
report omitted paths so large worktrees retain a usable conflict response.
Hidden-index diagnostics identify the flagged paths and explain clearing the
flags before inspection and retry. Ignored untracked files do not
block submission. A rejected attempt records no result or finish operation and
leaves the task, claim, and assignment active, so the agent can validate its
work, commit the intended changes successfully, and retry `finish`, including
with the same operation ID. A failed commit hook is not a successful commit.
An already successful operation replays its recorded outcome without inspecting
subsequent worktree changes.

Clean worktrees may submit their unchanged base commit, including review and
non-code assignments; Coterie does not require a new commit or infer task quality
from Git changes. Project and read-only assignments retain their existing
submission behavior, and `finish --status failed` remains available with dirty
work preserved. Bootstrap instructions and finish help explain the sequence:
validate, commit any intended changes, then finish. Unchanged Git worktree
results still follow guarded integration and explicit validated closure; see
the [unchanged-review acceptance guide](docs/review-acceptance.md).

### Linked-worktree commit handoff

Writable worktree assignments use an explicit coordinator-commit handoff.
Direct worker staging and committing is unsupported under the selected provider
policy: linked worktrees store their index, objects, and references outside the
assigned file tree, and filesystem write permission does not establish Git
metadata authority. Bootstrap diagnoses this before task work and requires the
worker to establish an available coordinator or operator with separately
authorized Git access before editing. If none is available, report a blocker.
Read-only assignments cannot request this as a way to acquire write authority.

`prime` exposes `commit_handoffs` for active writable worktree assignments,
including assignment and agent identity, the assigned workspace path and native
path bytes, owned reference, base commit, provider, and resolved permission
profile. This is the configured workflow, not a live filesystem probe or an
assertion that a coordinator has Git access. It survives reconnects and identifies
the fresh assignment after recovery. Completed and recovered source assignments
are excluded; recovery context remains available separately.

The worker validates its edits, then uses an authorized durable `send` to request
the commit, identifying the assignment, base, intended paths, proposed message,
validation commands and outcomes, and any blocked checks. It stops editing and
waits on its inbox. The recipient reviews the work and current ownership, stages
only the intended changes, and commits in that assignment's worktree through
their existing operator-approved Git access. They confirm the full commit ID in
a durable reply. The worker checks HEAD and cleanliness and calls `finish`;
submission, integration, validation, and closure retain their existing guards.
If interrupted while waiting, use normal recovery into a fresh worktree; never
commit into a recovered source on behalf of its continuation.

Messages convey a request, not executable commands or new authority. There is no
automatic commit, hook bypass, permission escalation, or Git-directory write
grant. Neither Coterie nor the provider may make the common Git directory
writable to solve this limitation. The primary checkout, sibling worktrees,
shared references, and repository configuration retain their protections.
Real-provider acceptance is opt-in and records the Coterie build, provider
version, selected policy, staging denial, handoff, submission, and recovery.

`coterie workspace integrate` is an explicit, capability-checked operation
requested by the lead or operator. It uses the workspace backend to apply a
submitted result to that task's target project and records exact
before-and-after identities. It preflights the operation without changing the
target and refuses dirty targets, unexpected target tips, ambiguous histories,
and conflicts. The Git working directory must still resolve to the attached
project, and every owned workspace path component must remain a real directory.
Checkout preserves ignored files. Index entries marked assume-unchanged or
skip-worktree make cleanliness unprovable, so integration refuses them with a
diagnostic. Coterie does not autonomously choose an integration order or
resolve conflicts.

Integration defaults to `rebase`: fast-forward when the target is still at the
recorded base; otherwise replay each contribution commit in order onto the
captured target tip. Replayed commits retain their authors and messages,
including empty commits, with a deterministic Coterie committer and the saved
integration timestamp. This adds no merge commits and preserves the target's
existing history. `--strategy merge` explicitly selects the former behavior:
fast-forward when possible, otherwise create a two-parent merge commit.
Results already reachable from the target require no new commit.

The CLI and agent MCP tool accept an optional strategy. Each new integration
plan records the resolved strategy before side effects, and the success response
and integration event report it. Retries use the original arguments and saved
plan, including when the strategy was omitted. Changing the strategy under the
same operation ID is a conflict. Migration 16 pins historical plans to `merge`, so
recovery never recomputes their commits using the new default.

Preflight checks the combined result without writing Git objects. Rebase also
checks each replayed commit before checkout or reference advancement; an
intermediate conflict is refused even if the combined result is conflict-free.
Commit construction may leave unreferenced objects after durable intent, but
never rewrites the submitted worktree or its owned reference. Reconciliation
reproduces the same commit IDs from the saved plan and original commits. Original
contribution commits remain recoverable through their owned reference even when
rebasing gives the integrated commits new IDs.

Git write return values alone do not establish observed success. Newly written
integration objects must be readable through a fresh repository handle with
their expected type and content hash. Before recording integration, a fresh
handle must observe the planned target branch and exact resulting commit, its
readable tree, and a clean index and working tree. Reconciliation applies the
same checks after interrupted publication. Failed writes preserve durable
intent and recoverable files; an ambiguous partial checkout remains unknown
until the operator repairs it. See the [publication audit](docs/git-publication.md).

### Bounded current context and full details

`prime [--after-task ID] [--limit 1..50]` returns compact task and assignment
context, defaulting to 20 tasks ordered by stable task ID. `next_task` and
`has_more` support continued inspection. This is a current view, so callers
refresh from the beginning after transitions. The caller's latest assigned task
is pinned in `current_task` on every page, including after submission, failure,
recovery, and closure. `active_task` identifies only an active assignment.
Ready-task IDs refer to the displayed page. Neither provider exit nor a compact
summary establishes acceptance.

Task titles, descriptions, result summaries, and assignment reports use UTF-8
previews with original byte counts and explicit truncation. Dependency previews
include omitted counts. Assignment context reports recorded session state,
generation, integration state, and mechanical next steps, preserving uncertainty
about provider activity. Recovery previews retain source and continuation IDs,
base commit, preserved path, and reason, with full details available by source
assignment ID. Long histories and reports never repeat in ordinary context.
The serialized compact task section is capped at 64 KiB by shortening the page;
identity, project, peer, command, and active commit-handoff metadata are separate
and scale with the run configuration. The bounds and measured complete response
sizes are documented in `docs/context-inspection.md`.

`task show ID` and `assignment show ID` (MCP `task_show` and `assignment_show`)
return full stored detail documents through UTF-8 byte pages, defaulting to
16 KiB and accepting `--limit 1..65536`. These reads require operator authority
or `task:read`, use the existing run-wide task visibility, and reauthenticate on
every request. Task details include full descriptions and results and references
to every assignment. Assignment details include the full report, workspace
identity, and recovery provenance. IDs remain valid after transitions. A page
returns `text`, `next_cursor`, `total_bytes`, `eof`, and a SHA-256 `revision` of
the complete JSON document. Continuations require that revision and refuse a
changed document; restart at zero to read the new version. Concatenate page text
before decoding JSON. These reads introduce no authority or persistent schema.

`logs --tail --limit N` (MCP `logs` with `tail: true`) reads the most recent
bounded raw transcript bytes without draining startup context. It returns the
starting byte offset, total bytes, and whether the first line is partial, along
with the ordinary session and next cursor. Tail selection applies only to the
first read when following; subsequent reads use the returned cursor and session.
It does not infer semantic activity or discard bootstrap, serialized context,
unknown frames, or incomplete final frames. Ordinary `logs --after 0` retains
full transcript access. Inspection measurements count repeated bootstrap and
serialized context as transcript bytes and report both raw and serialized page
sizes; they do not claim a provider token-cost measurement.

### Compact progress inspection

`coterie progress [--after <cursor>] [--limit <1..100>] [--wait <0..5>]`
returns one bounded page of durable lifecycle changes. The default limit is
100, and the default wait is zero seconds. The operator and authenticated
agents with `task:read` may inspect this view. It uses the current run-wide
task and peer visibility of `prime`; role names grant no authority. Every
poll rechecks the caller's current session generation and capability.

Changes contain sequence numbers and typed IDs and states only: task creation
and lifecycle, assignment creation, lifecycle, and session association, agent
creation and lifecycle, and session creation and lifecycle. Agent records
include the caller as well as peers. Task bodies, titles, results, messages,
paths, provider details, raw event payloads, and other operator events are
excluded. Assignment `completed` and task `submitted` report a submission;
session or agent `exited` reports provider lifecycle independently. Neither
implies task acceptance. These are historical transitions, not a current-state
snapshot; consumers apply them in sequence order.
Preparing a replacement foreground generation records both the agent's new
`starting` state and the new session atomically, before any provider observation.

The opaque versioned cursor pins the run, caller (operator or agent ID), and
last scanned durable event sequence. Omitting it starts at zero. Responses
include `next_cursor`, `has_more`, and `timed_out`. Clients resume with the last
returned cursor after reconnecting to the same run; a replacement session for
the same agent may reuse it after authenticating anew. A different caller or
run, a malformed cursor, or a sequence beyond the durable high-water mark is
refused. Cursors grant no authority, require no acknowledgement, and do not
depend on connection memory. Replaying one deliberately repeats its page.

Each inspection scans at most 256 event rows and returns at most the requested
number of changes. Only fixed-size IDs, numbers, and enumerated states cross
the boundary, keeping a page below 64 KiB regardless of task or event size.
Excluded events advance the cursor without exposing their contents. `has_more`
means the scan has not reached the high-water mark observed in that transaction;
clients continue even when such a page contains no changes.

Waiting is allowed only while caught up and returns when a change or further
page becomes available, or when the requested deadline expires. A timeout is a
successful empty page with `timed_out: true` and an updated cursor. Waiting
releases the supervisor between bounded polls and stays within the ordinary
RPC response deadline. Disconnects do not mutate state or acknowledge changes;
transport failures remain `unavailable`, and callers explicitly reconnect with
their last cursor. Progress never opens the database from an agent process or
falls back to operator event inspection. It requires no provider live steering.

## Providers and session state

Provider-specific behavior is isolated behind an internal interface resembling:

```text
probe() -> version, capabilities, compatibility
launch_interactive(specification) -> session handle
launch_job(specification) -> session handle
resume(session) -> session handle
observe(session) -> lifecycle state, activity state
steer(session, message, expected turn)
interrupt(session)
terminate(session)
attach(session)
stream(session) -> provider events
```

Lifecycle and activity are separate. Lifecycle states include `starting`,
`running`, `exited`, `lost`, and `quarantined`; semantic activity is `busy`,
`idle`, or `unknown`. A running process may have unknown activity.

Relevant provider capabilities include:

- startup instruction injection;
- foreground interactive sessions;
- background job execution;
- structured lifecycle events;
- live steering with turn identity;
- interrupt and termination;
- session resume;
- semantic busy or idle state;
- transcript streaming and attachment;
- multiple declared project roots and dynamic root expansion;
- enforceable filesystem, network, and approval policies.

Capabilities are discovered from the installed provider version where possible,
not merely asserted by configuration. An archetype is validated against them
before launch.

Codex is the first provider. In the initial product target:

- the lead runs as an ordinary foreground Codex TUI owned by the foreground
  Coterie process;
- background workers run as bounded, non-interactive jobs using Codex's
  documented JSONL output;
- each background worker starts in its assigned project or worktree, independent
  of the lead's current directory;
- workers expose logs but are not attachable terminal sessions;
- live steering and transparent lead reattachment are unavailable unless a later
  structured Codex adapter can provide them reliably.

Attaching a project does not silently widen a live provider's filesystem
sandbox. If the foreground provider cannot add a root safely at runtime, the
current lead coordinates that project through task-scoped workers and Coterie's
structured result and integration operations. An adapter may instead restart or
resume the lead with the expanded project set when the provider supports that
transition explicitly. `coterie prime` reports the distinction between a project
attached to the run and a project directly accessible to the current provider
session.

A structured Codex app-server adapter is a future capability path, not a
requirement for the initial product target. The provider interface must
accommodate it without making an experimental protocol part of Coterie's core
contract.

Every provider adapter has a shared conformance suite covering launch, output
framing, exit classification, cancellation, timeout, resume where claimed,
transcript tails, malformed events, and idempotent termination. Deterministic
fake providers exercise reconciliation and failure paths without invoking a
model.

## Workspaces and Git

Workspace policy is declared per role:

- `project`: work directly in the task's target project directory, normally used
  by the lead or an explicit integrator;
- `worktree`: use an isolated Git worktree belonging to the task's target
  project, normally used by implementation workers;
- `read-only`: inspect the target project or worktree under an enforceable
  provider permission profile. Its effective filesystem policy must be
  `read-only`; configuration validation rejects a writable profile.

Every assignment resolves its workspace from its task's project identity, never
from the supervisor's or caller's current directory. This rule keeps
cross-project delegation deterministic even when two repositories have similar
names or layouts.

Workspace records are immutable bindings to individual assignments, not a
registry that consumes a project directory permanently. Each `project` or
`read-only` assignment retains its own record and generation even when earlier
assignments used the same canonical project directory. Read-only assignments
may overlap, subject to configured role and run limits, and may inspect a
project with an assigned writer. They observe the live directory, not a frozen
snapshot. No role name changes these rules.

At most one background `project` assignment may hold writable ownership of a
project directory. Admission remains blocked until the previous assignment is
terminal and every session for its agent has an observed exit. Missing session
association, unknown process state, a submitted result without process exit,
and an active or draining assignment do not release that ownership. This guard
does not make the foreground operator's project directory isolated. Retrying a
spawn reuses its original assignment and workspace binding, including after
recovery, without granting new ownership.

An isolated `worktree` path remains reserved to its original assignment for the
lifetime of its record, regardless of completion or cleanup. No other workspace
binding may reuse that path. Assignment, project, run, generation, kind, path,
and base commit remain fenced and immutable; shared project-directory use never
relabels or deletes historical assignments or relaxes Git ownership checks.

For a Git-backed `--input-from` dependency, Coterie materializes the closed
task's accepted tree, without repository administrative data, at its recorded
integration commit beneath the consumer's workspace. It reports the alias, path,
and commit through `coterie prime`. This snapshot is contextual input, never an
integration target, so provider writes to it cannot alter the upstream project.
A task that requires a live external path or a non-Git input must request it
explicitly and can launch only when the permission profile and provider can
enforce the requested access.

Coterie uses `git2` for its own repository and worktree operations. Git behavior
is isolated behind an internal workspace trait so its safety rules can be tested
independently of provider behavior.

A worker worktree is created from a recorded base commit beneath the run's state
directory, partitioned by project and assignment identity. It uses a dedicated
Coterie-owned reference in the target repository named by run and assignment
identity. Coterie records desired ownership before creation and observed
repository identity afterward.

Before cleanup, Coterie verifies all of the following:

- the path resolves beneath the expected Coterie state directory;
- the database records ownership by the current run and generation;
- the Git administrative data identifies the expected repository and worktree;
- no provider process still owns the workspace;
- the worktree is clean and its commits remain reachable or explicitly
  preserved;
- the work has been integrated or the operator explicitly approves removal.

Coterie never automatically destroys a dirty, unintegrated, or otherwise
recoverable worktree. Failed cleanup leaves a diagnostic and a recoverable path.
`coterie doctor` reports stale and inconsistent ownership but does not repair
destructive cases without explicit approval.

Workers report their target project, base commit, result commit, summary, and
tests to the lead. The initial product target does not implement an autonomous
merge queue. The lead decides when to invoke guarded integration, validate the
result, and close the task. Integration in one project never changes another
project's worktree or branch.

Non-Git projects support `project` and enforceable `read-only` roles. An
archetype requiring `worktree` fails clearly for a task targeting a non-Git
project; Coterie does not silently weaken isolation.

## Reconciliation and supervision

The supervisor periodically and eventfully reconciles durable desired state with
project leases, provider processes, assignments, and workspaces. Reconciliation
is idempotent: repeating it with the same desired and observed state produces no
additional side effects.

Each owned resource carries a run ID and generation. Late output or process
exits from an earlier generation cannot mutate the current session. Provider
config fingerprints detect drift without relying on timestamps.

The supervisor adopts a live process only when the provider can prove its
identity and configuration generation. Ambiguous processes are reported as
unknown rather than killed or adopted.

Foreground recovery may record a session as lost when its saved provider
identity proves that the process is absent. This establishes neither an exit
status nor task success. A live, inaccessible, or unrecognized foreground
process remains unknown; only its wrapper owns process control and exit-status
observation.

A later exit observation from the owning foreground wrapper may refine a lost
session to exited within the same generation. Its exit details are recorded,
and its revoked credentials remain revoked. Other terminal states and stale
generations retain their existing fences.

Restarts are bounded. Repeated failures within a configured window quarantine
the session and emit a visible event. The supervisor does not spin indefinitely
or consume unbounded provider quota. The default policy allows three launch
attempts in a 60-second window, with exponential retry delays starting at one
second. Only a failure proved to precede process creation permits automatic
retry of that launch intent. Exhausted launch retries quarantine the session;
three foreground process failures in one window quarantine that session and
block replacement for 60 seconds. Repeated spawn preflight failures are also
bounded to three attempts. A worker that has executed remains for the lead to
inspect and recover; Coterie does not automatically repeat its task or transfer
its workspace to another generation.

Provider probes have a two-second deadline and bounded output. Session startup
has a 30-second deadline, and background jobs have a one-hour execution limit.
Interactive sessions have no execution limit. These are elapsed execution
bounds, not inferences about semantic activity. Timeouts initiate the same
bounded process-control phases as shutdown. Restart admission, quarantine,
control intent, and deadlines survive supervisor replacement; uncertain
in-flight launches remain unknown. Trusted operator configuration owns these
bounds; trusted global policy sets them when the run snapshot is created.

Shutdown proceeds in phases:

1. Stop accepting new spawns.
2. Mark affected assignments as draining.
3. Interrupt all targeted sessions.
4. Wait for a bounded grace period.
5. Terminate survivors explicitly.
6. Reconcile task and workspace state without deleting recoverable work.
7. Release attached-project leases and retire their active-run index entries.

The shutdown intent and draining assignments commit before any process control.
Both foreground launches and worker spawns are rejected while draining,
including launches attempted by reconciliation. The default supervision policy interrupts
first, sends `SIGTERM` after 250 milliseconds, and sends `SIGKILL` to verified
survivors after 2.5 seconds. The foreground wrapper controls its own child;
the supervisor never signals an ambiguous PID. The overall grace and
observation deadline is five seconds. A timeout emits a visible event and
returns an error while the run remains active and launches remain blocked.
Retrying the same stop operation rechecks progress without extending deadlines.
A later terminal observation can complete shutdown.

Shutdown preserves unfinished tasks, their claims, draining assignments,
transcripts, and workspaces for inspection. It does not infer task success,
reopen work automatically, materialize missing workspaces, or delete owned
references. Workspace observations and run completion precede socket and index
retirement and lease release. Recovery can finish retirement if a crash occurs
after the stopped state commits. M4 applies these phases to the primary project;
Attachment retirement removes secondary indexes before the primary index, then
releases all leases. The primary index therefore remains a recovery entrypoint
after an interrupted retirement. Finishing retirement of a stopped run acquires only the
projects still indexed to that run and preserves indexes belonging to newer runs.
Stopping from any attached project waits for the run's socket and all of its
attached-project index entries to retire. Entries belonging to newer runs do
not delay completion.

If `coterie stop` cannot reach the indexed supervisor, it starts a replacement
for that same run under its saved configuration. Recovery validates the existing
database and project identities, applies supported migrations, and acquires the
attached-project leases. It commits shutdown intent before reconciling sessions
or workspaces, so pending launches remain blocked. Current configuration files
and locks do not prevent stopping an older run. A missing database, unsafe
socket, or uncertain process state never authorizes discarding run state.
Interrupted completion is acknowledged from the committed stop operation, and
the caller waits for socket and index retirement without creating another run.

## Events and observability

Every state transition emits an immutable typed event in the same transaction
that records the transition. Events have:

- a monotonically increasing run sequence number;
- timestamp, type, actor, and subject;
- run, project, agent, task, and operation identifiers where applicable;
- correlation and causation identifiers;
- a versioned structured payload;
- a concise human-readable summary.

Watchers resume after a sequence cursor. This supports
`coterie events --follow`, status reconstruction, audit trails, and future
editor integrations without coupling producers to consumers.

Provider-native event frames may be retained separately, but Coterie emits
normalized lifecycle events for portable behavior. Unknown provider fields are
preserved only in the raw transcript, not promoted into the stable Coterie event
schema accidentally.

`coterie doctor` checks at least supervisor reachability, database migrations,
configuration and lock compatibility, provider versions and capabilities,
abandoned operations, stale assignments, task cycles, transcript accessibility,
and worktree ownership. Inspection is read-only, including when the supervisor
is unreachable: an offline reader may open an existing private database without
migrations or mutations. It verifies external configuration and locks and compares current effective
policy with the active snapshot. Recovery remains the existing
lease-protected startup and desired-state reconciliation path. An indexed run
must have a matching durable database; a responsive socket or ambiguous file
ownership is never discarded as stale.

Doctor reports supported pending migrations separately from modified,
noncontiguous, or newer schemas. Snapshot validation remains unavailable until
the supervisor performs that upgrade; doctor names startup as the next step
without decoding the historical document as the current schema or modifying it.

Foreground terminal diagnostics use startup evidence from the wrapper that owns
the provider child. It records the child's PID, Linux boot identity, start time,
user ID, and inherited input identity with the startup observation. This evidence
is immutable for that session generation; older sessions without it remain
unverified. Reading a stored PID or a durable `running` label cannot establish
process ownership.

Doctor opens the process's procfs directory, verifies the recorded identity,
and inspects its input through that pinned directory without reading terminal
input, writing output, changing terminal settings, or sending signals. A matching
Linux PTY slave whose inode has been unlinked identifies a closed terminal.
A linked PTY indicates no observed terminal loss, regardless of whether an editor
currently displays it. Missing processes, changed input, inaccessible procfs,
unsupported terminals, and mismatched process identities produce explicit
uncertainty rather than a healthy-session claim. Inspection rechecks identity
after observing the terminal and never changes durable session state. Recovery
uses `coterie stop` for bounded shutdown before relaunching; a failure to verify
shutdown preserves the run and its work for operator inspection.

Doctor distinguishes the operator-channel supervisor handshake and static
provider version, CLI, and MCP configuration checks from authenticated agent
connectivity. It reports `agent_connectivity` as `unavailable` for each enabled
role because it has no live probe of the provider-launched bridge or sandboxed
commands. A successful operator handshake, static probe, or earlier agent call
does not establish current agent access. Diagnostics identify the saved role
policy when available, otherwise label the current configuration as unverified
for an active run. Provider checks use that same policy. If neither configuration
is readable, a single unavailable check names the configuration prerequisite.
The next step is to call `prime` through the agent's Coterie MCP tools. When
operator access succeeds but that call fails, inspect the required bridge's
startup diagnostics and selected policy without widening permissions. Doctor
does not launch a model session, obtain agent credentials, or change policy to
perform this check.

Socket retirement verifies the original filesystem inode, private ownership,
one link, and listener inactivity before unlinking. Stale socket repair pins
the inspected inode and rechecks it after the connection probe. Active-run index
publication and retirement require the matching, still-held project lease and
validate the existing entry before replacement or removal. Conflicting entries
remain intact. Failed publication preserves its temporary file for inspection.

Event following emits bounded pages with sequence cursors. New events must fit
the 900 KiB page budget before their mutations commit. Older, larger events are
returned individually so readers can advance past them. Operator followers
reconnect only to the same run, and may read its immutable final pages after
shutdown retires the socket, draining all events through the final empty page.
Transcript pages use session IDs and byte cursors, keep UTF-8 characters whole
where possible, and preserve incomplete final JSONL
frames as data. Streaming redaction retains possible credential prefixes between
chunks, conceals interrupted prefixes, and filters known provider credentials
and Coterie tokens before controlled storage. Runtime checks validate ownership,
private modes, and file types, refusing symlinked and hard-linked data files.

## Trust and permission model

Coterie distinguishes these trust classes:

  | Input or component                         | Trust                                 | Rule                                                                                                 |
  | ------------------------------------------ | ------------------------------------- | ---------------------------------------------------------------------------------------------------- |
  | Compiled defaults and global configuration | Trusted operator policy               | May select provider executables and grant maximum authority.                                         |
  | Project `coterie.toml`                     | Untrusted declarative request         | May only select trusted definitions and reduce authority or limits.                                  |
  | Operator-attached project path             | Trusted host authority                | Must be canonicalized and explicitly attached; it grants no authority beyond resolved global policy. |
  | Repository content and task text           | Untrusted data to Coterie             | Never becomes a command, path authority, or policy value through interpolation.                      |
  | Provider executable                        | Trusted operator-selected code        | Runs only with the resolved permission profile and explicit environment.                             |
  | Agent behavior                             | Untrusted within granted capabilities | Supervisor RPCs enforce identity, state transitions, and limits.                                     |

Role capabilities and tokens protect the supervisor protocol from confused or
accidental use. When agents and the operator run as the same Unix user with
access to the same host, they do not form a hostile security boundary: a
sufficiently capable same-UID process may inspect other processes or
user-readable runtime files. Strong isolation requires a future hardened mode
using separate operating-system identities, user namespaces, or containers.

The ordinary provider sandbox is the default policy. Only an explicitly
operator-selected unrestricted profile disables it; failures never authorize
an automatic retry with broader access. Unrestricted processes can access other
workspaces and same-user runtime state, so the supervisor's logical ownership
checks do not provide host isolation for them.
`read-only` is advertised only when the selected provider can enforce it.
Workspace isolation prevents concurrent Git changes from colliding; it does not
by itself restrict filesystem or network access.

### Supervisor transport

The opt-in tests on Linux with Codex 0.153.4 reproduce the reported socket
denial even when global Unix socket access is allowed. The standalone probe
selects the workspace policy explicitly; it does not test TUI flag precedence.
A named profile with an exact socket allowance and network disabled also
receives `EPERM` under both writable and read-only filesystem policies. The
scoped socket proposal therefore cannot provide the required Linux transport
on this version.

Inspection of the source packaged with the installed provider confirms the
cause: restricted network mode unconditionally denies `connect()` in
`codex-rs/linux-sandbox/src/landlock.rs`. Proxy-routed mode denies creating
`AF_UNIX` sockets, and `unix_socket_permissions_supported()` in
`codex-rs/network-proxy/src/runtime.rs` is true only on macOS. The provider's
own non-macOS test rejects even the global Unix socket allowance. These are
version-specific findings, not an interface Coterie may depend on.

A session needs access to the run supervisor as part of its orchestration
capabilities. This local RPC access is separate from the permission profile's
task network access: `network=deny` must still prohibit external and other local
network destinations. The socket grant must not grant writes to its containing
runtime directory or the run database, and every request must retain the
existing token and generation checks.

An adapter may use a provider-supported, exact-path socket allowance only when
it can enforce these restrictions together with the selected filesystem and
approval policies. A global Unix socket allowance or a successful connection
from the operator does not prove access from a sandboxed command. The adapter
must check the effective policy, including configuration precedence, rather
than infer connectivity from a version or CLI option alone. Unsupported or
denied access must fail before starting agent work, with a policy-preserving
diagnostic. It must never retry by disabling the sandbox, enabling unrestricted
network access, granting a runtime-directory write root, or allowing arbitrary
Unix sockets. Real-provider regression tests remain explicit opt-ins.

Coterie provides a stdio MCP endpoint through the private `__mcp` entrypoint
of the same binary. Codex launches one bridge per session through its supported
MCP configuration for interactive sessions and jobs. This fixed-function broker
runs outside the command sandbox. It receives the run socket and complete
session-scoped agent credentials through an explicit environment allowlist,
authenticates with `whoami` before MCP initialization, and forwards typed agent
RPCs through the supervisor's existing authentication and capability checks.
Missing, incomplete, invalid, or stale credentials fail closed. The bridge has
no operator channel, arbitrary command execution, general file access tool, or
direct database mutation. Tool arguments cannot select a different caller or
socket. The supervisor remains the only run database writer.

The generated MCP catalog defines the exact tool allowlist. The adapter
preauthorizes only those named Coterie tools through Codex's per-tool approval
settings: invoking the transport may exercise existing Coterie role authority,
including from non-interactive jobs, but cannot acquire new role authority.
Every request still undergoes supervisor authorization. This is separate from
the provider's command approval policy, which remains unchanged; managed
provider requirements may still deny a tool. New or unrelated MCP tools receive
no automatic approval from this configuration. Invalid or denied operations
return explicit errors rather than retrying through another channel.

Each mutation requires an operation ID. `new_operation_id` allocates one, and
bootstrap instructs agents to preserve it and identical arguments on retries.
The bridge saves original typed mutation requests before dispatch and retries
once after a transient connection failure, authenticating again against the
same run. `retry_mutation` resends a retained request by operation ID through
the supervisor's normal generation and capability checks. The bounded in-memory
request cache never evicts unresolved requests. A restarted bridge requires the
original tool arguments and operation ID. Read checkpoints survive bridge or
session replacement for the same run and agent and grant no authority. The
[client helper contract](docs/client-bookkeeping.md) specifies bounds, partial
message handling, replay, and failure tests. No client database or new persistent
schema is introduced.
MCP uses bounded newline-delimited JSON-RPC, negotiated protocol version
`2025-06-18`, typed input schemas, and matching redacted textual and structured
results. The generated [tool catalog](schemas/mcp-tools-v1.json) is checked
against the Rust input types. Unknown tools and operator-only fields are
rejected before forwarding.

The adapter probes the installed provider's stdio configuration interface and
requires its configured bridge to initialize. Unique session server names
prevent a prior generation's tool catalog from proving startup readiness.
For `codex exec`, all generated permission and MCP configuration overrides
follow the `exec` subcommand so its own configuration parser receives them.
Real-provider acceptance covers the foreground TUI in a PTY and actual worker
launches under writable and read-only profiles. It checks tool discovery, allowed
and denied agent RPCs, stale credentials, failed initialization, and preserved
command restrictions, including a conflicting provider network default.

An agent with `project:attach` may attach only a canonical path beneath a
trusted global `allowed_project_roots` entry. The array defaults to empty and
accepts only absolute existing directories. Loading configuration resolves its
symlinks, and the run snapshot pins the canonical roots. Portable locks exclude
these host paths. Migration 12 adds an explicit empty allowlist and compiled
provenance to older snapshots without changing their portable fingerprints or
other saved policy. Merely mentioning a path in task
text or repository content never grants access. The operator may explicitly
attach a path outside those roots through the operator channel; the decision and
resolved identity are recorded. Project configuration cannot extend the
allowlist. Attachment authority and provider filesystem authority are checked
separately.

Secrets and ambient environment variables are denied by default and passed only
through compiled defaults or trusted global configuration. The Codex MVP
allowlist contains `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `CODEX_HOME`,
`OPENAI_API_KEY`, `__ETC_PROFILE_DONE`, and `__NIXOS_SET_ENVIRONMENT_DONE`,
preserving tool discovery and Codex authentication without exposing the
complete supervisor environment. Coterie redacts exact known credential values
from storage it controls, stores token verifiers rather than raw tokens, and
keeps runtime files private to the current user. It does not promise to sanitize
a provider's independently managed transcript if the provider itself prints or
stores a secret.

## Safety and reliability invariants

- Project configuration cannot introduce executable commands, grant
  capabilities, weaken permission profiles, or increase resource ceilings.
- Agents can instantiate only declared roles and only within role and global
  limits.
- Every task has exactly one writable attached target; dependencies, read-only
  inputs, and task groups may span projects.
- Project aliases are unique, paths are canonicalized, and each attached
  identity has one exclusive active-run lease.
- Cross-project dependencies are satisfied only by accepted, closed upstream
  tasks, not by provider exit or assignment submission.
- Task claims and assignment creation are atomic and idempotent.
- The run supervisor is the single database writer.
- Every external side effect has durable intent and a reconcilable observed
  result.
- Agent, session, assignment, and workspace ownership is fenced by run and
  generation.
- Messages are durable before live delivery is attempted.
- Provider state that cannot be established is `unknown`.
- Stop, restart, and cleanup operations have bounded timeouts and verified
  outcomes.
- Destructive cleanup requires positive proof of ownership and recoverability.
- No role name or delegation strategy is hardcoded into the runtime.
- Provider arguments are passed as structured arrays; untrusted text is never
  evaluated by a shell.

## Initial product target

The complete initial product target includes:

1. A single compiled `coterie` binary, apart from the selected agent provider.
2. Versioned built-in archetypes, trusted global archetypes, safe project
   restrictions, provenance, and optional lock files.
3. Primary-project discovery, attached-project identities, exclusive leases, and
   deterministic run state.
4. An automatically managed singleton per-run supervisor discoverable from every
   attached project.
5. A native SQLite task graph with project targets, cross-project dependencies,
   assignments, messages, operations, and typed events.
6. Codex as the first provider, with one foreground lead and bounded one-shot
   background workers.
7. Provider-level bootstrap injection and `coterie prime`.
8. Authenticated agent RPC with durable inboxes and explicit acknowledgements.
9. Native `git2` worktree creation, guarded integration, ownership fencing, and
   conservative cleanup in each target project.
10. Desired-state reconciliation, bounded shutdown, and crash-loop quarantine.
11. `status`, `progress`, `logs`, `send`, `spawn`, `finish`, `stop`, `project`, `task`,
    `workspace`, `events`, `doctor`, and configuration inspection commands.
12. Versioned structured output, stable exit codes, and operation IDs for every
    agent-facing command.
13. Provider and workspace conformance tests using deterministic fakes and
    temporary real repositories.

The implementation should be organized around the following internal components:

```text
cli          command parsing and human/JSON presentation
config       loading, provenance, policy intersection, locking, and validation
project      discovery, identity, run-scoped attachment, and leases
supervisor   desired-state reconciliation and process ownership
protocol     authenticated local RPC and versioned wire types
providers    out-of-process agent-harness adapters
tasks        native task graph, claims, groups, comments, and assignments
workspace    project and Git-worktree management through git2
state        SQLite migrations, transactions, operations, messages, and events
transcript   append-only provider output and normalized event ingestion
```

Implementation proceeds test-first around state transitions. Unit tests cover
configuration lattices, task readiness, cross-project dependency release, atomic
claims, capability checks, and reconciliation plans. Integration tests use
multiple temporary Git repositories and fake providers to exercise attachment
conflicts, project-specific working directories, guarded integration, and
crashes at every durable-intent boundary. Provider contract tests are shared by
fakes and the Codex adapter.

## Non-goals for the initial product target

Coterie initially does not provide:

- Beads or another required external task tracker;
- cross-machine task synchronization;
- persistent project registration, cities, or fleets;
- Kubernetes, containers, or hardened multi-UID execution;
- a remote archetype registry;
- project-defined executable hooks or provider commands;
- a general workflow or formula language;
- an autonomous graph scheduler or elastic worker pools;
- automatic merging or pull-request management;
- a web dashboard or custom foreground TUI;
- transparent lead-process reattachment or general live steering;
- its own model API client;
- compatibility with every coding-agent CLI.

These features may be considered later, but should not compromise the
project-native experience or turn Coterie into a general infrastructure
platform.

## Design criterion

The design succeeds when a user can open a project in Neovim, launch Coterie
through sidekick.nvim, and converse naturally with one lead agent while that
agent safely delegates work through Coterie orchestration tools. The same conversation can
implement a change in one project and then update a dependent project, with each
worker launched in the correct working directory and the downstream task blocked
until the upstream result is integrated and verified. Configuration is
reproducible and inspectable, tasks and handoffs survive process failures,
parallel changes are isolated, partial operations converge to a recoverable
state, and no separate orchestration workspace or companion task CLI needs to be
installed or maintained.
