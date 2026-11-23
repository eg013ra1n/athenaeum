#!/usr/bin/env bash
# One copy of rustafits and solvemyastro. Run by .github/workflows/ci.yml (build-and-test) and so by the GitLab
# test:linux job, which reads that job's steps. Members name the two by version or git rev (solvemyastro, starloom)
# and the root Cargo.toml's [patch] points them at the submodules. A patch that stops applying is silent (cargo just
# builds a second copy), so re-resolve the lock first (a gitlink-only bump changes nothing committed) and look.
set -euo pipefail
cargo metadata --format-version 1 >/dev/null
python3 - <<'EOF'
import re, sys
lock = open("Cargo.lock").read()
bad = [b for b in lock.split("[[package]]") if re.search(r'^name = "(rustafits|solvemyastro)"$', b, re.M) and "source = " in b]
sys.exit("a second rustafits or solvemyastro in Cargo.lock: a [patch] no longer applies" if bad else 0)
EOF
