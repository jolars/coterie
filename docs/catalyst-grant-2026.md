# Coterie 2026 Digital Science Catalyst Grant Proposal

## 1. THE PROBLEM

Computational researchers revise analysis code, numerical results, and
manuscript text together. A request to investigate a claim can involve checking
the literature, changing a proof or model, rerunning an experiment, and updating
the paper. Today, researchers coordinate these steps across Git repositories,
editors, terminals, and conversations with collaborators or AI assistants. A
single coding agent can help with an individual step, but the researcher must
still track dependencies, inspect competing edits, recover interrupted work, and
establish which checks support the final result.

For computational researchers who maintain code and a manuscript together and
use coding agents, this problem can recur with each revision that changes both.
It costs time spent coordinating and rechecking work, and it raises the risk of
accepting a plausible but unverified result. I have not measured a general time
or money cost. In one Coterie research run, four tasks---theory, literature
review, numerical work, and independent proof review---were validated and
closed, but the lead agent still spent substantial effort on handoffs and
validation. That experience motivates a focused evaluation rather than a claim
of productivity gains.

## 2. YOUR WORKFLOW

A researcher starts Coterie in an existing Git project containing analysis code
and manuscript sources, then asks its lead agent to tackle a multi-step problem.
The lead agent proposes bounded tasks, such as checking a claim, running a
numerical pilot, and reviewing the resulting proof. Coterie records the tasks
and dependencies, then launches authorized workers in separate Git workspaces.
Workers edit files or produce reports and run available checks. Under the
current sandboxed worktree policy, a worker who edits files asks an authorized
coordinator to review and commit those changes before submitting the result. The
lead agent examines reports and contributions, asks Coterie to integrate
accepted changes under Git safety checks, validates the combined project, and
closes the tasks. The researcher gets a Git revision and a durable record of the
work, evidence, and decisions behind that revision.

Agents may plan, implement, test, and report within configured permissions.
Coterie enforces task ownership, permissions, and guarded integration, and
stores messages durably. It does not decide whether a scientific claim is
correct. In the proposed research workflow, the researcher reviews the final
evidence and may reject or approve the result, or redirect the work. The
researcher launches Coterie from a terminal in the project's Git repository,
using tools computational researchers already know. Coterie currently launches
Codex as a separate agent program.

## 3. TRUST, AUDIT AND GOVERNANCE

A researcher can inspect task histories, agent messages, transcripts, events,
submitted commits, integration decisions, and validation reports. The record
links file changes to tasks, agent assignments, workspaces, and Git commits. For
each run, Coterie records the settings it used and where they came from. A
worker's exit does not count as success, and an interrupted task remains visible
so work can be recovered.

Git workspaces separate contributions, but do not restrict an agent's access to
the host. The standard worker profile uses Codex's sandbox to restrict writes
and network access. Coterie checks provider support before launch and does not
widen permissions when a task is blocked. A trusted operator can explicitly
select an unrestricted profile. These controls do not yet isolate hostile
processes running as the same Unix user. In disposable research projects, I
would test cross-workspace file access, blocked network connections, and whether
test credentials appear in records. I would record the selected policy, expected
and observed behavior, failures, and fixes.

Git commits record changes to project files and link them to submitted work.
Reports can point to the exact code and commit they evaluated. Literature
sources and scientific claims still require explicit citations and human
checking; Coterie does not currently authenticate external sources or verify
scientific claims. I would test a more consistent way to attach source
references and validation evidence to research tasks.

If an agent fails, reports uncertainty, or produces an unsound result, the
researcher can inspect the record, request more work, decline to integrate its
changes, or leave the task open. Coterie preserves recoverable work and refuses
unsafe Git operations when ownership or repository state is uncertain. The
researcher remains accountable for scientific conclusions and publication
decisions.

## 4. TEAM

I am Johan Larsson, a researcher in statistics and machine learning and the
developer of Coterie. I built it to coordinate agents in research projects while
keeping a clear record from each request to a reviewed result. My background
spans statistical research, computational experiments, and research software. I
am the sole developer. There are no collaborators or advisors at present.

## 5. WHERE YOU ARE TODAY

Coterie is working open-source software in active development. In one Git
project, it can launch a lead Codex agent, delegate to workers, keep tasks and
messages across interruptions, isolate Git contributions, and support explicit
integration, validation, and recovery. Source code and documentation are at
https://github.com/jolars/coterie. Published version 0.2.0 includes the core
operator loop; the current development build adds stopped-run recovery and
explicit approval-review profiles. A polished public demo is not yet available.

