# Contributing to Athenaeum

Thanks for wanting to help. Athenaeum manages astrophotography catalogs, so most
bugs are found with real data — a FITS file that parses wrong is worth more than
a synthetic test that passes.

## Getting set up

```bash
git clone --recursive https://github.com/eg013ra1n/athenaeum.git
cd athenaeum
npm install
npm run tauri dev
```

`--recursive` matters: [rustafits](https://github.com/eg013ra1n/rustafits)
(image rendering) and
[solvemyastro](https://github.com/eg013ra1n/solvemyastro) (plate solving) are
submodules *and* Cargo workspace members. Without them nothing builds. If you
already cloned flat, run `git submodule update --init --recursive`.

The Rust toolchain is pinned in `rust-toolchain.toml`; rustup fetches the right
version for you.

## The rules that matter

These are not style preferences. A pull request that breaks one of them will be
asked to change.

**Two backends stay in sync.** There are two hosts for the same logic: the Tauri
desktop shell (`crates/athenaeum-tauri/src/commands/<domain>.rs`) and the Axum
web server (`crates/athenaeum-web/src/routes/<domain>.rs`). They mirror each
other one-for-one. Adding or changing a command in one requires the matching
change in the other, in the same pull request. Real logic belongs in
`athenaeum-core`; both hosts are thin wrappers over it.

**The frontend never imports Tauri directly.** No `@tauri-apps/*` import outside
`src/api/`. The frontend talks to whichever backend is active through the single
`api` object, selected by `VITE_TARGET`. Desktop-only code lives in
`src/api/desktop.ts`.

**Mind the serde boundary.** Rust is snake_case, TypeScript is camelCase. Use
`#[serde(rename_all = "camelCase")]` and check that the interface in
`src/types/models.ts` matches the Rust struct. Most "the value is undefined"
bugs are a casing mismatch, not a logic error.

**Never swallow an error.** Log it before returning. Silent failures have
repeatedly cost hours here. Inside `athenaeum-core` use `anyhow::Result`;
convert with `.map_err(|e| e.to_string())` at the command boundary.

**Use design tokens, not raw colours.** `bg-surface`, `text-content-muted`,
`bg-accent`, `text-error` and friends — so both the dark and light themes keep
working.

**Log through `tracing`.** No `println!` or `eprintln!` in production code (CLI
binaries and tests are exempt). A message is a short stable phrase with the data
in snake_case fields: `info!(root_id, new = 12, "scan finished")`, never
`info!("scan finished — 12 new")`.

## Before you open a pull request

Run the three gates:

```bash
cargo build --workspace
cargo test -p athenaeum-core
npx tsc --noEmit
```

CI runs exactly these on Ubuntu. Clippy is not a gate.

## Commits and branches

`main` is the development trunk; releases are tags on it. Base your branch on
`main` and open the pull request against `main`.

Commit messages follow Conventional Commits — `feat:`, `fix:`, `docs:`,
`chore:`, `perf:`, `refactor:`, with an optional scope: `fix(calibration): …`.

## Repository layout

| Path | What lives there |
| ---- | ---- |
| `crates/athenaeum-core/` | All non-IPC logic: DB, FITS parsing, calibration, scanner, archive, export, plate solving, sync |
| `crates/athenaeum-tauri/` | Desktop shell; `commands/` thinly wraps core |
| `crates/athenaeum-web/` | Axum HTTP/SSE server; `routes/` mirrors the Tauri commands |
| `crates/perseus/` | Capture-agent CLI for observatory machines |
| `rustafits/` | Submodule — FITS/XISF image rendering |
| `solvemyastro/` | Submodule — plate solver |
| `src/` | React/TypeScript frontend |
| `docs/superpowers/specs/` | Design documents for the larger subsystems |

`CLAUDE.md` at the repository root is the long-form architecture reference —
subsystem by subsystem, with the invariants. It is written as guidance for an
agent working in the repository, but it is the most complete description of how
the system fits together and is worth reading before a substantial change.

## Reporting bugs

Use the bug report template. Attaching the JSONL log makes triage far faster —
Settings → Logging shows where the log directory is.
