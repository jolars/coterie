# Coterie

[![CI](https://github.com/jolars/coterie/actions/workflows/ci.yml/badge.svg)](https://github.com/jolars/coterie/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/coterie.svg)](https://crates.io/crates/coterie)
[![docs.rs](https://img.shields.io/docsrs/coterie)](https://docs.rs/coterie)

> [!WARNING]
> Coterie is under active development. The single-project Codex and Git
> operator loop is implemented, but the broader initial product target remains
> incomplete.

Coterie is a project-native Rust CLI for coordinating coding agents. It will
keep orchestration mechanics, durable state, workspaces, and policy enforcement
in one foreground program while agent harnesses remain out-of-process
providers.

The current platform target is Linux, developed on NixOS and tested on Ubuntu.

The current command slice can launch or reconnect to a durable local run,
open its foreground Codex TUI, inspect durable state, create and close tasks,
spawn Codex workers in isolated Git worktrees, finish assignments, exchange
durable messages, read and follow transcripts and events, diagnose runtime state
with `doctor`, explicitly integrate submitted
Git worktrees through a guarded operation, and stop the run while preserving
recoverable work. Run
`coterie --help` for the generated command reference; see the [CLI
contract](docs/cli-contract.md) for programmatic output and retry rules.

Configuration inspection supports `config check`,
`config show --effective --provenance`, `config schema`, and explicit
`config lock` creation. These commands resolve layered configuration and verify
portable locks without starting a run or probing providers. Launch and recovery
adoption of that configuration remains separate M5 work. See the
[configuration command contract](docs/cli-contract.md#coterie-config-check)
and [examples](examples/config).

## Installation

Coterie currently supports Linux. Install the latest prebuilt release with the
shell installer:

```console
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jolars/coterie/releases/latest/download/coterie-installer.sh | sh
```

Releases provide glibc and static musl archives for x86-64 and ARM64 Linux.
Each archive has a matching SHA-256 checksum and a GitHub build-provenance
attestation. To verify a downloaded archive:

```console
sha256sum --check coterie-x86_64-unknown-linux-gnu.tar.xz.sha256
gh attestation verify coterie-x86_64-unknown-linux-gnu.tar.xz --repo jolars/coterie
```

To install from crates.io with Rust 1.98.0 and Cargo instead:

```console
cargo install coterie --locked
coterie --version
```

To install the current source directly from GitHub:

```console
cargo install --git https://github.com/jolars/coterie.git --locked
```

To install a local checkout instead:

```console
git clone https://github.com/jolars/coterie.git
cargo install --path coterie --locked
```

The crates.io `0.1.0` package is the earlier development-foundation release; it
does not contain the operator loop documented below. macOS and Windows remain
outside the current platform contract.

To install the default Nix flake package:

```console
nix profile install github:jolars/coterie
```

Or run it without installing:

```console
nix run github:jolars/coterie
```

At runtime, `XDG_RUNTIME_DIR` must name an absolute, existing directory owned by
the current user with mode 0700. Coterie stores durable data beneath
`$XDG_STATE_HOME/coterie`, or `$HOME/.local/state/coterie` when
`XDG_STATE_HOME` is unset or relative.

## Codex prerequisites

Coterie launches the external `codex` program; it does not provide a model
client or authentication. Before starting Coterie:

1. [Install Codex CLI](https://learn.chatgpt.com/docs/codex/cli) and make sure
   `codex` is on the `PATH` inherited by the terminal or editor.
2. Run `codex` directly once and complete one of its offered sign-in methods.
3. Run `codex --version` and confirm that it reports `codex-cli` version
   0.151.0 or later, but earlier than 1.0.0.

At launch, Coterie probes the installed version and the documented command-line
features needed for an interactive TUI, `codex exec --json` jobs, startup
instructions, working-directory selection, sandboxing, and approvals. It fails
closed with exit code 7 when the executable, version, or required capability is
unavailable.

The MVP worker loop requires a clean, non-bare Git repository with at least one
commit because the built-in `worker` role receives an isolated Git worktree.
The foreground lead can open a non-Git directory, but spawning that role there
fails instead of weakening its isolation.

Start or reconnect to a run from the project:

```console
cd my-project
coterie
```

## sidekick.nvim

[sidekick.nvim](https://github.com/folke/sidekick.nvim) can launch Coterie as a
custom CLI tool. Add the entry beneath `opts.cli.tools` in the plugin
configuration:

```lua
{
  "folke/sidekick.nvim",
  opts = {
    cli = {
      tools = {
        coterie = {
          cmd = { "coterie" },
        },
      },
    },
  },
}
```

Restart Neovim, or reload the configuration, then run `:checkhealth sidekick`
and `:Sidekick cli show name=coterie focus=true`. Neovim must inherit a `PATH`
containing both `coterie` and the compatible `codex` executable. Sidekick's CLI
integration works independently of its optional Next Edit Suggestions feature.

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

Version `0.1.0` was published to crates.io and released on GitHub manually.
Versionary prepares and publishes later GitHub releases. Version tags trigger
separate workflows that publish the matching crate to crates.io through trusted
publishing and use cargo-dist to attach checksummed, provenance-attested Linux
binaries and a shell installer to the GitHub release.

## Project documentation

- [`DESIGN.md`](DESIGN.md) defines the intended product behavior and safety
  boundaries.
- [`TODO.md`](TODO.md) defines implementation order and milestone gates.
- [`AGENTS.md`](AGENTS.md) records the operational rules for contributors and
  coding agents.
- [`docs/cli-contract.md`](docs/cli-contract.md) defines commands, versioned
  JSON output, operation retries, authentication, recovery, trust boundaries,
  and process exit codes.
- [`docs/crash-matrix.md`](docs/crash-matrix.md) describes failure injection,
  recovery evidence, and concurrent stress testing.
- [`docs/destructive-operations.md`](docs/destructive-operations.md) maps
  destructive operations to their ownership, inactivity, and recovery guards.

## License

Coterie is available under either the [MIT license](LICENSE-MIT) or the
[Apache License 2.0](LICENSE-APACHE), at your option.
