# Coterie

## Status

This document describes the intended architecture and initial scope of Coterie. It is a design target rather than a compatibility promise.

[`TODO.md`](TODO.md) separates delivery into milestones. Its `v0.1.0` MVP is a
deliberately smaller single-project vertical slice; the complete initial product
target described here follows after that MVP. Unless a passage explicitly names
the MVP, references to the initial product target mean the complete scope in the
section of that name below.

## Purpose

Coterie is a project-native CLI for orchestrating coding agents. From a user's perspective, it should feel like launching an ordinary agent harness in the current project:

```console
cd my-project
coterie
```

The user interacts with one foreground lead agent. Coterie supplies that agent with a declaratively configured group of workers, an embedded durable task graph, isolated workspaces, and a CLI protocol for coordination. A run begins in one primary project and may attach other projects when a request spans repositories.

Coterie is inspired by Gas Town, Gas City, and Firstmate, but differs in five important respects:

1. Configuration is declarative and centered on reusable, versioned archetypes.
2. A run is anchored in the current project and may attach explicit additional projects; projects are not permanently registered in a separate city or fleet.
3. Coterie behaves as an agent harness and can run directly in a terminal integration such as sidekick.nvim.
4. Orchestration behavior and durable work tracking belong in the compiled binary rather than shell scripts or required companion CLIs.
5. Agent harnesses remain out-of-process providers. Coterie does not absorb their model clients, authentication, tools, or sandboxes.

The central product principle is:

> Running Coterie in a project should be as natural as running Codex or Claude Code there directly.

## Design principles

- **Project-native operation**: launching in a project is sufficient. Additional projects are attached only when a run needs them; there is no Coterie initialization or permanent project registry.
- **Libraries inside, protocols outside**: Coterie uses Rust libraries for state, Git operations, configuration, IPC, and process management. External processes exist only at deliberate provider boundaries.
- **Durable work, disposable sessions**: tasks, assignments, messages, and handoffs survive provider exits and supervisor restarts.
- **Configuration-defined behavior**: role names and delegation strategies are data. The binary contains no special cases for `lead`, `worker`, `reviewer`, or other archetype-defined names.
- **Mechanics in Rust, judgment in agents**: Coterie enforces policy, ownership, state transitions, and transport. Agents decide how to decompose work, what to delegate, and whether an outcome satisfies the user's request.
- **Desired-state reconciliation**: crashes and partial operations are repaired by comparing durable intent with observed state, not by assuming each multi-step operation completed.
- **Explicit uncertainty**: unknown provider or process state remains unknown. Coterie does not infer semantic idleness from CPU use, elapsed time, or terminal text.

Before adding a new core abstraction, ask whether it can be composed from existing concepts, whether it remains useful as models improve, and whether it would move judgment from an agent into Rust.

## Concepts

- **Project**: a canonical Git worktree, or a canonical directory for a non-Git project, attached to a run under a unique human-readable alias.
- **Primary project**: the project from which Coterie was launched. It anchors configuration, run discovery, and the lead's initial working directory.
- **Attached project**: an additional project root granted to an active run. Attachment is run-scoped and does not register the project permanently.
- **Archetype**: a reusable declarative description of roles, providers, permission profiles, workspace policies, and resource limits.
- **Role**: a configured type of agent. Roles have no built-in semantics.
- **Agent**: one instantiated role within a run.
- **Run**: one active orchestration spanning a primary project and zero or more attached projects.
- **Session**: one live or resumable provider execution associated with an agent.
- **Task**: a durable unit of work with exactly one writable target project and, when needed, explicit read-only input projects.
- **Assignment**: the durable association between a task, an agent, and a workspace.
- **Task group**: a lightweight grouping of related tasks created for one user request or delegation wave.
- **Event**: an immutable, sequenced record of something that happened in a run.

There is no persistent equivalent of a Gas Town town or Gas City city. An archetype is instantiated directly in the current project as a run, and any additional project membership lasts only for that run.

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

No initialization or project registration is required. Coterie discovers the primary project root, starts or connects to its run supervisor, and launches the lead agent there. The operator or an authorized lead may attach another canonical project root for the lifetime of the run.

An optional `coterie.toml` selects a versioned archetype and applies a narrow set of safe project overrides. An optional `coterie.lock` verifies the portable, non-secret effective configuration. Both files may be committed. Coterie must also work without either one.

