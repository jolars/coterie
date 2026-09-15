# Bounded context inspection

Use `prime` to recover current task context after a transition or reconnect.
Use `progress` to track lifecycle changes between inspections. Use `task show`
and `assignment show` when you need the complete description, report, or recovery
source. To inspect recent raw output, use `logs <agent> --tail --limit 4096`.

## Bounds

| View | Enforced bound |
| --- | --- |
| Each text preview | At most 512 original UTF-8 bytes, with `total_bytes` and `truncated`. |
| Dependencies in a task preview | Eight unresolved IDs, plus an omitted count. Full task details retain the complete unresolved list. |
| Tasks per `prime` page | Default 20, maximum 50, shortened further to fit the byte budget. |
| Compact task context | At most 65,536 bytes of compact JSON, including tasks, ready IDs, pinned current and active tasks, recovery previews, and pagination fields. |
| Full-detail page | Default 16,384 document bytes, maximum 65,536, plus at most three bytes to finish a UTF-8 character. |
| Raw transcript page or tail | Default 65,536 stored bytes, caller-selectable from 1 through 65,536, plus at most three bytes for UTF-8. |

The task budget counts JSON escaping, including control characters, before
admitting another task. Titles, descriptions, reports, result summaries, and
recovery reasons cannot grow a preview beyond its bound. Previews do not rewrite
stored text. A task ID references its full document, which also lists every
assignment ID. An assignment ID references its full report and recovery links.
These references survive submission, recovery, resubmission, and closure.

Recovery handoffs add path counts and the first reported check and unfinished
step, each with a 512-byte text and source preview and total item counts. Full
path lists and reports use `assignment show` pages and do not repeat in `prime`.
The same 64 KiB task-context budget includes these previews. See
[recovery handoffs](recovery-handoffs.md) for snapshot and attribution semantics.

`current_task` pins the caller's latest assigned task even after it finishes.
`active_task` describes only an assignment that has not ended. Each task reports
the latest assignment's ID, generation, recorded session state, and commit
identities. A task can remain in progress after its process exits; its
`next_action` then calls for provider inspection. Recovery exposes the preserved
source and the fresh continuation separately. Next actions describe mechanical
prerequisites, not verified liveness, permission grants, or acceptance judgments.

Prime's identity, projects, peers, commands, and active commit handoffs are
outside the task budget. They scale with the configured run, its attached
projects, agent count, and path lengths. The 64 KiB task budget is therefore
**not a universal bound on the complete prime response**. Human pretty printing
and MCP's textual plus structured representations also add serialization bytes.
The fixture below enforces separate complete-response bounds: 32 KiB for either
CLI view and 48 KiB for the serialized MCP result. It also exercises a shortened
page with escape-heavy titles and descriptions, verifying every task remains
retrievable without duplicates or omissions.

Full details are JSON documents transported as `text` pages. Concatenate text
before decoding JSON. Continue with `after=next_cursor` and the first page's
`revision`; a changed document returns a conflict instead of mixing versions.
Restart from zero to read the new version. Detail pages are independent of the
1 MiB RPC frame limit, so a large document can be drained in smaller pieces.
See the [CLI contract](cli-contract.md#coterie-task-show)
for authorization, failure codes, and generated schemas.

Transcript tails seek directly within the checked, private session file. They
return `start_cursor`, `next_cursor`, `total_bytes`, `partial_head`, and the
ordinary session, EOF, terminal, and incomplete-tail fields. A partial first
frame remains labeled as partial; raw output is never promoted into a semantic
activity claim. Follow mode uses tail selection once, then resumes with the
returned session and cursor. `logs --after 0` retains the complete transcript.

## Reproducible measurements

The regression fixture uses the Coterie binary built from this checkout, real
temporary Git repositories, and a deterministic fake provider implementing the
Codex adapter contract. Its probe advertises Codex 0.153.4; these are **not live
Codex measurements**. The fixture uses `builtin:standard@1` with read-only
reviewers. Ordinary CI needs neither authentication nor model access.

The fixture creates five tasks, each with a 54,000-byte description, a
63,000-byte submitted report, and a 49,500-byte acceptance report. All five reach
explicit validated closure. It retrieves every complete task and assignment
document and checks text equality. Worker and operator reads verify current
context before and after submission, closure, and a foreground reconnect.
Separate tests verify recovery through integration and closure, MCP reconnects,
missing capabilities, stale credentials, Unicode, and revision conflicts.

Measurements from the fixture on NixOS, September 15, 2026:

| Measurement | Bytes |
| --- | ---: |
| Full task and assignment documents combined | 1,194,980 |
| Complete JSON CLI `prime` output | 12,940 |
| Complete human CLI `prime` output | 15,408 |
| One bootstrap transcript frame | 4,805 |
| One serialized MCP-context transcript frame | 31,770 |
| Raw transcript including four copies of each frame and current activity | 146,419 |
| Serialized CLI pages needed to drain that transcript at 4,096 bytes per page | 172,844 |
| Serialized CLI tail response reaching the same current activity | 4,710 |

Draining the transcript took 36 pages; the tail reached the current activity in
one page. The test counts bootstrap and serialized context, including MCP's
textual and structured copies. It verifies that reading every ordinary page
reconstructs the entire stored transcript with all four copies intact. These
are byte measurements, not tokenizer estimates or claims about model cost.
Paths, provider instructions, and output envelopes can change exact totals;
the regression asserts documented ceilings rather than relying on these totals
as golden strings.

Run the measurements and related checks with:

```console
cargo test --test supervisor_runtime context -- --nocapture
cargo test --test supervisor_runtime recovery
cargo test --bin coterie supervisor::context
cargo test --bin coterie transcript::tests
task check
```

Regenerate the reviewed schemas and example from their Rust types with:

```console
cargo test --bin coterie regenerate_context_contracts -- --ignored
cargo test --bin coterie regenerate_mcp_catalog -- --ignored
```
