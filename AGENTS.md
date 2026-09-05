# AGENTS.md

This file is the operational repository guide for AI agents. Read
[`DESIGN.md`](DESIGN.md) before changing architecture, security boundaries,
provider behavior, public CLI contracts, configuration, persistence, or
workspace management. Follow [`TODO.md`](TODO.md) for implementation order and
milestone acceptance criteria.

## Current status

Coterie has completed the M0 development foundation. The repository contains a
behavior-free Rust binary and the canonical Task, devenv, pre-commit, CI,
documentation, and release-configuration gates. M1 in `TODO.md` owns the first
runtime contracts and durable state; do not imply that orchestration behavior
exists before its acceptance criteria pass.

## Project priorities

Coterie is a project-native Rust CLI that lets one foreground lead agent
coordinate declaratively configured workers. It owns durable orchestration,
tasks, messages, workspaces, and reconciliation while agent harnesses remain
out-of-process providers.

- Preserve the project-native experience: running `coterie` in a project is
  sufficient, and additional projects are attached only to an active run.
- Keep mechanics in Rust and judgment in agents. The binary enforces policy,
  ownership, state transitions, and transport; it does not decide how work
  should be decomposed or whether an implementation is good enough.
- Keep roles and delegation strategies in configuration. Never give names such
  as `lead`, `worker`, or `reviewer` special runtime semantics.
- Prefer durable, inspectable state over inference. Unknown provider or process
  state stays unknown.
- Preserve recoverable work. Cleanup must fail safely whenever ownership,
  inactivity, integration, or reachability cannot be proved.

The current platform target is Linux, developed on NixOS and tested on Ubuntu.
Do not imply macOS or Windows support until their process, socket, path, signal,
PTY, and sandbox behavior is implemented and tested.

## Sources of truth

- `DESIGN.md` defines product behavior, trust boundaries, invariants, initial
  scope, and non-goals.
- `TODO.md` defines implementation order and the gate for each milestone.
- Tests define implemented behavior. A checked roadmap item must have the tests
  required by its milestone.
- Generated CLI, configuration, and JSON schemas must agree with their typed
  Rust definitions; do not maintain competing handwritten contracts.

If implementation evidence invalidates a design assumption, stop and propose a
focused `DESIGN.md` change. Do not silently weaken an invariant to make a test
pass.

## Development workflow

Prefer test-driven development. Add or update the narrowest failing test first,
implement the behavior, run focused tests, and then run the checks required by
the current milestone.

M0 must establish these canonical commands:

- `task fmt`: check Rust, TOML, and Nix formatting without rewriting files.
- `task lint`: run Clippy with all targets and features and deny warnings.
- `task test`: run the test suite with `cargo nextest`.
- `task docs`: build rustdoc with warnings denied.
- `task audit`: run dependency policy and vulnerability checks.
- `task check`: run every required local and CI gate above.
- `task coverage`: generate coverage without making a percentage the sole test
  quality metric.

Keep Taskfile, devenv, pre-commit, and CI commands aligned. Run focused Cargo
tests while developing, followed by `task check` before handing off a complete
code change. Real Codex tests require an explicit opt-in and available local
authentication; they must never be required in ordinary CI.

## Architecture and dependencies

Start with one edition-2024 binary crate and internal modules matching the
components in `DESIGN.md`: `cli`, `config`, `project`, `supervisor`, `protocol`,
`providers`, `tasks`, `workspace`, `state`, and `transcript`. Extract a crate
only when a tested boundary or independent consumer justifies it.

- The supervisor is the only writer to a run database. Other processes use typed
  RPC and never open it for mutation.
- SQLite provides transactions and storage; Coterie owns task and orchestration
  semantics. Add schema changes as forward migrations and test upgrades from
  every released schema.
- Keep Git behavior behind the workspace trait. Coterie-owned code uses `git2`,
  not the Git CLI, for repository state and worktree mutations.
- Keep provider behavior behind a capability-probed adapter. Never link to a
  provider's internal crates or rely on undocumented terminal text.
- Represent commands as argument arrays and execute them directly. Never pass
  configuration, task text, repository content, or provider output through
  `sh -c`.
- Keep external side effects out of database transactions. Record durable
  intent, commit it, perform the effect, record the observation, and reconcile
  interrupted operations.
- Prefer small, focused dependencies with minimal features. Bundle narrowly
  scoped native dependencies when needed for a reproducible CLI, and document
  every exception to the single-binary distribution goal.

Use `thiserror`-style typed errors in domain and adapter code; add context and
map them to stable diagnostics at the CLI boundary. Use structured tracing and
never log capability tokens, credentials, or complete inherited environments.
Keep `unsafe` code isolated, justified by an OS boundary, and covered by a safe
abstraction and tests.

## Protocol and safety rules

- Treat compiled defaults and trusted global configuration as operator policy;
  treat project configuration, repository content, task text, and agent actions
  as untrusted.
- Every task has exactly one writable attached target. Read-only inputs and
  dependencies are explicit and may span projects only after attachment.
- Authenticate the caller independently of any agent name in a request. Tokens
  are random, scoped to a run, agent, and session generation, and stored only as
  verifiers.
- Every mutating RPC is idempotent under its operation ID. Task claim and
  assignment creation are one compare-and-set transaction.
- Fence late process output and exits by generation. Do not infer liveness from
  a PID file, semantic idleness from resource usage, or success from provider
  exit alone.
- Messages are durable before live delivery. Process control uses separate,
  capability-checked RPCs and is never encoded as message text.
- Refuse dirty, moved, ambiguous, or conflicting integration targets. Never
  delete a dirty or unintegrated worktree automatically.
- Coterie injects only orchestration bootstrap instructions. It does not create,
  modify, replace, or shadow a target repository's `AGENTS.md`.

## Testing expectations

- Unit-test pure state transitions, configuration lattices, authorization,
  readiness, reconciliation plans, framing, and error mapping.
- Use temporary real Git repositories for identity, worktree, reference,
  integration, symlink, dirty-tree, and cleanup tests. Do not mock the behavior
  that establishes safety.
- Use deterministic fake providers for lifecycle, malformed output, timeouts,
  cancellation, restart, transcript, and crash-boundary tests.
- Share provider conformance tests between fakes and Codex for every capability
  the adapter claims.
- Test human output separately from versioned JSON. JSON success output goes to
  standard output; diagnostics go to standard error; exit codes remain stable.
- Add failure injection around every durable-intent boundary. Repeating
  reconciliation with unchanged desired and observed state must produce no new
  side effects.

## Change synchronization

- Update `DESIGN.md` when product behavior, trust assumptions, invariants,
  initial scope, or non-goals change.
- Update `TODO.md` when a milestone is completed, split, reordered, or blocked.
  Never check an item merely because code exists; its tests and gate must pass.
- Update CLI help, exit-code documentation, JSON schemas, configuration schemas,
  and examples in the same change as their typed interfaces.
- Add and test a migration in the same change as any persistent schema update.
- Record provider-version assumptions in capability probes and contract tests,
  not only in prose.

Use Conventional Commits because Versionary derives releases from them. Keep
commit titles short, wrap code identifiers in backticks, use ASCII in commit
messages, and include `Fixes`, `Closes`, or `Refs` when a change corresponds to
an issue. Create a branch when a change spans multiple project areas, alters a
public interface or schema, or requires a migration; otherwise work on the
default branch.
