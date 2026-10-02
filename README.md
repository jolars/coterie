# Coterie

[![CI](https://github.com/jolars/coterie/actions/workflows/ci.yml/badge.svg)](https://github.com/jolars/coterie/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/coterie.svg)](https://crates.io/crates/coterie)
[![docs.rs](https://img.shields.io/docsrs/coterie)](https://docs.rs/coterie)

Coterie coordinates coding agents from the project you are working in. One foreground Codex agent can delegate tasks to configured workers while Coterie keeps durable task state, messages, transcripts, and isolated Git workspaces. Submission, integration, and accepted task closure are separate steps.

**[Read the guide and reference at coterie.fyi](https://coterie.fyi)**

Coterie currently targets Linux. The single-project Codex and Git operator loop is implemented. Project attachment is available, while per-project overlays and the complete cross-project workflow are still in development. Releases remain in the `0.x` series.

## Install

Install the latest prebuilt Linux release:

```console
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jolars/coterie/releases/latest/download/coterie-installer.sh | sh
```

Releases include checksummed, provenance-attested glibc and musl archives for x86-64 and ARM64. With Rust 1.98.0 and Cargo, you can instead run `cargo install coterie --locked`. Nix users can run `nix profile install github:jolars/coterie` or `nix run github:jolars/coterie`.

Coterie launches the external Codex CLI. [Install Codex](https://learn.chatgpt.com/docs/codex/cli), run it once to sign in, and confirm that `codex --version` reports `codex-cli` 0.153.4 or later, below 1.0.0. The default writable worker role needs a clean, non-bare Git repository with at least one commit. `XDG_RUNTIME_DIR` must be an existing absolute directory owned by you with mode `0700`.

```console
cd my-project
coterie doctor
coterie
```

See [Getting started](website/guide/getting-started.md) for prerequisites and [Your first run](website/guide/first-run.md) for a walkthrough. The [research claim demo](website/guide/demo.md) uses a disposable fixture.

## Development

Enter the development environment and run the local gate:

```console
devenv shell
task check
```

`task docs:site` builds the public site. `pnpm docs:dev` serves it locally, and `pnpm docs:preview` previews a production build. The site source is in [`website/`](website/); its release deployment and domain setup are described in [the maintainer runbook](docs/website-deployment.md).

[`DESIGN.md`](DESIGN.md) defines the product target and safety boundaries, [`TODO.md`](TODO.md) tracks milestone gates, and [`AGENTS.md`](AGENTS.md) gives contributor instructions. Implementation evidence and incident history remain in [`docs/`](docs/). The detailed [CLI contract](docs/cli-contract.md) covers programmatic output and recovery rules.

## License

Coterie is available under either the [MIT license](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option.
