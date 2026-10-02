# Getting started

This guide installs Coterie and starts it in a Git project. Coterie currently targets Linux on x86-64 and ARM64, with glibc and musl release archives.

## Install Coterie

Install the latest prebuilt release with its shell installer:

```console
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jolars/coterie/releases/latest/download/coterie-installer.sh | sh
coterie --version
```

Release archives have SHA-256 checksums and GitHub build-provenance attestations. The [GitHub releases](https://github.com/jolars/coterie/releases) page provides the archives and their matching files.

If you have Rust 1.98.0 and Cargo, you can install from crates.io instead:

```console
cargo install coterie --locked
```

Nix users can install or run the flake package:

```console
nix profile install github:jolars/coterie
# or: nix run github:jolars/coterie
```

## Prepare Codex

Coterie launches the external `codex` program. [Install Codex CLI](https://learn.chatgpt.com/docs/codex/cli), run it once to sign in, and check its version:

```console
codex --version
```

Coterie requires `codex-cli` **0.153.4 or later, but earlier than 1.0.0**. It probes the installed version and required CLI capabilities at launch. An incompatible or unavailable provider produces an actionable diagnostic; `coterie doctor` can inspect the provider without starting a model session.

## Prepare your project

For the default writable worker role, use a clean, non-bare Git repository with at least one commit. Coterie can open a non-Git directory for its foreground session, but that worker role cannot start there.

Your `XDG_RUNTIME_DIR` must be an existing absolute directory owned by you with mode `0700`. Coterie stores durable run data under `$XDG_STATE_HOME/coterie`, or `$HOME/.local/state/coterie` when `XDG_STATE_HOME` is unset or relative.

From the project root, run:

```console
coterie doctor
coterie
```

The second command opens the foreground Codex TUI. It creates or reconnects to a local run for this project. Coterie injects only its orchestration instructions; it does not replace your project's `AGENTS.md`.

Continue with [Your first run](./first-run), or try the [research claim demo](./demo) in a disposable repository.
