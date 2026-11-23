#!/usr/bin/env python3
"""test:linux (.gitlab-ci.yml): GitHub CI's "Build, test and typecheck" job, run on our Linux runner.

The commands are read from this commit's own .github/workflows/ci.yml (job build-and-test), never restated here, so
the two cannot drift (the Windows twin is windows_tests.ps1). Every step runs in file order, with the workflow's and
the job's `env:`, each printed as `+ <command>` before it runs.

Skipped by name: the steps the runner does not need. The apt install is one (the VM has the packages, a probe checked
every one of them), the toolchain resolve the other (rustup reads rust-toolchain.toml on the first cargo call).
`uses:` steps are GitHub actions the runner replaces (checkout, the cargo cache, node). Anything else this reader does
not know exits 2: a multi-line `run:` (move it to scripts/ci/), an unknown action, a step key it does not handle. A
step added to ci.yml is never skipped silently.

Exit code: the first failing step's; 2 for a step this reader does not know; 0 when every step passed.
"""
import os
import subprocess
import sys

import yaml

WORKFLOW = ".github/workflows/ci.yml"
JOB = "build-and-test"
SKIP_NAMES = {"Install Tauri system dependencies", "Resolve the pinned Rust toolchain"}
KNOWN_ACTIONS = ("actions/checkout@", "Swatinem/rust-cache@", "actions/setup-node@")
KNOWN_KEYS = {"name", "run", "uses", "with", "env", "id"}


def say(line: str) -> None:
    print(line, flush=True)


def main() -> int:
    with open(WORKFLOW, encoding="utf-8") as f:
        workflow = yaml.safe_load(f)
    job = (workflow.get("jobs") or {}).get(JOB)
    if job is None:
        say(f"no {JOB} job in {WORKFLOW}")
        return 2
    env = dict(os.environ)
    for scope in (workflow.get("env") or {}, job.get("env") or {}):
        for key, value in scope.items():
            env[key] = str(value)
            say(f"env {key}={value}")
    for i, step in enumerate(job.get("steps") or [], 1):
        label = step.get("name") or step.get("uses") or f"step {i}"
        unknown = set(step) - KNOWN_KEYS
        if unknown:
            say(f"step {i} ({label}): keys this reader does not handle: {sorted(unknown)}")
            return 2
        if "uses" in step:
            if str(step["uses"]).startswith(KNOWN_ACTIONS):
                say(f"skip {label} (a GitHub action the runner replaces)")
                continue
            say(f"step {i}: an action this reader does not know: {step['uses']}")
            return 2
        if label in SKIP_NAMES:
            say(f"skip {label} (not needed on this runner)")
            continue
        run = str(step.get("run") or "").strip()
        if not run:
            say(f"step {i} ({label}): no run")
            return 2
        if "\n" in run:
            say(f"step {i} ({label}): a multi-line run; move it to scripts/ci/ or add it to SKIP_NAMES")
            return 2
        step_env = dict(env)
        step_env.update({key: str(value) for key, value in (step.get("env") or {}).items()})
        say(f"+ {run}")
        code = subprocess.run(["bash", "-c", run], env=step_env).returncode
        if code != 0:
            say(f"step {i} ({label}) failed: exit {code}")
            return code
    return 0


if __name__ == "__main__":
    sys.exit(main())