For sidekick.nvim, Coterie should require only a normal custom CLI entry:

```lua
tools = {
  coterie = {
    cmd = { "coterie" },
  },
}
```

Coterie must behave correctly as a foreground terminal program: preserve the working directory, forward signals and terminal resize events, avoid unsolicited terminal output while the provider TUI is active, and return meaningful exit codes.

In the initial product target, the foreground Coterie process owns the lead TUI, while the supervisor owns background workers. Closing the foreground process ends that live TUI but does not discard the run or stop active workers. A later invocation resumes a recorded provider session when the adapter supports reliable resume; otherwise it starts a fresh lead session and reconstructs orchestration context through `coterie prime`. The initial product target does not promise transparent process reattachment.

`SIGINT` is forwarded to the foreground provider. It does not implicitly stop the run. Stopping all agents and cleaning up eligible resources requires `coterie stop`.

## Native dependencies and external boundaries

Coterie should be a single compiled Rust binary apart from the agent harnesses the operator chooses to run.

Core facilities use in-process Rust libraries:

- SQLite through `rusqlite`, or an equivalent narrowly scoped SQLite crate, for tasks and orchestration state;
- `git2` for repository discovery, status inspection, references, and worktree management;
- `serde`, `toml`, and `schemars` for typed configuration and generated schemas;
- `tokio` and focused Unix process, signal, socket, and PTY crates for runtime management.

Coterie owns the task domain and schema; SQLite and `rusqlite` supply storage and transactions rather than task semantics. Dependencies are locked, use narrowly selected features, and are reviewed like other trusted code. The `git2` boundary remains behind a trait so Coterie can move to a pure-Rust Git implementation when one supports the required worktree mutations reliably.

Coterie-owned code does not invoke `bd`, `git`, `sh -c`, or another general-purpose CLI to implement its state machine. A provider process may invoke tools such as Git as part of its own agent work; that behavior belongs to the provider's sandbox and permission policy, not to Coterie's internal implementation.

Agent harnesses remain external because the process boundary isolates authentication, configuration, model APIs, release cadence, and failure. A provider adapter communicates through the strongest machine-oriented interface the harness offers, in this order:

1. A versioned structured protocol with lifecycle and event semantics.
2. A documented JSON or JSONL non-interactive mode.
3. A foreground terminal interface for interactive use.

Coterie does not link against internal Codex or other provider crates. Such crates are implementation details of their products and would couple Coterie's release cycle to theirs more tightly than a process protocol.

External task trackers may be added later as explicit interoperability adapters. They are not required for normal operation and are not the authoritative store for a run using the built-in tracker.

## Declarative configuration

Global configuration lives at `$XDG_CONFIG_HOME/coterie/config.toml`, falling back to `~/.config/coterie/config.toml`.

```toml
schema = 1
default_archetype = "global:standard@1"

[policy]
allowed_providers = ["codex"]
allowed_project_roots = ["~/projects"]
max_concurrent_agents = 8
max_agents_per_run = 16
max_spawn_rate_per_minute = 8

[providers.codex]
kind = "codex"
command = ["codex"]

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

[archetypes.standard]
version = 1
lead = "lead"

[archetypes.standard.roles.lead]
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
  "project:attach",
  "workspace:integrate",
]

[archetypes.standard.roles.worker]
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

[archetypes.standard.roles.reviewer]
provider = "codex"
mode = "job"
max_instances = 1
workspace = "read-only"
permission_profile = "review"
capabilities = ["send:lead", "task:read", "task:comment"]
```

`max_instances` is a capacity limit, not a request to start idle agents. The lead creates agents explicitly, and the supervisor rejects spawns that exceed role or global limits. Automatic demand-based pool scaling is not part of the initial product target.

An optional project configuration contains only selection and monotone-safe overrides. A project may disable roles, reduce capacities, choose a globally defined permission profile that is no more permissive, and tighten resource limits. It cannot increase authority or resource ceilings.

```toml
archetype = "global:standard@1"

[roles.worker]
max_instances = 2
```

Configuration is resolved in this order:

1. Versioned compiled defaults and built-in archetypes.
2. Trusted global configuration and global local includes.
3. The selected global or built-in archetype.
4. Optional project restrictions.
5. Explicit operator command-line overrides, bounded by global policy.

