---
layout: home

hero:
  name: Coterie
  text: Coding agents, coordinated from your project.
  tagline: Launch one foreground agent, delegate work to isolated workers, and keep tasks, messages, and results in durable local state.
  actions:
    - theme: brand
      text: Get started
      link: /guide/getting-started
    - theme: alt
      text: How it works
      link: /guide/introduction
    - theme: alt
      text: View on GitHub
      link: https://github.com/jolars/coterie

features:
  - title: Start in your project
    details: Run coterie from a project directory. A local supervisor starts or reconnects without a permanent project registry.
  - title: Delegate with boundaries
    details: Configured roles determine capabilities and permissions. Writable worker tasks use isolated Git worktrees.
  - title: Keep the work
    details: Tasks, messages, transcripts, and workspaces survive foreground exits and supervisor restarts.
  - title: Review before acceptance
    details: A worker submission is separate from Git integration, validation, and task closure.
  - title: Inspect what happened
    details: Read status, events, transcripts, diagnostics, and versioned JSON from the CLI.
  - title: Stay in control
    details: Coterie preserves uncertain or unfinished work and requires explicit action for recovery and integration.
---
