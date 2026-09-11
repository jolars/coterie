# Coterie roadmap

This roadmap is ordered by dependency and recommended implementation sequence.
[`DESIGN.md`](DESIGN.md) is the architectural authority; this file divides that
design into acceptance-gated milestones. Check off a deliverable only when its
tests and the milestone gate pass.

The first public milestone is a deliberately narrow, single-project MVP. The
later milestones complete the broader initial product target in `DESIGN.md`.

## M0: Development foundation

The repository now contains the Rust package and project-specific checks that
later milestones build on.

- [x] Replace the generic `devenv.nix` template with a Rust environment modeled
  on `../basin`: enable the pinned toolchain from `rust-toolchain.toml`, and
  install Git, Go Task, SQLite, `cargo-nextest`, `cargo-llvm-cov`,
  `cargo-audit`, `cargo-deny`, `nixfmt`, `taplo`, and `actionlint`.
- [x] Scaffold one edition-2024 binary crate named `coterie`. Start at version
  `0.1.0`, keep the runtime in internal modules, and commit `Cargo.lock`.
- [x] Pin Rust 1.98.0 with rustfmt and Clippy. Do not declare an independent
  MSRV before release hardening establishes one from evidence.
- [x] Add a `Taskfile.yml` with `fmt`, `lint`, `test`, `docs`, `audit`, `check`,
  and `coverage` tasks. `check` must reproduce every required local and CI
  gate except coverage.
- [x] Enable pre-commit rustfmt, Clippy, TOML formatting, and Nix formatting
  through devenv. Hooks must call the same underlying commands as
  `task check`.
- [x] Add CI for formatting, Clippy with warnings denied, nextest, rustdoc with
  warnings denied, dependency policy, and Nix evaluation. Cache build
  artifacts without making the cache part of correctness.
- [x] Add the README, dual MIT/Apache-2.0 license files, `CHANGELOG.md`, package
  metadata, `deny.toml`, formatting configuration, and dependency-update
  policy.
- [x] Configure Versionary's Rust release-PR workflow with Conventional Commits,
  pre-major feature bumps, stable-major releases disabled, commit authors,
  and best-effort issue references. Release `v0.1.0` manually, then enable
  Versionary and trusted crates.io publishing for later releases.
- [x] Add an operational `AGENTS.md` that records project invariants, the
  test-first workflow, verification commands, and documentation
  synchronization rules.

### M0 gate

- [x] A fresh `devenv shell` can run `task check` successfully.
- [x] The pre-commit hooks pass on the scaffold without rewriting tracked files.
- [x] CI passes from a clean checkout, and Versionary configuration verifies
  without creating a tag or publishing a package.

## M1: Core contracts and durable state

- [x] Create the internal `cli`, `config`, `project`, `supervisor`, `protocol`,
  `providers`, `tasks`, `workspace`, `state`, and `transcript` modules. Keep
  one deployable binary; extract a crate only after a concrete boundary
  requires it.
- [x] Define stable, type-specific IDs using a short Coterie prefix and a ULID,
  including run, project, agent, session, task, assignment, message,
  operation, and event identities.
- [x] Define versioned JSON success envelopes, machine-readable error bodies,
  documented exit-code categories, and operation IDs for every mutating
  command. Standard output must never mix JSON and diagnostics.
- [x] Model the versioned `builtin:standard@1` archetype and its roles,
  capabilities, permission profiles, workspace policies, and limits as data.
  Defer external configuration layers without hardcoding role semantics.
- [x] Add append-only SQLite migrations and transactional repositories for runs,
  configuration snapshots, projects, agents, sessions, tasks, dependencies,
  task groups, comments, claims, assignments, messages, workspaces,
  operations, and events. Store provider transcripts outside the database.
- [x] Enforce foreign keys, bounded busy timeouts, single-writer access, WAL
  where supported, compare-and-set claims, and idempotent mutations.
- [x] Implement task readiness and the `open`, `in_progress`, `submitted`,
  `closed`, and `canceled` lifecycle. Dependencies are satisfied only by
  closed tasks; blocking remains derived state.

### M1 gate

