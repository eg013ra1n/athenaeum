# Windows: closing the 39 test failures and the CI gap that hid them

**Date:** 2026-09-07
**Status:** approved — ready to plan
**Scope:** `athenaeum-core` test fixtures, `sync::ingest` path handling, `.github/workflows/ci.yml`, repository `.gitattributes`

## 1. Problem

A Windows run on 2026-09-06 found **39 failing tests** in `athenaeum-core`, all
pre-existing on `main` and proven so by a baseline diff. Root-cause families,
file:line attribution and a suggested order are in
`research/2026-09-06-windows-test-failures.md`; this document does not repeat
them, it decides what to do about them.

Two facts frame the work.

**Nothing would have caught these.** `.gitlab-ci.yml` contains `cargo test` zero
times — its `build:windows` job is `only: tags` and builds the bundle without
testing — and `.github/workflows/ci.yml` runs on `ubuntu-latest`. No job on any
trigger has ever executed this suite on Windows. The failures are therefore not
a regression from any particular change; they are the accumulated cost of an
unmeasured platform.

**macOS is clean.** Measured 2026-09-07 on this machine (Darwin 25.5.0, arm64),
`cargo test -p athenaeum-core --lib`: **1738 passed, 0 failed, 12 ignored**, 41.9 s.
The research document listed macOS as a second unchecked platform (§5); it is now
checked, and it is not affected. The work below is Windows-only.

## 2. The mechanism, traced end to end

The research document grouped family A as "production-side shaped, not test-side".
Reading the code says otherwise for the two tests it names, and the distinction
decides how the whole cycle is shaped, so it is worth stating exactly.

`validate_transfer_dir_tolerates_an_offline_scan_root` (`api/sync.rs:11599`)
seeds a scan root with

```rust
// Seed the root in canonical form (what `add_scan_root` persists) and
// never create it on disk — that is an unplugged drive.
let base = tmp.path().canonicalize().unwrap();
```

