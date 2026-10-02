"""Summarize the toy observations used in the Coterie walkthrough."""

import csv
from pathlib import Path
from statistics import mean


with Path(__file__).with_name("data.csv").open(newline="") as source:
    rows = list(csv.DictReader(source))

control = [int(row["value"]) for row in rows if row["group"] == "control"]
treatment = [int(row["value"]) for row in rows if row["group"] == "treatment"]

print(f"Control mean: {mean(control):.1f}")
print(f"Treatment mean: {mean(treatment):.1f}")
print(f"Difference in means: {mean(treatment) - mean(control):.1f}")