- [x] Tests cover every task transition, dependency release, atomic claim,
  retry, authorization decision, migration, invalid ID, and corrupt-state
  error.
- [x] Golden tests lock the versioned JSON and exit-code contracts before other
  processes depend on them.

## M2: Supervised runtime with deterministic fakes

- [x] Discover the canonical Git project or non-Git directory, derive its
  identity, and place private runtime and durable data beneath the
  appropriate XDG directories.
- [x] Implement a nonblocking exclusive project lease, active-run index, Unix
  socket handshake, automatic singleton supervisor startup, and typed local
  RPC.
- [x] Authenticate agents with random, generation-scoped tokens whose stored
  representation is a verifier; keep the operator path distinct.
- [x] Implement a deterministic fake provider and use it to drive agent and
  session lifecycles without model access.
- [x] Implement the minimum delegation commands: foreground launch, `status`,
  `whoami`, `prime`, `task create`, `task ready`, `task close`, `spawn`,
  `finish`, `send`, `inbox`, `logs`, `events`, and `stop`.
- [x] Persist messages before delivery, use monotonic inbox cursors and explicit
  acknowledgements, and normalize every state transition into the event
  stream.
- [x] Record durable intent before launching a process or changing a workspace;
  reconciliation must distinguish desired, observed, lost, and unknown
  state.

### M2 gate

- [x] An integration test launches a fake lead and worker, delegates and closes
  a task, disconnects the foreground, and reconnects to the same run.
- [x] Restart tests preserve the task graph, transcript references, operations,
  and workspace metadata. An unverifiable live process is classified as
  `lost` or `unknown`, never silently adopted or declared successful.

## M3: Codex and Git vertical slice---v0.1.0 MVP

- [x] Probe the installed Codex version and required capabilities before launch;
  reject incompatible versions with an actionable diagnostic.
- [x] Launch the foreground Codex TUI with inherited terminal streams, working
  directory, resize behavior, and signal forwarding. Inject only Coterie's
  orchestration bootstrap through Codex's documented
  `developer_instructions` setting, leaving repository `AGENTS.md` discovery
  intact.
- [x] Launch workers through `codex exec --json`, parse its JSONL event stream,
  classify exits and malformed frames, store append-only transcripts, and
  pass only the identity-scoped environment.
- [x] Map the built-in permission profiles to enforceable Codex flags. Fail
  closed when a requested filesystem, network, approval, bootstrap, or
  working directory capability cannot be enforced.
- [x] Implement the workspace trait with `git2`: record a base commit, create a
  task-owned worktree and reference below Coterie's state directory, and
  record the resulting commit without invoking the Git CLI.
- [x] Implement explicit guarded integration. Refuse dirty targets, unexpected
  tips, ambiguous histories, and conflicts; never remove dirty,
  unintegrated, running, or ambiguously owned work.
- [x] Complete the operator loop for task creation, worker spawn, logs and
  messages, assignment submission, integration, validation, task closure,
  and safe stop.
- [x] Document installation, Codex prerequisites, the Sidekick custom-command
  entry, the trust model, recovery behavior, and every MVP command and exit
  code.
- [x] Enable crates.io publication with trusted publishing and attach
  checksummed, provenance-attested `x86_64` and `aarch64` binaries for glibc
  and musl Linux to the Versionary-created GitHub release.

### v0.1.0 MVP gate

- [x] From a clean Git repository, `coterie` starts or reconnects to its
  supervisor and opens the foreground Codex lead without unsolicited wrapper
  output corrupting the TUI.
- [x] The lead can create one task, spawn one Codex worker in an isolated
  worktree, receive its durable result, inspect its transcript, integrate it
  explicitly, validate it, and close the task.
- [x] Closing the foreground leaves the run and active worker intact. A later
  invocation reconstructs orchestration context even when transparent Codex
  session reattachment is unavailable.
- [x] Interrupting the foreground reaches Codex but does not stop the run;
  `coterie stop` performs bounded shutdown and preserves recoverable work.
- [x] Unit, fake-provider, temporary-repository, and opt-in real-Codex contract
  tests pass. CI never requires networked or account-authenticated Codex
  runs.
- [ ] Merge the first Versionary release PR only after every MVP criterion is
  satisfied; publish that release as `v0.2.0`.

