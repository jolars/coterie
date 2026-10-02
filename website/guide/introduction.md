# What is Coterie?

Coterie runs a foreground coding agent in your project and gives it a durable way to coordinate other agents. You start it like an ordinary CLI:

```console
cd my-project
coterie
```

The foreground agent uses Codex's interactive terminal. When work can be delegated, Coterie launches configured workers, records their tasks and messages, and gives writable Git tasks separate worktrees. The supervisor keeps that state locally, so closing the terminal does not erase a worker's assignment or result.

The agent decides how to divide and judge the work. Coterie enforces role permissions, task transitions, workspace ownership, and explicit integration. A worker exiting or submitting work does not mean the result has been accepted.

## What you can use today

- Start or reconnect to a run from a project, inspect it, and stop it safely.
- Delegate tasks to Codex workers in isolated Git worktrees, exchange durable messages, and inspect transcripts.
- Review a submission, explicitly integrate its Git commits, validate the result, and close the task.
- Inspect effective configuration, diagnose a run, and recover retained runs or interrupted assignments.
- Attach another project to an active run for discovery and inspection.

The current platform target is **Linux**. Coterie requires an installed, authenticated Codex CLI for agent runs. The default writable worker flow requires a clean Git repository with at least one commit. See [Getting started](./getting-started).

Project attachment is available, but per-project overlays, task targets across projects, cross-project dependencies, and the complete two-project workflow are still under development. Coterie is in the `0.x` series, and its public interface may change before `1.0`.

## Where to go next

- [Getting started](./getting-started) covers installation and prerequisites.
- [Your first run](./first-run) shows the operator loop.
- [Core concepts](./concepts) explains runs, tasks, assignments, and workspaces.
- [CLI reference](/reference/cli) lists every public command.
