# Permission profiles

Coterie resolves permission profiles from trusted global configuration and
records them with each run. It passes the selected sandbox, approval policy,
and reviewer explicitly to Codex. Personal Codex defaults do not select a
Coterie role's policy.

## Automatic approval review

In a global profile used by a custom archetype, configure:

```toml
[permission_profiles.interactive]
filesystem = "project-write"
network = "provider-default"
approvals = "interactive"
approval_reviewer = "auto-review"
```

`approval_reviewer` defaults to `user`. With `interactive` approvals, Coterie
sets Codex's `approval_policy="on-request"` and maps the selected reviewer to
`user` or `auto_review`. Automatic review evaluates eligible approval requests;
it does not approve every operation or disable sandboxing. See the
[Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).

`approvals="never"` suppresses approval requests. It leaves the selected
sandbox in place and requires the default `user` reviewer; explicitly combining
`never` with `auto-review` is a configuration error.

Human and automatic review are distinct trusted policies. Project restrictions
may disable approvals, but cannot switch the reviewer. Built-in archetypes
retain their existing policies; defining a profile with a matching name does
not override them. Select a custom global archetype to use automatic review.

## Unrestricted operation

An unrestricted global profile uses:

```toml
[permission_profiles.unrestricted]
filesystem = "unrestricted"
network = "provider-default"
approvals = "interactive"
approval_reviewer = "auto-review"
```

This selects Codex's `danger-full-access` sandbox mode. It removes filesystem
and network sandbox boundaries, so combining it with `network="deny"` is an
error. Approval policy remains independent: use `interactive` with either
reviewer, or `never` with the default reviewer. A read-only role cannot use
this profile.

The [complete global example](../examples/config/unrestricted.toml) defines
`global:unrestricted@1`. Include it from your global configuration and select it
explicitly:

```console
coterie --archetype global:unrestricted@1
```

Alternatively, set it as the `archetype` in global configuration. A project
`coterie.toml` cannot activate an archetype with enabled unrestricted roles
unless that archetype is also selected by the global default or the CLI.
Projects may restrict an authorized unrestricted profile to a sandboxed one.

Workspace placement and supervisor authorization still apply. Unrestricted
processes can access other workspaces and same-user runtime files, however, so
these logical checks do not isolate such a process from the host.

## Inspection and upgrades

Use `coterie config show --provenance` to inspect each role's resolved policy.
`coterie doctor` checks the installed provider's capabilities. Unsupported
automatic review or unrestricted access fails before launch; Coterie does not
retry with another policy. Both foreground and background launches use these
checks.

Existing profiles default to human review, and database upgrades preserve
saved authority and portable fingerprints. Internal RPC protocol 15 carries
the reviewer in permission profiles; an older active supervisor requires its
matching executable for inspection and shutdown before using the new binary.

Changing a profile does not hot-apply it to an active or recovered run. Finish
or explicitly stop that run before starting with the new policy. Review and
regenerate a configuration lock when intentionally changing its policy.
