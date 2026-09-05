# Coterie

> [!WARNING]
> Coterie is at the design and foundation stage. The binary does not yet expose
> orchestration behavior.

Coterie is a project-native Rust CLI for coordinating coding agents. It will
keep orchestration mechanics, durable state, workspaces, and policy enforcement
in one foreground program while agent harnesses remain out-of-process
providers.

The current platform target is Linux, developed on NixOS and tested on Ubuntu.

## Development

Enter the reproducible development environment and run the complete local gate:

```console
devenv shell
task check
```

The canonical maintainer commands are:

| Command | Purpose |
| --- | --- |
| `task fmt` | Check Rust, TOML, and Nix formatting. |
| `task lint` | Run Clippy with warnings denied and validate workflows. |
| `task test` | Run tests with cargo-nextest. |
| `task docs` | Build rustdoc with warnings denied. |
| `task audit` | Check vulnerabilities, licenses, bans, and sources. |
| `task check` | Run every required local and CI gate except coverage. |
| `task coverage` | Generate an HTML coverage report without a threshold. |

Use `devenv test` to reproduce the clean-shell gate, including all configured
pre-commit hooks.

## Releases

The crate starts at `0.1.0` and is not published to crates.io. The first tag and
GitHub Release will be created manually after the MVP gate. Versionary is
configured for later release PRs, but its workflow remains dormant while the
GitHub repository variable `VERSIONARY_ENABLED` is not `true`.

## Project documentation

- [`DESIGN.md`](DESIGN.md) defines the intended product behavior and safety
  boundaries.
- [`TODO.md`](TODO.md) defines implementation order and milestone gates.
- [`AGENTS.md`](AGENTS.md) records the operational rules for contributors and
  coding agents.

## License

Coterie is available under either the [MIT license](LICENSE-MIT) or the
[Apache License 2.0](LICENSE-APACHE), at your option.