but `add_scan_root` does not persist that. It persists
`normalize_path(canonicalize(...))` — and `normalize_path`
(`api/scan_roots.rs:58`) exists precisely to strip the `\\?\` verbatim prefix
Windows `canonicalize` adds. So on Windows the fixture stores `\\?\C:\…` where
production would have stored `C:\…`. The test's own comment describes the right
behaviour; the test's code does not follow it.

The two `validate_transfer_dir` assertions in that test still pass, because
production normalises both sides: `validate_transfer_dir:235` normalises its
argument and `check_scan_root_overlap:340` defensively normalises the row it read.
What fails is the last loop, where the raw `\\?\`-spelled path is passed as the
**argument** to `check_scan_root_overlap` and no longer equals the normalised
stored string.

So the bug is in the fixture, and the production rule it violates is one that
already holds everywhere it was checked. That is the finding this cycle is built
on.

The same fixture shape explains a failure the research document filed separately.
`overview_counts_archived_sets_and_distinct_zip_bytes` (`api/scan_roots.rs:2849`)
fails with `Os { code: 123, kind: InvalidFilename }`, which the document read as
"a path literal that simply cannot exist on Windows". It is narrower than that:
the fixture takes the raw `canonicalize()` string and then builds children with
`format!("{arc}/lights.zip")`. A `\\?\` verbatim path is handed to the kernel
without any normalisation, so a forward slash in it is a literal filename
character rather than a separator — which is what error 123 is saying. Strip the
verbatim prefix and the same forward slash is accepted again. So this is not a
third root cause but the second consequence of the first one, and it belongs with
the fixture work rather than with the pure-text edits.

## 3. The invariant

The rule exists in the code and is nowhere written down, which is why fixtures
drift from it. This cycle writes it down and pins it with a test.

1. **Normalise at the write boundary.** Any path that reaches the database or the
   settings table passes through `normalize_path(canonicalize(...))`. Today:
   `add_scan_root`, `validate_transfer_dir` (`api/sync.rs:235`),
   `resolve_archive_root` (`archive/root.rs:56`).
2. **Stored paths are normalised.** A comparison helper may assume it. The
   defensive `normalize_path` already inside `check_scan_root_overlap` stays as
   belt-and-braces for catalogs written by older builds; it does not license new
   code to store un-normalised paths.
3. **Wire strings are POSIX on the wire, native on the filesystem.** A manifest
   `rel_path` arrives with forward slashes. It must be converted to native
   separators before it is joined onto a filesystem path or written into the
   catalog.
4. **Fixtures obey rules 1–3.** A test that seeds the database must seed what
   production would have written.

Rule 3 is the one with a live production consequence, and it is why family F is
not a test-text problem — see §4, D3.

## 4. Decisions

**D1 — Production changes are limited to violations of §3.** The alternatives
considered were (a) teaching every comparison helper to tolerate a `\\?\`-spelled
row, and (b) a one-time migration rewriting `\\?\` rows in `scan_roots` / `files`.
Both were rejected. (a) dissolves rule 2: "stored paths are normalised" stops
being true and every future comparison site has to remember it. (b) is a
migration across the hottest table in the catalog that cannot be rehearsed —
there is no Windows beta catalog on hand to test it against. Catalogs written by
older Windows betas that hold `\\?\`-spelled rows remain what
`open-items.md` already says they are: a manual re-add or relink.

**D2 — The Windows CI job lands first and starts non-blocking.** Task 1 adds a
`windows-latest` job to `.github/workflows/ci.yml` with `continue-on-error: true`.
It is red from the first run, on purpose: it is the independent measurement each
subsequent task moves. The last task of the cycle removes `continue-on-error`,
which is the moment green-on-Windows becomes a gate. The repository is public on
GitHub, so the runner minutes cost nothing.

**D3 — The job runs the same command as the Linux job.** Not
`-p athenaeum-core --lib`, which is only what the 2026-09-06 run happened to
measure. Two jobs with different commands drift, and the crates the narrower
command omits — `athenaeum-tauri`, `athenaeum-web`, `perseus` — have the same
zero Windows coverage and may hold failures of their own. **The consequence is
that the real number is not known until task 1 reports it, and it may exceed 39.**
The plan therefore carries an explicit scope checkpoint after task 1 rather than
silently absorbing whatever appears.

**D4 — One shared fixture helper, guarded against drift.** `athenaeum-core` has
29 `canonicalize().unwrap()` sites — in `api/sync.rs`, `api/scan_roots.rs`,
`api/files.rs` and `archive/root.rs`, every one of them below that file's
`#[cfg(test)]` line — and no shared test-support module. Fixing
roughly seventeen of them by hand fixes today's failures and leaves the next
fixture free to repeat the mistake, discoverable only by a Windows runner. The
cycle introduces one helper that yields a canonical, normalised temporary
directory, moves the affected fixtures onto it, and adds a drift-guard test that
fails when a fixture seeds a raw `canonicalize()` result. The repository already
uses this genre — `every_terminal_writer_announces_or_is_a_named_exemption`
(`sync/receiver.rs:5988`) is one, and it is itself one of the 39.

**D5 — Family D is diagnosed before it is fixed.** Six `QueryReturnedNoRows`
failures sit on the non-destructive in-place rescan path — the path that preserves
`files.id` / `frames.id` so junction rows survive an edit. A path-keyed lookup
returning nothing is also exactly what a genuine separator or spelling bug looks
like once it reaches SQLite. The working hypothesis is that `%TEMP%` on Windows is
frequently handed out in 8.3 short form while `canonicalize()` expands it, so
`tmp.path()` and `tmp.path().canonicalize()` differ by more than the prefix. That
is a hypothesis, not a finding. The first step of that task prints both spellings
on the Windows machine; the fix follows the evidence. If the production side turns
out to be wrong, that is a scope change and gets raised, not absorbed.

**D6 — `.gitattributes` is its own commit.** The drift guard in family G splits
`include_str!("receiver.rs")` on a literal `"\n#[cfg(test)]\nmod tests"`, which
never matches under git's Windows default `core.autocrlf=true`, so the guard
silently counts the test module too (26 instead of 12) — it cannot do its job on
any Windows checkout. The narrow fix is a CRLF-tolerant split. The broader one,
which the repository should have anyway given how much of this suite compares
paths and text, is pinning line endings in `.gitattributes`. Both are done; the
`.gitattributes` change is committed separately because it is the only change in
this cycle that every contributor's working copy feels, and a separate commit is
one that can be reverted on its own.

