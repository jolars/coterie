# Field report: normreg-multi research run

Recorded September 15, 2026, by the coordinating agent after run
`cr-01M2HV7JE2YF36E4VSWD86THG9`. These are observations from one NixOS research
workflow, with proposed follow-ups for triage. They are not new product
contracts or evidence that the same problems affect every provider
configuration.

## Outcome and workload

Coterie kept the work recoverable and reviewable, but operating the run required
substantial coordinator attention. The user authorized three research tasks:
theory, a literature audit, and a numerical pilot. A separate referee reviewed
the proof. Five worker sessions handled these four tasks, including a
replacement for the first numerics worker. All four tasks reached validated
closure.

The project combines a LaTeX manuscript, Python calculations, and a devenv
environment. Theory and numerics workers used isolated Git worktrees. Literature
and proof reviews produced reports, while the coordinator integrated manuscript
changes sequentially.

The run used the Coterie executable below; its exact source revision was not
verified. The repository was at `6c81e1b6baa7d7134ad686f7562d0701c87dffbb` when
these notes were added, which should not be treated as the executable's
revision.

```text
/nix/store/bynzspjhmikxl1cijv41gmw537knngsg-coterie-0.1.0/bin/coterie
```

## What worked

- Task identities, messages, submissions, and acceptance records remained
  available across foreground interruptions and an MCP bridge change.
- Isolated contributions integrated without conflicts. Independent proof review
  and numerical checks could be tied to specific submitted commits.
- Provider exit did not imply success. When the first numerics session exited
  before committing or submitting, its task remained unfinished and its files
  survived for continuation.
- Submission and acceptance remained separate. The coordinator reviewed results,
  integrated Git contributions, validated the target, and then closed tasks.
  This supported a research workflow with stronger acceptance requirements than
  merely receiving a worker's report.

## Friction and proposed follow-ups

### 1. Worker edits succeeded, but Git commits could be blocked

The theory worker and replacement numerics worker encountered permission errors
writing Git metadata shared with the primary repository. The replacement could
edit its assigned files but received this error when staging them:

```text
fatal: Unable to create '/home/jola/research/normreg-multi/.git/worktrees/cr-01M2HV7JE2YF36E4VSWD86THG9-ca-01M2J123B1GNWH6M4C1869E3P0/index.lock': Read-only file system
```

The coordinator validated and committed the contributions through approved shell
execution, after which the workers could submit. The original numerics worker
had successfully staged its files, so this was not a uniform failure across
sessions. The precise provider-policy cause remains to be isolated.

**Suggested priority: high.** Reproduce the complete edit, validate, stage,
commit, and submit path with an actual linked worktree under the selected
provider policy. Diagnose an unsupported commit path early. Consider scoped
commit support or an explicit coordinator-commit handoff. Preserve restrictions
on the primary checkout, sibling worktrees, shared references, and repository
configuration; broad write access to the Git common directory is not an adequate
fix.

Some validation commands also hit Nix daemon access or `.devenv` write
restrictions. Other workers successfully entered the environment. Treat these as
separate environment-access observations until reproduced, and report which
validation steps the selected policy permits.

### 2. Context inspection repeated large amounts of text

Some `prime` displays included full task descriptions and lengthy reports on
repeated calls. Worker transcripts also contained substantial bootstrap and
serialized context before the current activity. Inspecting those logs required
multiple chunk reads. No controlled token-cost measurement was made.

**Suggested priority: high.** Provide bounded task and assignment summaries with
stable references for fetching full descriptions and reports. Measure serialized
response size on a fixture with long reports and several completed tasks. Ensure
recovery provenance and actionable blockers remain visible in the compact view.
The existing bounded `progress` feed is useful, but does not replace the current
task context needed after a transition or reconnect.

### 3. Recovery preserved work but required a manual handoff

The first numerics worker left four staged artifacts and exited without a commit
or submission. The cause of that exit was not established. The coordinator
inspected its state, invoked `task recover`, launched a continuation, and
supplied the remaining work and validation context. The replacement copied the
preserved artifacts into its fresh worktree, then completed the handoff.

This follows the documented recovery model in [DESIGN.md](../DESIGN.md):
preserved files are contextual input, and continuation does not inherit writable
ownership of the old workspace. Recovery itself worked. The opportunity is to
reduce the effort needed to identify and transfer useful work safely.

**Suggested priority: medium.** Make the handoff concise and explicit: preserved
path and base commit, dirty/staged/untracked paths, prior validation evidence,
and unfinished steps. Clearly distinguish agent-reported checks from recorded
mechanical state. A continuation test should preserve the source, expose these
facts, and carry the task through integration and validated closure. Any
optional transfer helper should identify conflicts and require an explicit
selection of changes.

### 4. Protocol bookkeeping occupied coordinator attention

The coordinator maintained separate progress and inbox cursors, drained pages,
acknowledged handled messages, obtained operation IDs before mutations, and
rechecked task state. These safeguards worked; no message loss or duplicate
mutation was established in this run.

**Suggested priority: medium.** Consider client support for this bookkeeping
while preserving its semantics. Progress must not acknowledge messages.
Uncertain mutation retries must reuse the original operation ID and identical
arguments. Exercise empty pages with `has_more`, reconnects, partial message
handling, and uncertain mutation outcomes before reducing agent-facing
instructions.

A later foreground turn also had a stale MCP tool identifier. Rediscovering the
current bridge restored access to the same run and lead identity. Clear
reconnect guidance would help; this was not evidence of lost supervisor state.
Automatic foreground wake-up support was not established by this run.

## Research judgment and limits

The literature audit answered its assigned question. The coordinator generalized
its overlap findings too far and let the research framing drift from the user's
intended follow-up: how normalization interacts with learning rate, batch size,
and stopping time to determine the resulting model. That was a coordination and
interpretation error. Coterie's durable records helped revisit it, but assessing
scientific relevance and maintaining scope remain agent responsibilities under
the design's mechanics/judgment boundary.

These notes establish workflow observations, not general reliability rates,
productivity gains, or scientific novelty. Follow-up implementation should first
pin the provider version and effective policy and reproduce the specific issue.

## Evidence anchors

- Theory assignment: `ca-01M2HVF3DYZM4STC686R59RP19`; submitted commit
  `c82e75270cca9b92d45716b7a018867b88691035`, integrated as
  `8326033a2c958061b962112be4c4b170b54f7464` in normreg-multi.
- Independent proof report: `cm-01M2HXFDVTJVK5AA5EF0KV49WD`.
- Literature report: `cm-01M2HW4YS755JWH2Z7VM2ZQJ05`.
- Numerics recovery: assignment `ca-01M2HW7RQTEFP0NZWCTQXAQ9M4` continued as
  `ca-01M2J123B1GNWH6M4C1869E3P0`; submitted and integrated commit
  `945a89abc3ad3d573f9caa18f9b226968113901f` in normreg-multi.
- The coordinator independently reproduced the pilot's JSON before recovery; its
  SHA-256 was
  `1c72c1dd3e241094f21bc0f62492368e84dd0e67881d8fa8b910299fbdb266de`. Recovery
  retained all 864 evaluation records. Subsequent formatting changed Markdown
  only; the experiment script and JSON were preserved.