I have used Coterie in a computational research project combining a LaTeX
manuscript and Python experiments. In that field run, tasks were validated and
closed, contributions were tracked, an independent review was recorded, and
unfinished work was recovered after a worker exited. It also exposed
coordination friction and an error in scientific framing by the lead agent. The
field report is at
https://github.com/jolars/coterie/blob/main/docs/field-report-normreg-multi.md.
This is internal field evidence, not a controlled study. There are no external
users or customers yet.

## 6. ALTERNATIVES AND COMPETITORS

Researchers today can coordinate agent-assisted work manually, using individual
coding agents alongside Git branches, terminals, issue trackers, and their own
notes. [OpenAI Codex](https://openai.com/codex/) and [GitHub
Copilot](https://github.com/features/copilot) are established coding agents that
also support parallel or delegated work. Researchers using them still need to
connect a scientific question to the tasks, evidence, decisions, and manuscript
changes that follow.

Open-source orchestration alternatives include [Gas Town](https://gastown.dev/),
[Gas City](https://github.com/gastownhall/gascity),
[Daintree](https://github.com/daintreehq/daintree),
[FirstMate](https://github.com/kunchenguid/firstmate), and
[Agetor](https://github.com/alamops/agetor). Gas Town and Gas City are working
platforms with durable tasks and workspaces. Daintree and Agetor have released
interfaces for supervising parallel coding agents; FirstMate is a public agent
distribution built around a lead and crew. As of October 2026, public GitHub
interest ranges from fewer than 100 stars for Daintree and Agetor to roughly
18,000 for Gas Town and 7,400 for FirstMate. These tools already preserve or
expose aspects of agent work. Their public examples chiefly concern software
delivery, while Coterie's proposed pilot follows scientific claims through
literature, analysis, manuscript changes, and independent review.

Scientific-agent systems approach the problem from the other direction.
[FutureHouse](https://www.futurehouse.org/), for example, develops agents for
literature search, data analysis, and multi-step scientific discovery. Such
systems aim to improve what an AI agent can do scientifically.

Coterie instead focuses on the orchestration and provenance of computational
research. Its unit of work is not simply a coding task: a research question may
generate linked literature, theoretical, computational, manuscript, and
independent-review tasks. Coterie's run database records their dependencies,
assignments, messages, validation, and integration decisions. Git commits record
file changes, and Coterie links submitted and integrated commits to the work
that produced them. A researcher can inspect that record and reject a result
before accepting it.

The proposed project would test whether a research-oriented orchestration and
evidence layer makes multi-agent work easier to review and reduces the effort
required to establish how a computational research result was produced.

## 7. WHERE THIS GOES

The near-term goal is a dependable workflow for computational research projects
in which code, results, and writing evolve together. I would improve evidence
capture and run a paired pilot with 16 computational researchers who already use
coding agents. Each would attempt comparable, bounded revisions using Coterie
and their usual agent and Git workflow, with the order alternated. I would
measure the time they spend coordinating, then ask another researcher to trace
each final manuscript change to the code, checks, and decisions that support it.
The pilot would provide preliminary evidence on usability and traceability; its
size would not support general productivity estimates. Longer term, Coterie
could support more agent providers and carefully scoped connections to research
tools, while keeping the project repository and human research judgment central.

Coterie is open source. There is no commercial plan or pricing at present. If a
supported product becomes useful, research groups or institutions could be
potential customers. The proposed grant work would first establish whether that
need exists.

## 8. FIT WITH DIGITAL SCIENCE

Coterie would support research writing and research integrity in computational
projects by keeping manuscript changes connected to the code, experiments,
reviews, and decisions that support them. The immediate audience is researchers
and research software teams. Digital Science's experience with
[Overleaf](https://www.digital-science.com/products/overleaf/) could help me
recruit computational authors who keep LaTeX manuscripts and analysis code in
Git. Together, we could test whether Coterie's evidence record answers the
questions a coauthor asks before approving a manuscript revision. That pilot
would guide any later connection to manuscript systems; none exists today.

## 9. BUDGET

I would use up to £25,000: £12,000 for approximately eight weeks of my
development and pilot coordination time to improve evidence capture, test
sandbox boundaries, and prepare representative research tasks; £8,000 to pay 16
researchers £500 each for two structured tasks and an evidence review; and
£5,000 for an independent assessment of access controls and secret handling,
plus a usability walkthrough, with written findings. This would let me test the
workflow beyond my own projects and address the most consequential failures
before wider use.
