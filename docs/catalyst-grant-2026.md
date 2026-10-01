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

This problem recurs whenever a research project has several linked tasks or
revisions. It costs researchers time spent coordinating and rechecking work, and
it raises the risk of accepting a plausible but unverified result. I have not
measured a general time or money cost. In one Coterie research run, four
tasks---theory, literature review, numerical work, and independent proof
review---were validated and closed, but the lead agent still spent substantial
effort on handoffs and validation. That experience motivates a focused
evaluation rather than a claim of productivity gains.

## 2. YOUR WORKFLOW

A researcher starts Coterie in an existing Git project containing analysis code
and manuscript sources, then asks its lead agent to tackle a multi-step problem.
The lead agent proposes bounded tasks, such as checking a claim, running a
numerical pilot, and reviewing the resulting proof. Coterie records the tasks
and dependencies, then launches authorized workers in isolated Git workspaces.
Workers edit files or produce reports, run available checks, and submit their
results. The lead agent examines their reports and contributions, asks Coterie
to integrate accepted changes under Git safety checks, validates the combined
project, and closes the tasks. The researcher gets a Git revision and a durable
record of the work, evidence, and decisions behind that revision.

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
https://github.com/jolars/coterie. The current development build has more
functionality than the published 0.1.0 package; a polished public demo is not
yet available.

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
notes. OpenAI Codex and GitHub Copilot can perform substantial coding tasks, and
increasingly support parallel or delegated work. These tools provide the agents
that do the work, but do not by themselves provide a research-specific record
that connects a scientific question to delegated tasks, competing contributions,
validation evidence, integration decisions, and the resulting manuscript and
analysis changes.

A second category is emerging around multi-agent software development. Gas Town
and Gas City coordinate coding agents using persistent tasks, isolated Git
workspaces, handoffs, and automated integration. Other projects such as Daintree
and Agetor similarly make it easier to supervise several coding agents working
in parallel. These are the closest technical alternatives to Coterie, but they
are primarily designed around software-engineering throughput and delivery.

Scientific-agent systems approach the problem from the other direction.
FutureHouse, for example, develops agents for literature search, data analysis,
and multi-step scientific discovery. Such systems aim to improve what an AI
agent can do scientifically.

Coterie instead focuses on the orchestration and provenance of computational
research. Its unit of work is not simply a coding task: a research question may
generate linked literature, theoretical, computational, manuscript, and
independent-review tasks. Coterie records their dependencies, assignments,
messages, contributions, validation, and integration in the research project's
Git history, while preserving explicit points at which a researcher can inspect
or reject the result.

The proposed project would test whether a research-oriented orchestration and
evidence layer makes multi-agent work easier to review and reduces the effort
required to establish how a computational research result was produced.

## 7. WHERE THIS GOES

The near-term goal is a dependable workflow for computational research projects
in which code, results, and writing evolve together. I would improve evidence
capture, reduce the manual work of coordinating agents, and test the workflow
with researchers on realistic tasks. Longer term, Coterie could support more
agent providers and carefully scoped connections to research tools, while
keeping the project repository and human research judgment central.

Coterie is open source. There is no commercial plan or pricing at present. If a
supported product becomes useful, research groups or institutions could be
potential customers. The proposed grant work would first establish whether that
need exists.

## 8. FIT WITH DIGITAL SCIENCE

Coterie would support research writing and research integrity in computational
projects by keeping manuscript changes connected to the code, experiments,
reviews, and decisions that support them. The immediate audience is researchers
and research software teams. Digital Science's experience with scholarly
workflows and institutions could help me recruit appropriate pilot users and
test what evidence they need. It could also help me identify where this workflow
should connect to the tools they already use, including manuscript systems.

## 9. BUDGET

I would use up to £25,000: £12,000 to improve how Coterie records evidence for
research tasks and produce a usable pilot workflow; £8,000 to compensate
researchers for structured pilot sessions and feedback; and £5,000 for
independent usability and security review. This would let me test the workflow
beyond my own projects, measure time spent coordinating and how readily
participants can review results, and address the most consequential failures
before wider use.
