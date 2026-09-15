"""Run the opt-in NixOS fixture's independent validation access probes."""

import json
from pathlib import Path
import subprocess
import sys

workspace = Path.cwd()
state_probe = (
    "from pathlib import Path; "
    "p = Path('.devenv'); p.mkdir(exist_ok=True); "
    "(p / 'access-probe').write_text('validation state probe\\n')"
)
commands = {
    "inherited_validation": ("local", ["python3", "validate.py"]),
    "nix_eval": (
        "local",
        ["nix", "--extra-experimental-features", "nix-command", "eval", "--offline", "--expr", "1 + 1"],
    ),
    "nix_daemon": (
        "local",
        ["nix", "--extra-experimental-features", "nix-command", "store", "info", "--store", "daemon", "--json"],
    ),
    "local_state": ("local", ["python3", "-c", state_probe]),
    "shared_state": ("shared", ["python3", "-c", state_probe]),
    "devenv_local": ("local", ["devenv", "--offline", "--no-tui", "shell", "--", "python3", "validate.py"]),
    "devenv_shared": ("shared", ["devenv", "--offline", "--no-tui", "shell", "--", "python3", "validate.py"]),
}
observations = {}
for name, (directory, argv) in commands.items():
    cwd = workspace / directory
    observation = {"argv": argv, "cwd": str(cwd)}
    try:
        result = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, timeout=60)
        observation.update(exit_code=result.returncode, stdout=result.stdout, stderr=result.stderr)
    except (OSError, subprocess.TimeoutExpired) as error:
        observation.update(exit_code=None, error=str(error))
    observations[name] = observation

report = {"workspace": str(workspace), "observations": observations}
# The test owns this temporary output path; it is never a repository artifact.
Path(sys.argv[1]).write_text(json.dumps(report, sort_keys=True))
print(json.dumps(report, sort_keys=True))
