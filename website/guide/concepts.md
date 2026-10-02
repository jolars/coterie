# Core concepts

A **project** is the canonical directory or Git worktree from which Coterie runs. One **run** begins in a primary project. Additional projects can be attached to that active run under aliases, but attachment is not a permanent registration.

An **archetype** is a versioned, declarative set of roles, provider bindings, permission profiles, workspace policies, and limits. The default `builtin:standard@1` archetype includes a foreground `lead`, a writable `worker`, and a read-only `reviewer`. These names are configuration data, not special runtime roles. See [Configuration and permissions](./configuration).

An **agent** is one instance of a role. A **session** is one provider execution for that agent. Sessions can end or be replaced while durable work remains. Coterie authenticates agent actions through a scoped session token, not an agent name.

A **task** is a durable unit of work with a lifecycle: `open`, `in_progress`, `submitted`, `closed`, or `canceled`. A task can depend on other tasks; its dependencies are satisfied only when they are closed. An **assignment** links a task to an agent and a workspace. Task submission, Git integration, and accepted closure are separate actions.

A writable Git **workspace** is an isolated worktree with a recorded base and an owned reference. Coterie preserves uncertain, dirty, or unintegrated work instead of cleaning it up automatically. See [Tasks and workspaces](./tasks-and-workspaces).

**Messages** are durable before delivery. **Events** record run changes in sequence, and **transcripts** retain provider output. `status`, `prime`, `events`, `logs`, and `doctor` let you inspect different parts of this state.

## Current cross-project limit

`project attach` and `project list` work for an active run. The complete multi-project task workflow remains unfinished: attached project overlays, writable task targets across projects, and cross-project dependencies are not yet available. Use one primary Git project for the documented worker loop.
