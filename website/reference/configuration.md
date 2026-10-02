# Configuration reference

Coterie combines compiled defaults with trusted global configuration, a selected archetype, project restrictions, and bounded operator overrides. A run snapshots the resolved policy. Use `coterie config show --effective --provenance` to inspect the result, and `coterie config check` to validate it without launching a provider.

The generated schemas are the exact field and type contracts: [global](/schemas/config-global-v1.schema.json), [project](/schemas/config-project-v1.schema.json), [lock](/schemas/config-lock-v1.schema.json), and [effective policy](/schemas/config-effective-v1.schema.json). `coterie config schema --target <name>` prints the same schema from the installed binary.

## Files and precedence

| Source | Location | Authority |
| --- | --- | --- |
| Compiled defaults | In Coterie | Select `builtin:standard@1` and its sealed roles. |
| Trusted global file | `$XDG_CONFIG_HOME/coterie/config.toml`, otherwise `$HOME/.config/coterie/config.toml` | Define providers, permission profiles, archetypes, and operator bounds. |
| Project file | `<project-root>/coterie.toml` | Select trusted definitions and reduce authority or limits. |
| Operator flags | Foreground launch and selected configuration commands | Override within trusted bounds. |
| Project lock | `<project-root>/coterie.lock` | Pin a portable resolved policy when explicitly created. |

The global file can include other trusted TOML files through `includes`. Global definitions must exist before a project can select them. Project files and repository content are untrusted. A lock is checked by `config check` and `config show`; only `config lock` writes it. Existing runs retain their saved policy rather than hot-applying file edits.

## Global file

The global file accepts these top-level keys. The generated schema supplies precise types, ranges, and nested shapes.

| Key | Purpose |
| --- | --- |
| `schema_version` | Configuration format version; currently `1`. |
| `includes` | Ordered trusted TOML files relative to the global file. |
| `archetype` | Default selected archetype reference. |
| `archetypes` | Custom versioned role definitions. |
| `providers` | Provider command arrays; a command is executed directly. |
| `permission_profiles` | Filesystem, network, approval, and reviewer policy. |
| `limits` | Agent and spawn ceilings. |
| `supervision` | Timeouts, restart bounds, and idle shutdown. |
| `allowed_project_roots` | Existing absolute parent directories for agent-initiated attachment; omitted means none. |

`[providers.<name>].command` is an argument array, for example `command = ["codex"]`. Coterie never sends configuration through a shell. An archetype sets its interactive `lead` role and a table of roles. Each role can set `provider`, `mode`, `workspace`, `permission_profile`, `instructions`, `capabilities`, and `max_instances`. `mode` is `interactive` or `job`; workspaces are `project`, `worktree`, or `read-only`. Role names and capabilities are data, not built-in role semantics.

### Limits and supervision

`[limits]` supports `max_concurrent_agents`, `max_agents_per_run`, and `max_spawns_per_minute`. `[supervision]` supports `idle_timeout_seconds`, `interrupt_grace_ms`, `job_timeout_seconds`, `max_launch_attempts`, `restart_backoff_seconds`, `restart_window_seconds`, `shutdown_timeout_ms`, and `startup_timeout_seconds`. The default idle timeout for new runs is 60 seconds after every session has an observed exit and no operation remains pending or uncertain. Setting it to `0` in trusted global configuration requires an explicit `coterie stop`.

### Permission profiles

`[permission_profiles.<name>]` has these fields:

| Field | Values |
| --- | --- |
| `filesystem` | `read-only`, `workspace-write`, `project-write`, `unrestricted` |
| `network` | `deny`, `provider-default` |
| `approvals` | `never`, `interactive` |
| `approval_reviewer` | `user` (default), `auto-review` |

Policy combinations are validated. `unrestricted` is an explicit trusted choice and cannot be combined with network denial; a read-only role needs read-only filesystem authority. The [permissions guide](/guide/configuration#permission-profiles) explains how the selected profile reaches Codex.

## Project file

The project file accepts `schema_version`, `archetype`, `[limits]`, and `[roles.<name>]`. A role restriction can set `enabled`, `max_instances`, or `permission_profile`. The selected profile must reduce or preserve the trusted authority; a project cannot define a new provider, permission profile, or archetype.

```toml
schema_version = 1

[limits]
max_concurrent_agents = 3

[roles.worker]
max_instances = 2
```

The [global](https://github.com/jolars/coterie/blob/main/examples/config/global.toml) and [project](https://github.com/jolars/coterie/blob/main/examples/config/project.toml) examples show a complete custom archetype. With the built-in archetype, no file is required.

## Inspect and lock

```console
coterie config check
coterie config show --effective --provenance
coterie config schema --target project
coterie config lock
```

Locks record a portable policy fingerprint and compatible schema/version requirements. They omit local provider command paths, credentials, project identity, and allowed roots. A mismatch fails with `invalid_configuration`; review the differences before deliberately writing a new lock. Configuration and lock changes do not alter an active run's saved policy.
