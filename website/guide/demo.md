# Research claim demo

This small, disposable project shows Coterie coordinating an analysis check and independent review. The fixture has eight invented observations and a manuscript claim that deserves closer inspection. Agent output can vary; the researcher decides whether any revised claim is scientifically appropriate.

## Prepare the fixture

Install [Coterie and Codex](./getting-started), then create a new Git project from the repository fixture:

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

The script reports a difference in means. The treatment group includes one large observation, so the manuscript's use of “typical” needs review.

## Ask Coterie to coordinate

Start `coterie` in the fixture and give its foreground agent this request:

> Investigate whether the claim in `manuscript.md` is supported by `data.csv` and `analysis.py`. Create separate tasks for an analysis check and an independent review. Ask a worker to add a robust summary and propose a correction. Follow the configured commit handoff. Review and integrate the contribution only after checking it, run the analysis again, and record validation before closing the tasks. Report what the data support and what still requires a human decision.

In a second terminal, inspect progress:

```console
coterie status
coterie events
coterie task ready
```

After the foreground agent reports completion, inspect `manuscript.md`, rerun `python3 analysis.py`, and compare `git log --oneline -3` with the fixture's initial commit. `status` and `events` show whether tasks were submitted, integrated, and closed. A worker exit alone does not establish that the claim was checked or accepted.

The [original demo notes](https://github.com/jolars/coterie/blob/main/docs/demo.md) include a longer inspection checklist.
