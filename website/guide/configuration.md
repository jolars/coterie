# Configuration and permissions

You can start with Coterie's sealed `builtin:standard@1` archetype without creating configuration files. For a custom setup, trusted operator definitions live in `$XDG_CONFIG_HOME/coterie/config.toml`, or `$HOME/.config/coterie/config.toml` when `XDG_CONFIG_HOME` is unset. A project's `coterie.toml` may select those definitions and restrict their authority or limits.

Global configuration defines providers, permission profiles, archetypes, resource limits, supervision settings, and allowed roots for agent-initiated project attachment. Project configuration cannot create a provider or grant more authority than trusted global policy allows. [Configuration reference](/reference/configuration) lists the fields and generated schemas.

The [complete global example](https://github.com/jolars/coterie/blob/main/examples/config/global.toml) defines a custom coordinator and builder archetype. Its matching [project example](https://github.com/jolars/coterie/blob/main/examples/config/project.toml) reduces concurrency and builder instances. The built-in archetype needs neither file.

## Inspect before launch

Configuration commands work without an active run or installed provider:

```console
coterie config check
coterie config show --effective --provenance
coterie config schema --target global
```

`check` validates the configuration and any existing `coterie.lock`. `show --provenance` reports where effective values came from. `config lock` explicitly writes a portable lock for a project's resolved policy; it does not start a run. Launches snapshot the resolved policy, and recovery reuses it. An incompatible change requires restoring that policy or stopping the active run before launching with new settings.

## Permission profiles

Profiles separately choose filesystem access, network access, approval behavior, and the approval reviewer. Built-in foreground and background roles use different profiles. A trusted custom profile can select Codex's automatic approval reviewer or unrestricted filesystem access, but both must be selected explicitly and pass provider capability checks. Project restrictions can narrow policy; they cannot promote themselves to unrestricted access.

Use `coterie config show --provenance` to inspect the selected policy and `coterie doctor` to check provider compatibility. The [permission notes on GitHub](https://github.com/jolars/coterie/blob/main/docs/permissions.md) explain the combinations and their security implications.

Repository content, project configuration, task text, and agent actions are untrusted. Coterie authenticates an agent's session independently of its name. A worktree isolates ordinary Git edits; it is not, by itself, a security boundary.
