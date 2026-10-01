# Coterie 2026 Digital Science Catalyst Grant Proposal

## 1. THE PROBLEM

Computational researchers who maintain code and manuscripts in Git must decide
whether a revised scientific claim is ready to enter a paper. Depending on the
claim, they may need to check cited literature, a proof, analysis code,
numerical results, and an independent review. Researchers coordinate those
checks across their repository, editor, terminal, and conversations with
collaborators or AI assistants. They must then determine which evidence supports
the exact version of the claim under consideration.

This decision arises whenever a substantive claim changes. We have not measured
its frequency or cost across research groups. The immediate costs are time
spent coordinating and rechecking work and the risk of accepting a plausible
but unsupported claim. In one internal Coterie run, four tasks---theory,
literature review, numerical work, and independent proof review---were validated
and closed, but the lead agent still spent substantial effort on handoffs and
validation. That experience motivates a focused evaluation rather than a claim
of productivity gains.

## 2. YOUR WORKFLOW

A researcher starts Coterie in a Git project containing a manuscript and its
code, then asks whether a revised claim is supported. For example, a claim
about how normalization, learning rate, batch size, and stopping time determine
the resulting model may need a theory revision, a literature check, a numerical
pilot, and an independent proof review. The lead agent proposes those tasks;
Coterie records their assignments and launches authorized workers in isolated
Git workspaces.
Workers run available checks, edit code or manuscript files, and submit reports
and Git contributions. The lead examines the reports and exact commits, then
summarizes the evidence and remaining gaps for the researcher. If the researcher
approves the proposed revision, the lead asks Coterie to integrate the Git
contributions under safety checks, validates the combined project, and presents
the final revision for acceptance. After the researcher accepts it, an
authorized coordinator closes the tasks with the validation evidence.

Agents can plan, execute, and review within configured permissions. Coterie
enforces task ownership, keeps messages durable, and guards Git integration. It
does not judge whether the claim is scientifically sound. In the proposed pilot,
the researcher will inspect the sources, results, review, and wording before
integration and accept the final claim only after validation. They may request
more work at either point. These human decisions are a pilot protocol, not
software-enforced Coterie gates; authorized agents can currently integrate and
close tasks. Coterie launches from the Git repository and terminal already used
by computational researchers, with Codex as a separate agent program. The
output is a manuscript and code revision with a durable record of the work
behind it.

## 3. TRUST, AUDIT AND GOVERNANCE

A researcher can inspect task histories, agent messages, transcripts, events,
submitted commits, integration decisions, and validation reports. Coterie links
file changes to tasks, assignments, workspaces, and Git commits and records the
settings used for each run. These records show how the work proceeded. Agent
reports can identify cited literature, commands run, results, and the commit
reviewed, but Coterie does not currently authenticate those sources or verify
scientific claims. Grant work would make this evidence easier to inspect as one
summary while keeping reported claims distinct from recorded Git and task
state.

In the internal run, a numerical worker exited before submitting. Coterie left
its task unfinished and preserved its files for recovery. The lead agent also
overgeneralized a literature finding; Coterie did not detect that scientific
error. In the pilot, the lead will report unfinished work and identified gaps
in the evidence. The researcher must check the scientific interpretation and
can request another review or reject the revision.

Coterie refuses unsafe Git operations when ownership or repository state is
uncertain. The researcher remains accountable for scientific conclusions and
publication decisions.

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
https://github.com/jolars/coterie. Installable Linux binaries are available in
the v0.2.0 release (https://github.com/jolars/coterie/releases/tag/v0.2.0). A
polished public demo is not yet available.

We have used Coterie in a computational research project combining a LaTeX
manuscript and Python experiments. That field run showed that tasks could be
validated and closed, contributions tracked, an independent review recorded,
and unfinished work recovered after a worker exited. It also exposed
coordination friction and an error in scientific framing by the lead agent. The
field report is at
https://github.com/jolars/coterie/blob/main/docs/field-report-normreg-multi.md.
This is internal field evidence, not a controlled study. There are no external
users or customers yet.

## 6. ALTERNATIVES AND COMPETITORS

Researchers can keep coordinating manually, use one coding agent at a time, or
combine Git branches and pull requests with their own notes. They can also use
OpenAI Codex (https://openai.com/codex/) on its own or GitHub Copilot's agent
workflows
(https://docs.github.com/en/copilot/get-started/where-to-use-github-copilot).
Both are established commercial products. GitHub Copilot supports parallel agent
work and review through its own application. Codex is also the agent program
Coterie currently launches. Coterie is an early-stage, open-source project.

Coterie starts in an existing project and adds durable tasks and messages,
explicit ownership and permissions, recoverable work, and guarded Git
integration around an existing agent program. We would compare how well each
approach preserves a reviewable path from a revised claim to its sources,
checks, Git commits, and acceptance decision after an interruption.

## 7. WHERE THIS GOES

The near-term goal is a dependable workflow for computational research projects
in which code, results, and writing evolve together. We would compare manually
coordinated and Coterie-assisted reviews of similar claim revisions with six to
eight computational researchers. Our primary measure would be researcher time
from review request to a documented accept-or-reject decision. We would also
record whether participants can retrieve each source, check, and commit behind
the decision and whether known gaps reach them before approval. Longer term,
Coterie could support more agent providers and carefully scoped connections to
research tools, while keeping the project repository and human research
judgment central.

Coterie is open source. There is no commercial plan or pricing at present. If a
supported product becomes useful, research groups or institutions could be
potential customers. The proposed grant work would first establish whether that
need exists.

## 8. FIT WITH DIGITAL SCIENCE

Coterie would serve research writing and research integrity in computational
projects: when a researcher considers a revised claim, the supporting code,
results, sources, reviews, and decision should remain connected. The first users
would be researchers who keep manuscripts and analysis code together in Git.
Digital Science's experience with scholarly workflows and institutions could
help us recruit pilot users, test what evidence they need to accept a claim,
and identify useful connections to manuscript systems. Such connections are
prospective; Coterie currently runs in the Git project and terminal.

## 9. BUDGET

We would use up to £25,000: £12,000 to make task evidence easier to inspect and
build the pilot workflow, £8,000 to compensate six to eight researchers for
comparative review sessions, and £5,000 for independent usability and security
review. This would let us measure time to a documented claim decision, how
readily participants can trace its evidence, and whether failures reach human
review before wider use.
