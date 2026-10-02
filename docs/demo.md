# Coterie demo: review a research claim

This self-guided demo uses eight invented observations and a deliberately
overstated manuscript claim. It shows how to ask Coterie to coordinate an
analysis check and independent review, then inspect task state, worker output,
and the Git result. This is a runnable walkthrough; no completed run or
scientific verification is claimed.

## Prepare a disposable project

On Linux, install [Coterie and its Codex
prerequisite](../README.md#installation) and authenticate Codex. Clone the
source repository and create a fresh Git project from the demo fixture:

```console
git clone https://github.com/jolars/coterie.git
cd coterie
demo_dir=$(mktemp -d)
cp examples/demo/* "$demo_dir"/
cd "$demo_dir"
git init -b main
git add .
git commit -m "Start claim-check demo"
python3 analysis.py
```

The script reports a seven-unit difference in **means**. The treatment group
contains one value of 33; its median is 11.5, while the control median is 9.5.
The manuscript's use of “typical” therefore deserves review. The tiny fixture
keeps the coordination and acceptance steps visible.

## Run the agent workflow

Start Coterie in that project:

```console
coterie
```

Give the foreground agent this request:

> Investigate whether the claim in `manuscript.md` is supported by `data.csv`
> and `analysis.py`. Create separate tasks for an analysis check and an
> independent review. Ask a worker to add a robust summary and propose a
> correction to the manuscript. Follow the configured commit handoff for worker
> edits. Review and integrate the contribution only after checking it, run the
> analysis again, and record the validation before closing the tasks. Report
> what the data support and what still requires a human decision.

The foreground agent decides how to carry out the request within its configured
permissions. Coterie records task ownership and dependencies, launches workers
in separate workspaces, and keeps submission, integration, and task closure as
distinct steps. Agent output and timing can vary.

From a second terminal in the demo project, inspect the run as it proceeds:

```console
coterie status
coterie events
coterie task ready
```

Once `coterie status` lists a worker, read its recent transcript with
`coterie logs <worker-name> --tail`, substituting the listed name. After the
foreground agent reports completion, compare the final manuscript and analysis
with the fixture's initial commit:

```console
git show "$(git rev-list --max-parents=0 HEAD)":manuscript.md
cat manuscript.md
python3 analysis.py
git log --oneline -3
coterie status
```

If the contribution was integrated, the final Git revision shows its accepted
file changes. `coterie status` and `coterie events` show whether tasks were
submitted, integrated, and closed; a worker exiting alone does not establish
success. The researcher still decides whether the revised claim is
scientifically appropriate.

For an observed research run, including an interrupted worker and its recovery,
see the [field report](field-report-normreg-multi.md). That run used a private
research repository, so this public fixture is the reproducible example.
