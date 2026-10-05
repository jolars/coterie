# Coterie 2026 Digital Science Catalyst Grant Proposal

## 1. THE PROBLEM

Before approving a computational manuscript revision, a coauthor must establish
which code, experiments, sources, and checks support the changed claims. This
decision recurs whenever a revision changes both the analysis and the paper.
Researchers using coding agents currently assemble that evidence across Git
repositories, terminals, and conversations, while tracking dependencies,
reviewing competing edits, and recovering interrupted work.

This coordination costs time and risks accepting plausible but unsupported
results. I have not measured a general time or money cost. In one Coterie
research run, four tasks—theory, literature review, numerical work, and
independent proof review—reached validated closure, but handoffs and validation
still required substantial coordinator effort. The grant would test whether a
consistent evidence record helps another researcher review the resulting
manuscript changes.

## 2. YOUR WORKFLOW

A researcher starts Coterie from a terminal in an existing Git project
containing code and manuscript sources, then asks its lead agent to investigate
a claim. The lead proposes bounded tasks, such as checking the literature,
running an experiment, and reviewing the proof. Coterie records dependencies and
launches authorized workers in separate Git workspaces. Workers edit files,
produce reports, and run checks. Under the current sandboxed worktree policy,
workers ask an authorized coordinator to review and commit edits before
submission. The lead reviews contributions, requests integration under Git
safety checks, validates the combined project, and closes tasks.

Agents perform these steps within configured permissions. Coterie enforces
ownership and integration rules and preserves messages; it does not judge
scientific correctness. In the proposed workflow, a researcher examines the
resulting Git revision and evidence record before approving the manuscript
change, requesting more work, or rejecting it. Coterie currently runs on Linux
and launches Codex as a separate program.

## 3. TRUST, AUDIT AND GOVERNANCE

Researchers can inspect task histories, messages, transcripts, commits,
integration decisions, validation reports, and the settings used for each run. A
worker's exit does not count as success. Interrupted work remains recoverable,
and Coterie refuses unsafe Git operations when ownership or repository state is
uncertain.

The funded improvement would be a report template and export linking each
manuscript change to its task, source references with cited passages, code and
data versions, check commands and results, review comments, and acceptance
decision. Missing evidence would be explicit. A sample report would follow one
revised claim through these links. Sources and scientific claims still require
human checking; Coterie does not authenticate external sources or verify
conclusions.

Git workspaces separate contributions. Codex's sandbox restricts worker writes
and network access, and Coterie checks provider support before launch. Only a
trusted operator can select an unrestricted profile; blocked work does not
authorize wider permissions. These controls do not isolate hostile processes
running as the same Unix user.

In disposable projects, I would test an unauthorized write to a sibling
worktree, a prohibited network connection, and exposure of planted test
credentials in records. The first two should be blocked and reported without
widening permissions; any leaked test credential would count as a failure. I
would also give a review agent a citation that does not support the manuscript
claim: it should flag the mismatch and request human review. I would record
policy, expected and observed outcomes, failures, and fixes. The researcher
remains accountable for conclusions and publication.

## 4. TEAM

I am Johan Larsson, a researcher in statistics and machine learning and
Coterie's sole developer. My background spans statistical research,
computational experiments, and research software. I built Coterie to coordinate
agents in my research while preserving a record from request to reviewed result.
There are no collaborators or advisors at present.

## 5. WHERE YOU ARE TODAY

[Coterie](https://github.com/jolars/coterie) is working open-source software.
Published version 0.2.0 includes the core operator loop; the development build
adds stopped-run recovery and explicit approval-review profiles. The [public
guide and reference](https://coterie.fyi/) explain installation, permissions,
the operator workflow, and CLI commands. The site describes the development
build, including capabilities not yet in the published release.

My [field
report](https://github.com/jolars/coterie/blob/main/docs/field-report-normreg-multi.md)
describes a LaTeX and Python research run with validated tasks, tracked
contributions, independent proof review, and recovery after a worker exited. It
also documents coordination friction and a scientific framing error by the lead
agent. This is internal field evidence, not a controlled study. A [self-guided
demo](https://coterie.fyi/guide/demo) uses a public example to show Coterie's
current workflow. The grant would add an export linking manuscript changes to
inspectable evidence.

## 6. ALTERNATIVES AND COMPETITORS

The immediate alternative is manual coordination using coding agents, Git
branches, issue trackers, and notes. [OpenAI Codex](https://openai.com/codex/)
and [GitHub Copilot](https://github.com/features/copilot) are established
commercial coding agents. [Gas Town](https://github.com/gastownhall/gastown) is
a working open-source orchestration system with persistent tasks and workspaces
and roughly 18,000 GitHub stars as of October 2026. These tools already preserve
aspects of agent work. [FutureHouse](https://www.futurehouse.org/) is a
nonprofit developing agents to automate scientific research.

Coterie's proposed contribution is a workflow in the researcher's existing Git
project that connects literature, theory, experiments, and manuscript changes to
independent review. The pilot would test whether its evidence record makes those
changes easier for a coauthor to assess.

## 7. WHERE THIS GOES

I would recruit 16 computational researchers who use Git and coding agents on
Linux, through research software communities and academic contacts. Each would
attempt two comparable revisions in prepared public research projects, using
Coterie and their usual agent and Git workflow. Tasks would require a source
check, a small experiment, and a manuscript change, with a two-hour limit each.
I would counterbalance task assignment and workflow order and record tools and
experience.

Each participant would then review another participant's pair of results, with
review order also counterbalanced. The primary outcome would be the proportion
of revisions whose supporting sources, code, checks, and decisions the reviewer
correctly identifies within 20 minutes per revision. I would score responses
against a predefined rubric and the source files and records. Missing evidence
and failed or incomplete runs would count as unsuccessful traces. Reviewers
would know which workflow produced each record, a limitation of the comparison.
Review time and coordination time would be secondary measures. An improvement
over the usual workflow of at least 20 percentage points in successful traces,
without more incorrect links, would support a larger study. This exploratory
target would guide further evaluation; the pilot cannot establish general
productivity gains.

Eight weeks of my effort would span twelve calendar weeks. Weeks 1–4 would cover
evidence export, task preparation, recruitment, and initial access-control
assessment. Weeks 5–8 would cover the paired sessions and reviews. Weeks 9–12
would cover fixes, reassessment, and a public report and demo. The report would
include incomplete runs and recruitment shortfalls.

Participant interviews would establish revision frequency, current coordination
costs, and demand for continued use. Longer term, Coterie could support more
providers and research tools. It is open source, with no pricing or commercial
plan; research groups and institutions are potential customers for future
support.

## 8. FIT WITH DIGITAL SCIENCE

Coterie would support research writing and integrity for computational
researchers and research software teams. It sits in their existing Git and
terminal workflow. Digital Science's experience with
[Overleaf](https://www.digital-science.com/products/overleaf/) could help
recruit authors and shape the evidence a coauthor needs before approving a
revision. There is no Overleaf integration today; the pilot would inform any
later connection to manuscript systems.

## 9. BUDGET

Of the £25,000, I would allocate £12,000 to eight weeks of development and
coordination to deliver the evidence export, report, and demo. I would reserve
£1,000 for capped agent usage and £8,000 to pay 16 researchers £500 each for two
tasks and a paired evidence review.

The remaining £4,000 would fund an assessor independent of development to test
workspace and network restrictions, inspect secret handling, conduct a usability
walkthrough, document findings, and verify fixes.