Later values replace earlier scalar and array values. Tables merge recursively. Every effective value retains provenance identifying its source layer and file.

Coterie provides the following inspection commands:

```console
coterie config check
coterie config show --effective --provenance
coterie config schema
coterie config lock
```

Unknown fields and unsupported schema versions are errors. Includes are global-only, non-recursive, cycle-checked, and resolved relative to the including file.

Built-in archetypes use the reserved `builtin:` namespace and cannot be shadowed by global configuration. Global archetypes use `global:`. If no configuration exists, Coterie uses `builtin:standard@1`.

`coterie config lock` explicitly writes `coterie.lock`. The lock records the selected archetype reference, configuration schema, compatible Coterie version range, provider requirements, and a SHA-256 digest of the portable effective configuration. It contains no secrets, executable paths, or host-specific values. When a lock is present, a mismatch fails with an actionable diagnostic rather than silently using a different archetype.

Project configuration is untrusted. It cannot define provider executables, instructions, hooks, host paths, environment-variable passthrough, capabilities, or permission profiles. Effective project settings are intersected with trusted global policy. Commands are represented as argument arrays and executed directly; Coterie never evaluates configuration with `sh -c`.

The primary project selects the run archetype. When another project is attached, Coterie loads that project's restrictions and lock, applies them to work targeting that project, and snapshots the result. An attached project's archetype selector cannot replace the active run archetype; an incompatible selector, lock, or restriction fails attachment with an actionable diagnostic.

The run-level effective configuration and its fingerprint are snapshotted when a run starts; each additional project's effective restriction overlay is snapshotted when it is attached. The initial product target does not hot-apply configuration changes to an active run. Starting Coterie or attaching a project with a conflicting archetype or configuration reports the active snapshot and requires an explicit resolution.

## Runtime architecture

Each run has a supervisor process that is started automatically on demand. The foreground Coterie process connects to the supervisor before it launches the provider TUI.

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

The supervisor communicates through a Unix-domain socket under `$XDG_RUNTIME_DIR/coterie/`. Durable state lives under `$XDG_STATE_HOME/coterie/runs/<run-id>/`. A small local index beneath `$XDG_STATE_HOME/coterie/projects/` records which active run, if any, holds a project identity. This is disposable coordination metadata, not project registration or configuration.

For a Git project, the identity includes both the canonical Git common directory and the current worktree identity. Two linked worktrees from the same repository therefore do not accidentally share one active run. A canonical directory identity is used for non-Git projects. Symlinks are resolved, aliases must be unique within the run, and both resolved identities and original paths are stored for diagnostics.

An active run holds a nonblocking exclusive lease for every attached project identity, including its primary project. Runtime locks and a supervisor socket handshake enforce ownership; a PID file or durable index entry alone is never treated as proof of liveness. Because attachment never waits for another project lease, two runs attempting to cross-attach each other's projects fail visibly rather than deadlock. Stale sockets, leases, index entries, and interrupted attachment are repaired conservatively.

Launching Coterie from any project leased by a live run connects to that run or reports its identity before performing a conflicting action. The initial product target uses an exclusive lease; concurrent read-only attachment may be added later if it can preserve comprehensible ownership.

The supervisor is the single writer for the run database. CLI processes issue typed RPC requests rather than opening the database directly. This makes ordering explicit and keeps project attachment, dependency, claim, and assignment transactions local to one process.

The supervisor may outlive the foreground lead while workers are active. It exits when the run is stopped or after a configurable idle period with no live sessions or pending operations.

## Embedded task and state store

SQLite is the source of truth for both durable work and orchestration state. The schema includes at least:

- runs and effective configuration snapshots;
- attached projects, aliases, identities, restrictions, and leases;
- agents, sessions, and lifecycle generations;
- tasks, dependencies, comments, and task groups;
- claims and assignments;
- messages, delivery cursors, and acknowledgements;
- workspace ownership and integration metadata;
- operations and reconciliation attempts;
- the append-only typed event stream.

Provider transcripts are stored separately as append-only files with database references. This avoids large transcript blobs in ordinary state queries.

