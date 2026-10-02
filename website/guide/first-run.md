# Your first run

Start Coterie from a project that meets the [prerequisites](./getting-started):

```console
cd my-project
coterie
```

Ask the foreground agent to inspect a small, bounded part of your project and delegate a separate review. For example:

> Review the error handling in this project's command-line entrypoint. Create a separate task for an independent reviewer. Show me the evidence and any proposed changes before closing the work.

The agent chooses how to carry out the request within its configured permissions. Coterie records task and assignment state. A read-only review can finish without a Git commit; a writable worker contribution follows the [commit handoff and integration workflow](./tasks-and-workspaces).

## Watch the run

Use another terminal in the same project:

```console
coterie status
coterie task ready
coterie events
```

When `status` shows an agent, inspect its transcript with `coterie logs <agent-name> --tail`. `coterie prime` summarizes current work and `coterie task show <task-id> --json` returns a complete task document in pages. The [CLI reference](/reference/cli) lists the remaining inspection commands.

Task states matter: **submitted** means a worker offered a result; **closed** means an authorized coordinator accepted it. A provider process exiting is a separate observation. For code changes, inspect the submitted Git result, integrate it explicitly, validate the target, and only then close the task.

## Leave and return

Closing or interrupting the foreground Codex TUI does not stop the run or an active worker. Starting `coterie` again in the project creates a fresh foreground session and reconstructs context from durable state. Once all sessions have observed exits and no operation is pending or uncertain, the default run stops after 60 seconds of inactivity. A stopped run retains its tasks and workspaces; [recovery](./recovery) explains how to continue it.

To request a bounded stop while work remains:

```console
coterie stop
```

Stopping preserves recoverable work. It does not silently remove dirty or unintegrated worktrees.
