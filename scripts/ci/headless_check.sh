#!/usr/bin/env bash
# Headless stays headless: core without its default features compiles, and neither native-heavy dependency
# (rustafits, solvemyastro) is in its normal dependency tree, starloom's layers included. Run by
# .github/workflows/ci.yml (build-and-test) and so by the GitLab test:linux job.
set -euo pipefail
cargo check -p athenaeum-core --no-default-features
# The tree is read first, so a failing `cargo tree` fails the check instead of reading as "nothing found".
tree=$(cargo tree -p athenaeum-core --no-default-features -e normal --prefix none)
if grep -E '^(rustafits|solvemyastro) ' <<<"$tree"; then
  echo "a headless athenaeum-core build pulls rustafits or solvemyastro" >&2
  exit 1
fi
