# Recovery handoffs

When a worker exits before submission, recover its task with a sourced report:

```console
coterie task recover --assignment <source-id> --reason 'Exited before submission.' \
  --report '{"validation_evidence":[{"text":"python3 validate.py passed in the assigned workspace; full suite blocked by Nix daemon access under workspace-write/network-deny policy.","source":"message cm-01ARZ3NDEKTSV4RRFFQ69G5FAW"}],"unfinished_steps":[{"text":"Port result.json, rerun validation, and request a coordinator commit before submission.","source":"message cm-01ARZ3NDEKTSV4RRFFQ69G5FAW"}]}'
```

The operator or an agent with `task:recover` selects the report from available
messages, logs, or artifacts. Coterie records that caller's identity. Every
statement has a source reference, but references and claims are unverified
reporter input. Messages and logs retain their existing read permissions.
If evidence is unavailable, leave the report empty or omit it; the handoff
explicitly has no reported evidence. Do not infer that checks passed.

## Reading the handoff

`prime.recoveries` provides the source assignment ID, preserved path, base
commit, continuation ID, and compact `handoff` metadata. The metadata includes
HEAD, path counts, the first check and next step, their sources, and total
report counts. Follow the source reference with:

```console
coterie assignment show <source-id> --json
```

The continuation's own assignment ID also retrieves its source handoff.
Concatenate `data.text` across pages before decoding JSON. Continue with
`--after <next_cursor>` and `--revision <revision>` until `eof` is true. MCP uses
`assignment_show` with the same fields. Reconnects authenticate again and retain
the revision and cursor. The complete document contains:

| Field | Meaning |
| --- | --- |
| `recoveries` | Source path, exact path bytes, recorded base commit, reason, and continuation links. |
| `recovery_handoffs[].mechanical` | Git observations recorded before retirement: HEAD, any unfinished Git operation, path lists, and inspection completeness. |
| `recovery_handoffs[].reported` | Validation evidence and unfinished steps supplied by the recovering caller, each with its source reference. |
| `operation_id`, `source_assignment_id`, `recorded_at`, `reported_by` | Provenance for each handoff. A null reporter denotes the operator. |

The [full handoff example](../examples/recovery-handoff.json) and
[report example](../examples/recovery-report.json) are checked against Rust
types. The [assignment document schema](../schemas/assignment-detail-v1.schema.json)
defines the full output.

Dirty paths are the union of observed, nonignored changes. Staged and unstaged
lists can overlap. Untracked directories are expanded. Display paths may replace
invalid UTF-8; `path_bytes` retains each exact repository-relative path.
Unreadable paths and entries marked assume-unchanged or skip-worktree make
`complete` false, even if other path lists are empty. Inspection does not write
or refresh the index. Missing or ambiguous workspace ownership refuses recovery.

The snapshot records recovery time and remains unchanged if an operator later
edits the source. It is not a live status query. Migration 17 preserves older
events without inventing snapshots: their handoff metadata is absent (null in
`prime`), and there is no corresponding full handoff document.

## Continuing safely

Spawn a worktree role on the reopened task. The continuation inspects the
preserved source and ports selected changes into its fresh worktree. Recovery
does not copy files or grant writable ownership of the source, its index, or
its references. Use the fresh assignment's normal coordinator-commit handoff,
then submit. Review and integrate the submission, validate the integrated
target, and explicitly close the task. Dependencies stay blocked until closure.

Recovery stores the handoff, retires the assignment, and records its operation
result in one transaction. Retry an uncertain outcome with the same operation
ID and identical arguments, including the report. A successful retry returns
the original observation and report without re-inspecting Git.

## Verification

Deterministic provider tests use real linked worktrees. They cover an exited
worker's staged artifact, unstaged edits, untracked files, a sourced report,
fresh continuation, MCP reconnects and full-detail paging, submission,
integration, validation, and closure. The source HEAD, files, and raw index
bytes must survive, and the continuation receives only its fresh writable
workspace. Full handoff access does not grant source transcript access.

Unit tests cover native paths, incomplete inspections, attribution, credential
redaction, missing source references, immutable storage, changed-request
conflicts, and preserved observations on retry. Crash tests interrupt both
inspection and transactional persistence. Upgrade tests cover every released
schema and preserve missing historical evidence. Ordinary tests use no model
credentials or real provider sessions.

The gate passed on NixOS on September 15, 2026: `task check` completed all 495
ordinary tests, formatting, linting, rustdoc, dependency audits, release checks,
and Nix evaluation. The 22 ignored tests include explicit regeneration commands
and opt-in provider tests.

```console
cargo test recovery --bin coterie
cargo test crash_matrix_recover_assignment --bin coterie
cargo test --test supervisor_runtime recovery
cargo test every_released_schema_upgrades_through_all_forward_migrations --bin coterie
task check
```
