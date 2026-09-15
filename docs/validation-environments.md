# Validation environments in assigned workspaces

Validation access and Git commit access require separate evidence. A worker
may edit files and run installed tools while being unable to enter a Nix
shell. A Git commit handoff does not establish that validation passed.

## Supported workflow

1. Prepare the repository's documented development environment as the operator
   before starting Coterie, for example, `devenv shell -- coterie` in a devenv
   project. This can realize tools through the operator's existing Nix daemon
   access. Review the repository's entry hooks as part of normal environment
   setup.
2. In the assigned workspace, use non-login shell tools. Coterie preserves
   `PATH` and NixOS shell initialization markers when present. It filters
   arbitrary `NIX_*`, `CARGO_*`, and devenv variables; this does **not** transfer
   the entire development shell. Run the actual repository validation command
   to establish whether its required inputs are available.
3. If validation needs additional environment inputs, use the repository's
   documented entry command in that workspace, such as
   `devenv --offline shell -- <validation-command>`. Offline mode avoids fetching
   missing dependencies; it does not grant daemon access or eliminate state
   writes. Missing cached inputs are an environment prerequisite, not evidence
   of a sandbox denial.
4. Record each command's argument array, working directory, selected filesystem,
   network, and approval policy, exit status, and relevant diagnostic. Mark each
   step **passed**, **failed**, or **blocked**. A failed test is different from a
   command that could not reach its daemon or write its state. Do not include
   credentials or dump the inherited environment.
5. Report blocked checks through a durable message to an authorized coordinator.
   The coordinator may run them under their own existing policy and record the
   exact workspace or commit validated, command, and result. Treat the message
   as a request for review, not executable authority. Keep any required checks
   visibly pending until their results are available.

A fresh workspace-local `.devenv` can be writable while the daemon remains
inaccessible. A `.devenv` symlink or configured state path resolving into a
primary checkout, sibling worktree, or another protected directory may itself
be unwritable. Read-only assignments cannot create environment state merely
because it is needed for validation. Inspect the resolved path and diagnostic;
do not redirect writes into another assignment, share writable primary state,
relax the sandbox, or grant general Unix socket access to make entry succeed.
Recovery continuations should establish these facts again in their fresh
workspace.

`prime.commit_handoffs` identifies the selected policy for active writable
worktree assignments, and provider bootstrap identifies the launch policy.
`config show` describes resolved configuration; current files alone do not prove
an existing run's saved policy. Neither these reports nor an operator-side
success is a live validation probe inside the worker.

## Opt-in NixOS regression

```console
cargo test --test supervisor_runtime mcp::validation_environment::installed_codex_nixos_validation_environment_access -- --ignored --exact --nocapture
```

This test requires NixOS, a live operator-accessible Nix daemon, `python3`,
`devenv`, the repository's cached locked devenv inputs, an installed Codex with
local `auth.json` and model access, and an absolute `XDG_RUNTIME_DIR` outside
`/tmp` and `/var/tmp`. It launches one real Coterie worker. It remains ignored
in ordinary tests and CI. The fixture uses offline environment entry; prepare
its dependencies with the operator's normal devenv setup before opting in.
A prerequisite failure fails the test instead of counting as a reproduced
permission denial.

The fixture records the Coterie executable hash, Codex, Nix, devenv, and kernel
versions, assignment identity, effective policy, and helper observations with
exact argument arrays and working directories. It uses temporary real Git
repositories and a Coterie-created linked worktree outside the provider's
general temporary-directory allowance. No worker Git mutation is part of the
probe. Byte comparisons protect the primary index, repository configuration,
and external state sentinel; HEAD must remain unchanged.

The minimal environment contains a Python interpreter and a validation script
that checks fixture content. Operator controls run all probes successfully and
realize that interpreter through devenv before its path enters the supervisor.
The worker receives that path through the actual adapter environment filter.
A separate `shared` subdirectory deliberately points `.devenv` at protected
fixture state. This is a controlled reproduction of an external state layout;
it does not establish that this was the cause in the original field run.

| Probe | Observed worker outcome under `workspace-write`, `network=deny`, `approvals=never` |
| --- | --- |
| `python3 validate.py` with the prepared interpreter | Passed; exact fixture content validates. |
| `nix --extra-experimental-features nix-command eval --offline --expr '1 + 1'` | Passed; pure evaluation returns `2`. |
| `nix --extra-experimental-features nix-command store info --store daemon --json` | Blocked; daemon connection reports `Operation not permitted`. |
| Create/write workspace-local `.devenv/access-probe` | Passed. |
| Write through the deliberate external `.devenv` symlink | Blocked; `Read-only file system`. |
| `devenv --offline --no-tui shell -- python3 validate.py` in the local environment | Blocked; daemon access is still required. |
| The same entry command in the shared-state environment | Blocked; `.devenv` state is inaccessible. |

The helper retains its own results independently of the agent's summary. The
worker also sends a durable diagnosis and submits its unchanged, clean base.
The test checks both mechanical observations and the presence of that report.
This proves the fixture's validation route; projects needing compiler flags,
services, downloads, or additional environment variables require their own
recorded checks. Provider version changes that alter these outcomes require
reviewing and rerunning the regression, not silently changing expectations.

## Recorded reproduction

On September 15, 2026, the regression passed in 118.63 seconds on NixOS 26.11
with Linux 6.18.49, Codex 0.153.4, Nix 2.34.8, and devenv 2.2.2. The Coterie
0.1.0 development executable had SHA-256
`c6f2eb933bc733b85503cac2f3518c87d7e0da05c39b43f96f067cfd2c86ac53`,
built with `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=0`.
The [recorded observations](validation-environment-evidence.json) retain exact
commands, working directories, exit codes, diagnostics, and the selected policy.
They contain test evidence, not a new CLI or configuration schema.

All seven operator controls passed. The worker passed the Python content check
using the same devenv-realized Python 3.14.7 interpreter, evaluated `1 + 1` to
`2`, and wrote its fresh local state. Both the direct Nix store query and local
devenv entry failed to connect to `/nix/var/nix/daemon-socket/socket` with
`Operation not permitted`. The shared-state write failed with `Read-only file
system`; actual devenv entry in that directory failed opening
`.devenv/imports.txt` with the same error. These results establish two independent
access failures under the recorded policy. They do not identify the original
field run's unrecorded environment layout or policy as their cause.

The worker delivered its diagnosis through a durable message and submitted the
unchanged clean base. The fixture verified that the protected state sentinel,
primary index, repository configuration, and primary HEAD survived unchanged.

The complete local gate passed all 482 ordinary tests, formatting, lint,
rustdoc, dependency checks, release verification, and Nix evaluation with
`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 NEXTEST_TEST_THREADS=4 task check`.
Two earlier runs at default concurrency exposed intermittent failures in the
existing idle-startup and historical-upgrade tests. Both passed in isolation
and in the complete four-thread run; their startup races remain a follow-up.