Task lifecycle initially supports `open`, `in_progress`, `submitted`, `closed`, and `canceled`. A task has exactly one attached project as its writable target and may name other attached projects as read-only inputs. Its project set cannot change while claimed or assigned. Dependencies may cross project boundaries. Blocking is derived from dependency state rather than stored as a second source of truth. A task is ready when it is open, has no unresolved blocking dependency, and has no active claim.

Claims are compare-and-set transitions performed in an immediate transaction. Claiming a task and creating its assignment are one database operation. Each initial job agent has at most one active assignment. Stable task IDs use a Coterie namespace and collision-resistant identifier rather than database row numbers.

A completed assignment moves its task to `submitted`; it does not by itself satisfy dependent tasks. Closure records that the task's acceptance condition has been verified. For a worktree assignment, this normally requires an integration record identifying the target project, base commit, result commit, and resulting target commit. The lead may close work performed directly in a target project, or non-code work, after explicit validation. An operator override is recorded as such rather than fabricated as integration evidence.

Dependencies wait for `closed`, not merely `submitted`. This gives cross-project sequences precise semantics: a bindings task remains blocked until the upstream library change has been integrated and validated in the library project. The closed task exposes a compact result record—including its project, relevant commits, summary, and test outcome—to downstream agents through `coterie prime`.

The store uses foreign keys, explicit schema migrations, bounded busy timeouts, and WAL mode where the platform supports it. Every mutating RPC accepts an operation ID so retries are idempotent.

Database transactions cannot include process or filesystem side effects. Operations that attach a project, create or integrate a worktree, or launch a provider therefore follow a durable intent pattern:

1. Record the desired operation and ownership in SQLite.
2. Commit the transaction.
3. Perform the external side effect.
4. Record the observed result.
5. Let reconciliation repair an interrupted sequence.

The initial product target provides local durability across crashes and sessions, not cross-machine task synchronization. Export, import, and external tracker adapters may be designed later without changing the internal task interface.

## Agent bootstrap and identity

Coterie injects a small provider-specific bootstrap instruction before an agent begins work. For providers such as Codex, this uses a supported developer- or system-instruction mechanism rather than modifying `AGENTS.md` or sending an ordinary first chat message.

The bootstrap establishes only orchestration behavior:

```text
You are the lead agent for Coterie run 7b2f.
Use the coterie CLI for delegation and communication.
Run `coterie prime` now for current identity, peers, tasks, and command guidance.
Follow the repository's AGENTS.md instructions for work in the project.
```

Repository instructions remain the source of project conventions. Coterie does not generate, modify, shadow, or replace `AGENTS.md`. Each agent receives the instructions that apply to its assigned project's working directory. The bootstrap must avoid conflicting work instructions, while recognizing that the provider's instruction hierarchy may place injected developer instructions above repository files.

Dynamic context is obtained through `coterie prime` so agents can recover after compaction, provider resume, or a fresh session. Every agent process receives an identity-scoped environment:

```text
COTERIE_PROJECT_ROOT
COTERIE_PROJECT_ID
COTERIE_PRIMARY_PROJECT_ROOT
COTERIE_RUN_ID
COTERIE_AGENT_ID
COTERIE_ROLE
COTERIE_TASK_ID
COTERIE_SOCKET
COTERIE_TOKEN
```

For a job agent, `COTERIE_PROJECT_ROOT` and the process working directory identify the task's target project or isolated worktree. For the lead, they initially identify the primary project. `coterie prime` always reports every attached project, its alias, the caller's access, and the target project of each visible task; agents should not infer project identity from repository names or relative paths.

The token is random, scoped to one run, agent, and session generation, and rotated when the session is replaced. The supervisor stores a verifier rather than the raw token. Known credentials are redacted from Coterie-controlled logs and transcripts.

Startup injection is a declared provider capability. If an archetype requires guaranteed bootstrap instructions and the selected provider cannot supply them, Coterie fails clearly rather than silently degrading to a normal user message.

## Agent and operator protocol

Agents coordinate through the installed Coterie binary:

```console
coterie whoami --json
coterie prime
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

This is the ordinary cross-project workflow, not a separate orchestration mode. The lead attaches the second project, creates one task for the library and another for the bindings, and makes the latter depend on the former. `--input-from` makes the accepted upstream tree available as a pinned input without granting write access to the upstream project. The supervisor launches each worker with the correct target working directory. The bindings task becomes ready only after the library task is integrated, validated, and closed.

Agent requests carry the session capability token. Supplying an agent name never changes the caller's identity or authority. Missing agent credentials do not automatically grant operator authority.

Operator requests use a distinct local operator channel established by the foreground or operator CLI. In the default same-user deployment, this distinguishes normal code paths and prevents accidental privilege confusion, but it is not a security boundary against a deliberately hostile same-UID process. The trust model is described below.

All programmatic commands provide versioned JSON output and documented exit codes. Successful JSON contains a schema version and never mixes diagnostic output into standard output. Diagnostics go to standard error. Mutating commands return their operation ID.

Ordinary communication and lifecycle control are separate planes:

```console
coterie send worker-1 "Please check the failing integration test."
coterie agent interrupt worker-1
coterie agent terminate worker-1
```

A text message can never be interpreted as a process-control command. Lifecycle operations require explicit capabilities and verified postconditions.

Messages receive stable IDs and are written durably before delivery is attempted. Inbox reads use a monotonic cursor; acknowledgement is explicit and idempotent. Delivery to a provider's live-steering interface is an optimization, not the durable acknowledgement. Agents check their inbox at startup, task boundaries, and before finishing.

`coterie finish` records the assignment outcome, summary, final session state, and result metadata in one transaction. For a completed implementation task, it also records the reported base and result commits and moves the task to `submitted`. Task closure is permitted only when the caller has `task:close` and the task's acceptance condition is met; otherwise the submitted task remains visibly awaiting integration, review, or lead action.

`coterie workspace integrate` is an explicit, capability-checked operation requested by the lead or operator. It uses the workspace backend to apply a submitted result to that task's target project and records exact before-and-after identities. It preflights the operation without changing the target and refuses dirty targets, unexpected target tips, ambiguous histories, and conflicts. Coterie does not autonomously choose an integration order or resolve conflicts.

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

Lifecycle and activity are separate. Lifecycle states include `starting`, `running`, `exited`, `lost`, and `quarantined`; semantic activity is `busy`, `idle`, or `unknown`. A running process may have unknown activity.

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

Capabilities are discovered from the installed provider version where possible, not merely asserted by configuration. An archetype is validated against them before launch.

Codex is the first provider. In the initial product target:

- the lead runs as an ordinary foreground Codex TUI owned by the foreground Coterie process;
- background workers run as bounded, non-interactive jobs using Codex's documented JSONL output;
- each background worker starts in its assigned project or worktree, independent of the lead's current directory;
- workers expose logs but are not attachable terminal sessions;
- live steering and transparent lead reattachment are unavailable unless a later structured Codex adapter can provide them reliably.

Attaching a project does not silently widen a live provider's filesystem sandbox. If the foreground provider cannot add a root safely at runtime, the current lead coordinates that project through task-scoped workers and Coterie's structured result and integration operations. An adapter may instead restart or resume the lead with the expanded project set when the provider supports that transition explicitly. `coterie prime` reports the distinction between a project attached to the run and a project directly accessible to the current provider session.

A structured Codex app-server adapter is a future capability path, not a requirement for the initial product target. The provider interface must accommodate it without making an experimental protocol part of Coterie's core contract.

Every provider adapter has a shared conformance suite covering launch, output framing, exit classification, cancellation, timeout, resume where claimed, transcript tails, malformed events, and idempotent termination. Deterministic fake providers exercise reconciliation and failure paths without invoking a model.

## Workspaces and Git

Workspace policy is declared per role:

- `project`: work directly in the task's target project directory, normally used by the lead or an explicit integrator;
- `worktree`: use an isolated Git worktree belonging to the task's target project, normally used by implementation workers;
- `read-only`: inspect the target project or worktree under an enforceable provider permission profile.

Every assignment resolves its workspace from its task's project identity, never from the supervisor's or caller's current directory. This rule keeps cross-project delegation deterministic even when two repositories have similar names or layouts.

For a Git-backed `--input-from` dependency, Coterie materializes the closed task's accepted tree, without repository administrative data, at its recorded integration commit beneath the consumer's workspace. It reports the alias, path, and commit through `coterie prime`. This snapshot is contextual input, never an integration target, so provider writes to it cannot alter the upstream project. A task that requires a live external path or a non-Git input must request it explicitly and can launch only when the permission profile and provider can enforce the requested access.

Coterie uses `git2` for its own repository and worktree operations. Git behavior is isolated behind an internal workspace trait so its safety rules can be tested independently of provider behavior.

A worker worktree is created from a recorded base commit beneath the run's state directory, partitioned by project and assignment identity. It uses a dedicated Coterie-owned reference in the target repository named by run and assignment identity. Coterie records desired ownership before creation and observed repository identity afterward.

Before cleanup, Coterie verifies all of the following:

- the path resolves beneath the expected Coterie state directory;
- the database records ownership by the current run and generation;
- the Git administrative data identifies the expected repository and worktree;
- no provider process still owns the workspace;
- the worktree is clean and its commits remain reachable or explicitly preserved;
- the work has been integrated or the operator explicitly approves removal.

Coterie never automatically destroys a dirty, unintegrated, or otherwise recoverable worktree. Failed cleanup leaves a diagnostic and a recoverable path. `coterie doctor` reports stale and inconsistent ownership but does not repair destructive cases without explicit approval.

Workers report their target project, base commit, result commit, summary, and tests to the lead. The initial product target does not implement an autonomous merge queue. The lead decides when to invoke guarded integration, validate the result, and close the task. Integration in one project never changes another project's worktree or branch.

Non-Git projects support `project` and enforceable `read-only` roles. An archetype requiring `worktree` fails clearly for a task targeting a non-Git project; Coterie does not silently weaken isolation.

## Reconciliation and supervision

The supervisor periodically and eventfully reconciles durable desired state with project leases, provider processes, assignments, and workspaces. Reconciliation is idempotent: repeating it with the same desired and observed state produces no additional side effects.

Each owned resource carries a run ID and generation. Late output or process exits from an earlier generation cannot mutate the current session. Provider config fingerprints detect drift without relying on timestamps.

The supervisor adopts a live process only when the provider can prove its identity and configuration generation. Ambiguous processes are reported as unknown rather than killed or adopted.

Restarts are bounded. Repeated failures within a configured window quarantine the session and emit a visible event. The supervisor does not spin indefinitely or consume unbounded provider quota.

Shutdown proceeds in phases:

1. Stop accepting new spawns.
2. Mark affected assignments as draining.
3. Interrupt all targeted sessions.
4. Wait for a bounded grace period.
5. Terminate survivors explicitly.
6. Reconcile task and workspace state without deleting recoverable work.
7. Release attached-project leases and retire their active-run index entries.

## Events and observability

Every state transition emits an immutable typed event in the same transaction that records the transition. Events have:

- a monotonically increasing run sequence number;
- timestamp, type, actor, and subject;
- run, project, agent, task, and operation identifiers where applicable;
- correlation and causation identifiers;
- a versioned structured payload;
- a concise human-readable summary.

Watchers resume after a sequence cursor. This supports `coterie events --follow`, status reconstruction, audit trails, and future editor integrations without coupling producers to consumers.

Provider-native event frames may be retained separately, but Coterie emits normalized lifecycle events for portable behavior. Unknown provider fields are preserved only in the raw transcript, not promoted into the stable Coterie event schema accidentally.

`coterie doctor` checks at least supervisor reachability, database migrations, configuration and lock compatibility, provider versions and capabilities, abandoned operations, stale assignments, task cycles, transcript accessibility, and worktree ownership.

## Trust and permission model

Coterie distinguishes these trust classes:

| Input or component | Trust | Rule |
| --- | --- | --- |
| Compiled defaults and global configuration | Trusted operator policy | May select provider executables and grant maximum authority. |
| Project `coterie.toml` | Untrusted declarative request | May only select trusted definitions and reduce authority or limits. |
| Operator-attached project path | Trusted host authority | Must be canonicalized and explicitly attached; it grants no authority beyond resolved global policy. |
| Repository content and task text | Untrusted data to Coterie | Never becomes a command, path authority, or policy value through interpolation. |
| Provider executable | Trusted operator-selected code | Runs only with the resolved permission profile and explicit environment. |
| Agent behavior | Untrusted within granted capabilities | Supervisor RPCs enforce identity, state transitions, and limits. |

Role capabilities and tokens protect the supervisor protocol from confused or accidental use. When agents and the operator run as the same Unix user with access to the same host, they do not form a hostile security boundary: a sufficiently capable same-UID process may inspect other processes or user-readable runtime files. Strong isolation requires a future hardened mode using separate operating-system identities, user namespaces, or containers.

The ordinary provider sandbox remains mandatory policy, not an optimization. `read-only` is advertised only when the selected provider can enforce it. Workspace isolation prevents concurrent Git changes from colliding; it does not by itself restrict filesystem or network access.

An agent with `project:attach` may attach only a canonical path beneath a trusted global `allowed_project_roots` entry. Merely mentioning a path in task text or repository content never grants access. The operator may explicitly attach a path outside those roots through the operator channel; the decision and resolved identity are recorded. Project configuration cannot extend the allowlist. Attachment authority and provider filesystem authority are checked separately.

Secrets and ambient environment variables are denied by default and passed only through trusted global configuration. Coterie redacts exact known credential values from storage it controls, stores token verifiers rather than raw tokens, and keeps runtime files private to the current user. It does not promise to sanitize a provider's independently managed transcript if the provider itself prints or stores a secret.

## Safety and reliability invariants

- Project configuration cannot introduce executable commands, grant capabilities, weaken permission profiles, or increase resource ceilings.
- Agents can instantiate only declared roles and only within role and global limits.
- Every task has exactly one writable attached target; dependencies, read-only inputs, and task groups may span projects.
- Project aliases are unique, paths are canonicalized, and each attached identity has one exclusive active-run lease.
- Cross-project dependencies are satisfied only by accepted, closed upstream tasks, not by provider exit or assignment submission.
- Task claims and assignment creation are atomic and idempotent.
- The run supervisor is the single database writer.
- Every external side effect has durable intent and a reconcilable observed result.
- Agent, session, assignment, and workspace ownership is fenced by run and generation.
- Messages are durable before live delivery is attempted.
- Provider state that cannot be established is `unknown`.
- Stop, restart, and cleanup operations have bounded timeouts and verified outcomes.
- Destructive cleanup requires positive proof of ownership and recoverability.
- No role name or delegation strategy is hardcoded into the runtime.
- Provider arguments are passed as structured arrays; untrusted text is never evaluated by a shell.

## Initial product target

The complete initial product target includes:

1. A single compiled `coterie` binary, apart from the selected agent provider.
2. Versioned built-in archetypes, trusted global archetypes, safe project restrictions, provenance, and optional lock files.
3. Primary-project discovery, attached-project identities, exclusive leases, and deterministic run state.
4. An automatically managed singleton per-run supervisor discoverable from every attached project.
5. A native SQLite task graph with project targets, cross-project dependencies, assignments, messages, operations, and typed events.
6. Codex as the first provider, with one foreground lead and bounded one-shot background workers.
7. Provider-level bootstrap injection and `coterie prime`.
8. Authenticated agent RPC with durable inboxes and explicit acknowledgements.
9. Native `git2` worktree creation, guarded integration, ownership fencing, and conservative cleanup in each target project.
10. Desired-state reconciliation, bounded shutdown, and crash-loop quarantine.
11. `status`, `logs`, `send`, `spawn`, `finish`, `stop`, `project`, `task`, `workspace`, `events`, `doctor`, and configuration inspection commands.
12. Versioned structured output, stable exit codes, and operation IDs for every agent-facing command.
13. Provider and workspace conformance tests using deterministic fakes and temporary real repositories.

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

Implementation proceeds test-first around state transitions. Unit tests cover configuration lattices, task readiness, cross-project dependency release, atomic claims, capability checks, and reconciliation plans. Integration tests use multiple temporary Git repositories and fake providers to exercise attachment conflicts, project-specific working directories, guarded integration, and crashes at every durable-intent boundary. Provider contract tests are shared by fakes and the Codex adapter.

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

These features may be considered later, but should not compromise the project-native experience or turn Coterie into a general infrastructure platform.

## Design criterion

The design succeeds when a user can open a project in Neovim, launch Coterie through sidekick.nvim, and converse naturally with one lead agent while that agent safely delegates work through the Coterie CLI. The same conversation can implement a change in one project and then update a dependent project, with each worker launched in the correct working directory and the downstream task blocked until the upstream result is integrated and verified. Configuration is reproducible and inspectable, tasks and handoffs survive process failures, parallel changes are isolated, partial operations converge to a recoverable state, and no separate orchestration workspace or companion task CLI needs to be installed or maintained.