The MVP intentionally excludes attached projects, cross-project dependencies,
external configuration layers, provenance and lock files, live steering,
transparent provider reattachment, and exhaustive crash-boundary coverage.

## M4: Reliability and safety hardening

- [x] Reconcile every durable operation and owned resource idempotently after
  crashes between intent, external side effect, and observed-result
  recording.
- [x] Fence sessions, assignments, workspaces, and late provider output by run
  and generation; adopt a live process only when it proves both.
- [x] Add bounded restart windows, crash-loop quarantine, timeout handling, and
  the full phased shutdown protocol.
- [x] Complete `doctor`, conservative stale-state repair, resumable event
  following, transcript-tail handling, credential redaction, and private
  runtime permission checks.
- [x] Inject failures at every database/process/filesystem boundary and verify
  convergence to a recoverable state without duplicated side effects. See the
  [crash matrix](docs/crash-matrix.md) for boundaries and recovery evidence.

### M4 gate

- [x] The crash matrix, restart tests, cleanup safety tests, and fake-provider
  conformance suite pass repeatedly under concurrency.
- [x] No destructive path runs without positive proof of ownership, inactivity,
  and recoverability. See the [safety audit](docs/destructive-operations.md) for
  the operation inventory, guards, and regression evidence.

## M5: Full declarative configuration

- [x] Implement the internal loader and resolver for versioned compiled
  defaults, trusted global configuration and local includes, a selected
  built-in or global archetype, safe project restrictions, and bounded
  operator overrides in the specified precedence order.
- [x] Track provenance for every effective value; reject unknown fields,
  unsupported schemas, recursive includes, cycles, shadowed `builtin:`
  names, and references to missing trusted definitions.
- [x] Implement monotone policy intersection so project data can disable roles
  or reduce authority and capacity but cannot introduce commands,
  instructions, hooks, paths, environment variables, capabilities, or
  permission profiles.
- [x] Implement `config check`, effective configuration with provenance, schema
  generation, explicit lock creation, and lock verification without secrets
  or host-specific values.
- [x] Wire resolved configuration into launches and recovery, snapshot the
  effective run configuration, and reject incompatible changes rather than
  hot-applying them. The [runtime tests](tests/supervisor_runtime.rs) cover
  configured launches, authority, limits, overrides, and recovery. Migration 11
  pins historical policy for existing runs; upgrade and crash tests verify
  durable snapshots. `task check` passes.

### M5 gate

- [x] Lattice and property tests prove that an untrusted project override can
  never increase authority or a resource ceiling. See the
  [policy tests](src/config/policy_tests.rs) for intersection laws, combined
  restrictions, trusted selection, operator bounds, and injection rejection.
- [x] Golden tests cover schemas, provenance, configuration fingerprints, lock
  portability, includes, and actionable mismatch diagnostics. See the
  [lock tests](src/config/lock/tests.rs) and
  [configuration CLI tests](tests/config_cli.rs), alongside the loader and
  provenance golden tests. `task check` passes.

## Follow-ups from the Diplodocus delegation run

Observed on September 11, 2026, in run
`cr-01M27JEZ698AG5Y4EX1AH230VM`, with two implementation workers and one
reviewer. These are follow-ups to the M3-M5 workflow. Specify any new commands,
capabilities, or recovery transitions in `DESIGN.md` before implementation.

- [x] **Reject incomplete worktree submissions.** A worker called
  `finish --status completed` with uncommitted changes, so the recorded result
  was the unchanged base commit `27ae246`. Reject staged, unstaged, and
  non-ignored untracked changes before recording a completed Git result; keep
  the assignment active and identify the paths that need attention. Explain
  the validate, commit, finish sequence in bootstrap guidance and CLI help.
  Test each dirty state, commit-hook failure, and legitimate clean submissions
  with no new commit, including review and non-code assignments.
  Implemented with bounded, escaped path diagnostics, unreadable-path checks,
  and hidden-index guards. The [submission tests](tests/supervisor_runtime/finish.rs)
  cover rejected retries, successful replay, failing commit hooks, and clean
  non-code results. `NEXTEST_TEST_THREADS=1 task check` passes.
