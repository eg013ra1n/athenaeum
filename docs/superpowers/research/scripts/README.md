# Moved: the measurement harness lives in athenaeum-lab

On 2026-10-06 every script that was here moved to the private `athenaeum-lab` repository, under `harness/`. The layout
did not change:

- `acceptance/`, including `tier1/`;
- `ln-math/`;
- `ln-model/`;
- `ln-scale-audit/`;
- `weight_audit_compare.py`.

The synthetic star lab and every new measurement tool live there too. The history up to the move stays in this
repository's git, at commit `002a1e08`.

A path in an older acceptance record, spec or plan that reads `docs/superpowers/research/scripts/<x>` now means
`athenaeum-lab/harness/<x>`. The scripts find this checkout through `ATHENAEUM_REPO`, which defaults to
`~/Documents/Projects/athenaeum`. The Rust probes they drive stay in `crates/athenaeum-core/examples/`.