## 5. Work breakdown

Ordered so that each step is measured by the step before it. Test counts are from
the 2026-09-06 run and are expected to be restated after task 1.

| # | Work | Side | Tests |
| ---- | ---- | ---- | ---- |
| T1 | `windows-latest` job in `ci.yml`, `continue-on-error: true`, same command as Linux. Report the real workspace-wide failure count. **Scope checkpoint.** | CI | — |
| T2 | Family B: `#[cfg(windows)]` branch with `FILE_FLAG_BACKUP_SEMANTICS` in the `set_mtime_ago` test helper (`api/sync.rs:7157`) | test | 5 |
| T3 | Family G: CRLF-tolerant split; `.gitattributes` as a separate commit | test + repo | 1 |
| T4 | Family C: hardcoded forward slashes in `calibration_library/paths.rs` expectations | test | 2 |
| T5 | D4's shared fixture helper; move families A and E onto it — including the E straggler at `scan_roots.rs:2849`, see §2 — and add the drift guard | test | ~18 |
| T6 | Family D: diagnose first (D5), then fix the side the evidence names | ? | 6 |
| T7 | Family F: native separators for `rel_path` before the join in `sync/ingest.rs:780` and before the catalog write; includes the escape-the-root guard | production | 4 |
| T8 | Family H: likely the tail of D; confirm or attribute separately | ? | 1 |
| T9 | Remove `continue-on-error`; update `open-items.md` and this spec with what was found | CI + docs | — |

The family counts do not add up to 39, and the plan should not pretend they do.
Summed, they are 37 — and family E's bracket says 16 while its own enumeration
lists 15 (eleven, three, one), so the honest range is 36–37 attributed against a
headline of 39. Two or three failures are therefore either unattributed or
double-counted in `research/2026-09-06-windows-test-failures.md`. This is not a
reason to distrust the research — the headline came from the test runner and the
families came from reading the output — but it does mean **T1's measurement is
the reconciliation**, and no task may be closed on "the family is done" alone.
The count the runner prints is the only number that closes anything.

T2 is first among the fixes because it is five tests behind one flag with no
design question — the research document's own ordering, and it stays.

## 6. Verification

- The Windows machine runs the suite after each task; the failure count is the
  measurement, and it only ever goes down.
- macOS and Linux stay green throughout. The macOS baseline is §1's 1738/0; the
  Linux baseline is the existing CI job.
- The invariant of §3 is held after the cycle by the drift guard from D4, not by
  anyone's memory.
- T7 is verified by more than its four tests: a landed file must not reach the
  catalog with a mixed separator, because the scanner walks the same tree with
  `WalkDir` and would spell it with backslashes, producing a second row for one
  file.

## 7. Risks and accepted consequences

- **The scope is not fully known at planning time.** D3 makes that explicit rather
  than hiding it; T1's checkpoint is where it is resolved.
- **Family D may be a production bug.** If it is, the non-destructive rescan
  orphans junction rows on Windows, which is a user-visible catalog fault and a
  larger piece of work than this cycle assumes. D5 makes the diagnosis a gate.
- **`.gitattributes` renormalises working copies.** The next checkout on Windows
  re-evaluates line endings across the tree, possibly needing
  `git add --renormalize`. D6 isolates it in one commit for that reason.
- **A `windows-latest` job adds runner time to every PR.** Free on a public
  repository, but it lengthens the wall-clock wait for a green tick. The Linux job
  currently takes ~570 s; a Windows run of the same command should be assumed
  slower until measured.
- **Catalogs from older Windows betas are not healed.** D1, and consistent with
  what `open-items.md` already records.

## 8. Out of scope

- Healing `\\?\`-spelled rows in existing catalogs (D1).
- macOS — measured clean in §1.
- Any change to the file-move hot path's deliberate absence of `canonicalize`
  (`CLAUDE.md`, Dual-Pane hot-sync semantics). Nothing here touches it, and
  nothing here is a reason to revisit it.

## 9. Release-note lines owed

Only T7 changes behaviour a user can see; the rest restores a test suite.

- On Windows, files received from another device now land with native path
  separators, so a received file is no longer at risk of being catalogued twice
  once the folder is scanned.