- [ ] **Recover an incorrect submitted result.** The worker subsequently
  committed `320ea60`, but another `finish` failed because it had no active
  assignment. Integration then refused the mismatch between the worktree tip
  and the recorded result. Provide an authorized, explicit recovery path to
  reopen or supersede an unintegrated submission while preserving its original
  record and commits. Test retries, crashes, concurrent integration, stale
  sessions, and refusal to replace an already integrated result. Error messages
  should name the supported next action.
- [ ] **Record validated work integrated outside Coterie.** The lead
  cherry-picked the reviewed implementation as `66117ed`, but task closure
  remained blocked because no Coterie integration record existed. Expose the
  documented operator closure override with the actual result and target
  commits, validation evidence, and reason. Record it as an override rather
  than fabricated integration evidence. Test externally cherry-picked work,
  authorization, retries, and dependency release; preserve the worktree and
  existing dirty-target and unexpected-tip guards.
- [ ] **Make worker bootstrap tools available in the actual shell.** Both
  workers initially failed to find `coterie` and `rg`; their login shells lost
  the user/devenv tool paths. Investigate the required user-identity and
  toolchain inputs, provide a reliable bootstrap CLI location, and test the
  provider's actual login and non-login shell behavior on NixOS. Preserve the
  explicit environment allowlist and credential redaction. Diagnose bootstrap
  command or supervisor-socket access failures under the selected permission
  profile without silently widening permissions.
- [ ] **Make command guidance reflect capabilities and assignment state.**
  `prime` omitted `spawn` for an agent allowed to spawn workers and reviewers,
  yet advertised `finish` without an active assignment. Generate actionable
  guidance from effective capabilities and current state, including available
  roles and required identifiers. A denied operator-only `status` request
  should point agents to an authorized inspection command. Test custom roles,
  restricted capabilities, and active versus submitted assignments without
  adding runtime semantics for built-in role names.
- [x] **Provide compact progress updates for authorized agents.** The lead
  repeatedly polled `prime` and `inbox` to discover task submissions and worker
  exits; `prime` repeated complete task descriptions and results. Full `status`
  and `events` are operator-only. Add a scoped, bounded inspection or wait
  interface with resumable cursors for visible tasks, assignments, and peers.
  Distinguish task submission from provider exit, and report changed state
  without replaying every task body. Test reconnects, timeouts, multiple
  completions, and authorization without requiring provider live steering.
  Implemented as `coterie progress` with caller-scoped cursors, bounded lifecycle
  pages, and optional waits. The [progress tests](tests/supervisor_runtime/progress.rs)
  cover authorization, reconnects, supervisor restart, and concurrent mutations
  during waits; unit tests cover paging, session renewal, and legacy payloads.
  `NEXTEST_TEST_THREADS=1 task check` passes.
- [ ] **Add concise transcript inspection.** Inspecting recent worker progress
  required paging through large, JSON-escaped transcripts from byte zero and
  extracting relevant records locally. Offer bounded tail or structured-event
  views for an authorized agent/session while retaining the raw transcript
  and byte-cursor interface. Preserve redaction and explicit incomplete-frame
  handling. Test UTF-8 boundaries, oversized records, partial final JSONL
  frames, session selection, and recovery after a reader disconnects.

## Follow-ups from the submission and progress run

Observed on September 11, 2026, in run
`cr-01M27MHPXV53BPGWT9CC6GSV3P`. Specify changes to commands, workspace
ownership, or recovery transitions in `DESIGN.md` before implementation.

- [ ] **Allow repeated read-only review assignments.** After the first review
  finished, another `spawn reviewer` failed with
  `UNIQUE constraint failed: workspaces.run_id, workspaces.path` because both
  assignments used the primary project path. Model repeated use of project
  and read-only workspaces without deleting historical assignment records or
  weakening isolated-worktree ownership. Test sequential reviews in one run,
  configured custom roles, retries, recovery, and concurrent assignments where
  policy permits them; retain exclusive writable workspace guards.
- [ ] **Recover work from exited agents before submission.** The progress
  worker exited with uncommitted implementation and review fixes, leaving its
  task `in_progress`. A continuation copied the preserved candidate into a new
  worktree and completed, while the original assignment remained active in
  durable state. Provide an authorized, explicit path to retire or supersede
  the interrupted assignment and link its continuation, preserving its
  workspace, edits, commits, and history. Require verified process inactivity
  before transferring writable ownership. Test exits and timeouts before
  commit or finish, stale sessions and late output, retries, crashes during
  recovery, continuation integration, and dependency release only after
  accepted closure. Diagnostics should name the supported next action.
- [ ] **Make crash-matrix tests reliable under parallel execution.** Parallel
  validation produced a shutdown trace mismatch and timeouts in the runtime
  and attached-run publication/retirement matrices. Those cases passed
  serially, and the final gate required `NEXTEST_TEST_THREADS=1`. Investigate
  wall-clock and scheduling dependencies in the
  [crash tests](src/supervisor/crash_tests.rs), using controlled time and
  explicit synchronization where needed. Retain every crash boundary and
  repeated-recovery assertion. Verify repeated default parallel `task check`
  runs under load; serial execution alone does not satisfy this follow-up.

## M6: Cross-project orchestration

- [x] Attach canonical project roots under unique aliases, enforce global root
  policy and per-project leases, and discover the same active run from every
  attached project. The [runtime tests](tests/supervisor_runtime.rs) cover
  authorization, aliases, symlinks, linked worktrees, discovery, and lease races.
  The [crash tests](src/supervisor/crash_tests.rs) cover attachment and retirement;
  migration 12 pins historical root policy. `task check` passes.
- [ ] Apply and snapshot each attached project's restrictions and lock without
  allowing its archetype selector to replace the run archetype.
- [ ] Give every task exactly one writable target and explicit read-only input
  projects. Resolve every assignment workspace from the task's project
  identity.
- [ ] Support cross-project dependencies and materialize an accepted upstream
  Git tree at its recorded integration commit as a non-writable input
  snapshot.
- [ ] Make attachment and workspace operations durable, idempotent, and
  deadlock-free; interrupted or conflicting attachment must remain visible.

### M6 gate

- [ ] In two temporary repositories, a library worker runs in the library
  worktree, a bindings worker runs in the bindings worktree, and the
  bindings task remains blocked until the library task is integrated,
  verified, and closed.
- [ ] Attachment races, alias collisions, symlinks, linked Git worktrees,
  incompatible restrictions, dirty targets, and cross-run lease conflicts
  have deterministic tests and diagnostics.

## M7: Complete the initial product target

- [ ] Audit every item under `DESIGN.md`'s initial product target and every
  stated safety invariant; add any missing command, state transition, or
  test.
- [ ] Run shared provider conformance tests against the fake and Codex adapters,
  and shared workspace tests against temporary real repositories.
- [ ] Finish CLI help, JSON schema and exit-code documentation, configuration
  references, recovery guidance, threat-model documentation, shell
  completions, and a Sidekick smoke test.
- [ ] Validate installation from crates.io and the Linux release artifact in a
  clean environment, including database and configuration upgrades from
  every published `0.x` release.
- [ ] Define an evidence-based MSRV and supported Linux baseline. Keep releases
  in `0.x` until a separate compatibility review establishes the `1.0`
  contract.

### M7 gate

- [ ] The full design criterion succeeds end to end, including the two-project
  workflow, crash recovery, conservative cleanup, and configuration
  inspection.
- [ ] Every non-goal remains absent or explicitly proposed as a later design
  change rather than entering the implementation accidentally.

## Future work to scope

- [ ] Design exclusive resource reservations for performance measurements:
  allow parallel implementation while serializing benchmark windows across
  Coterie runs on the same machine. Agents request and release reservations;
  Rust enforces admission after competing workers acknowledge safe stopping
  points and their builds, tests, and other competing subprocesses have
  finished. Prevent competing work from starting until release, and make
  reservation ownership, timeouts, and crash recovery durable and inspectable.
  Define enforcement and subprocess tracking in `DESIGN.md` before
  implementation, with tests for concurrent requests, interrupted acquisition,
  and recovery. Scope the guarantee to Coterie-managed workloads; unrelated
  host processes require separate handling.
