# Windows Test-Failure Cycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Take `athenaeum-core` from 39 failing tests on Windows to zero, and add the CI job that keeps it there.

**Architecture:** Most failures are test fixtures that seed the database with a spelling production would never write — a raw `canonicalize()` result, which on Windows carries the `\\?\` verbatim prefix. The cycle writes the normalisation rule down (spec §3), moves those fixtures onto one shared helper, and pins the rule with a drift-guard test. Two families are genuine production defects or unknowns and are handled separately: wire `rel_path` joined without native separators (T7), and six in-place-rescan lookups that are diagnosed before anything is changed (T6). A `windows-latest` CI job lands first, red and non-blocking, and becomes the gate in the last task.

**Tech Stack:** Rust (`athenaeum-core`, `rustc` pinned by `rust-toolchain.toml`), GitHub Actions, git attributes.

**Spec:** `docs/superpowers/specs/2026-09-07-windows-test-failures-design.md`

## Global Constraints

- **Windows verification runs on the `Pull the main` session** (Remote Control, id `51312d`). It holds a checkout of this repository on the Windows machine. Every task that claims a test now passes on Windows must have that session's output pasted into the task before the task is closed.
- **The measurement command on Windows is** `cargo test -p athenaeum-core --lib` for per-task checks, and the workspace pair from T1 for the CI-equivalent number.
- **The count the runner prints is the only number that closes a task.** The research document's family counts sum to 36–37 against a headline of 39 (spec §5), so "the family is done" is not an acceptance criterion.
- **macOS and Linux must stay green.** The macOS baseline measured 2026-09-07 is `cargo test -p athenaeum-core --lib` → 1738 passed, 0 failed, 12 ignored. Run it locally before every commit.
- **No Tauri command or Axum route is added or changed by this cycle**, so the two-backends-in-sync rule (CLAUDE.md) does not apply to any task here. If a task appears to need one, stop — that is a scope change.
- **Zero-print rule:** `println!` / `eprintln!` are forbidden in production code and permitted only under `#[cfg(test)]`. Every diagnostic in this plan is inside a test.
- **Formatting:** run `rustfmt <the files you touched>`, never `cargo fmt -p` (repo gotcha).
- **Commits are authored as the user.** Every commit message ends with:
  ```
  Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01KtRKsr5SfMysik5qCtRjLw
  ```
- **Work happens on a branch** (`windows-test-failures`), created via the `superpowers:using-git-worktrees` skill at execution time. `main` is the trunk; how the branch lands on it is the owner's call at T9.

---

## File Structure

| File | Change | Responsibility |
| ---- | ---- | ---- |
| `.github/workflows/ci.yml` | modify | Gains a `windows-latest` job (T1), which loses `continue-on-error` in T9 |
| `.gitattributes` | create | Pins `*.rs` to LF so `include_str!`-based guards can split on `\n` (T3) |
| `crates/athenaeum-core/src/lib.rs` | modify | Declares the new test-only `test_support` module (T5) |
| `crates/athenaeum-core/src/test_support.rs` | create | The one canonical-tempdir fixture helper, its drift guard, and the Windows path diagnostic (T5, T6) |
| `crates/athenaeum-core/src/api/sync.rs` | modify | `set_mtime_ago` opens directories on Windows (T2); fixtures move to the helper (T5) |
| `crates/athenaeum-core/src/api/scan_roots.rs` | modify | Fixtures move to the helper, including the error-123 straggler (T5) |
| `crates/athenaeum-core/src/archive/root.rs` | modify | Fixture moves to the helper (T5) |
| `crates/athenaeum-core/src/sync/receiver.rs` | modify | Drift guard splits CRLF-tolerantly (T3) |
| `crates/athenaeum-core/src/calibration_library/paths.rs` | modify | Path expectations built with `join`, not forward-slash literals (T4) |
| `crates/athenaeum-core/src/sync/ingest.rs` | modify | Wire `rel_path` converted to native separators before any join (T7) |
| `crates/athenaeum-core/src/scanner/mod.rs` | modify | Family D, after diagnosis (T6) |
| `docs/superpowers/open-items.md` | modify | The Windows section is rewritten to what is left (T9) |

---

## Task 1: The Windows CI job, red and non-blocking

**Files:**
- Modify: `.github/workflows/ci.yml` (add a second job after `build-and-test`)

**Interfaces:**
- Produces: a job named `build-and-test-windows`. T9 removes its `continue-on-error`.

**How this job actually gets to run.** The workflow triggers on `pull_request` and on `push` to `main` or a tag — a push to a working branch triggers nothing. So step 4 opens a **draft pull request**, purely as the vehicle that makes the job run on every push of this branch. It is not a review gate, and it is not how the work lands: `main` is the trunk and the owner decides the landing at T9. The fast per-task loop stays the `Pull the main` machine — it answers in minutes where CI answers in tens of them; CI is the second opinion and, from T9 on, the permanent fence.

- [ ] **Step 1: Add the job**

Append to `.github/workflows/ci.yml`, at the same indentation as `build-and-test:` (two spaces, under `jobs:`):

```yaml
  # The Rust suite has only ever run on Linux: this workflow was ubuntu-only and
  # .gitlab-ci.yml contains `cargo test` zero times (its build:windows job is
  # `only: tags` and builds the bundle without testing). A Windows run on
  # 2026-09-06 found 39 pre-existing failures behind that gap. This job is the
  # measurement that keeps them closed.
  #
  # `continue-on-error` is deliberate and TEMPORARY: the job is red on the day it
  # lands and turns green over the course of the cycle in
  # docs/superpowers/plans/2026-09-07-windows-test-failures.md, whose last task
  # removes this line and makes Windows a gate.
  build-and-test-windows:
    name: Build and test (Windows)
    runs-on: windows-latest
    continue-on-error: true
    timeout-minutes: 60
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive

      # No system-dependency step: the MSVC toolchain and the WebView2 runtime
      # Tauri needs are already on the windows-latest image. rustafits has needed
      # no nasm/cmake since it moved to a pure-Rust JPEG encoder (2026-07-29).
      - name: Resolve the pinned Rust toolchain
        run: |
          rustup show active-toolchain
          rustc --version
          cargo --version

      - uses: Swatinem/rust-cache@v2

      # The command MUST stay identical to the Linux job's. Two jobs running
      # different commands drift, and the whole point of this one is that it
      # measures the same thing on another platform.
      - name: Compile workspace and tests
        run: cargo test --workspace --no-run

      - name: Test workspace
        run: cargo test --workspace -- --skip unclean_shutdown_mid_transfer_resumes_on_restart
```

Note the frontend typecheck is intentionally absent: `npx tsc --noEmit` is platform-independent and the Linux job already runs it.

- [ ] **Step 2: Verify the workflow file still parses**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml')); print('ok')"`
Expected: `ok`

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "$(cat <<'MSG'
ci: run the workspace suite on windows-latest, non-blocking for now

The Rust suite has only ever run on Linux, which is why 39 pre-existing
Windows failures went unmeasured. The job runs the Linux job's exact
command so the two cannot drift. It is red today and carries
continue-on-error until the cycle closes it.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KtRKsr5SfMysik5qCtRjLw
MSG
)"
```

- [ ] **Step 4: Push the branch and open the draft PR**

```bash
git push -u github windows-test-failures
gh pr create --draft --repo eg013ra1n/athenaeum \
  --base main --head windows-test-failures \
  --title "Windows: close the 39 test failures and the CI gap that hid them" \
  --body "CI vehicle for the cycle in docs/superpowers/plans/2026-09-07-windows-test-failures.md. Draft on purpose: it exists so the new windows-latest job runs on every push. Landing on main is the owner's call at the end of the cycle."
```

Then confirm the job appears: `gh run list --branch windows-test-failures --limit 3`.
Expected: a run with both `Build, test and typecheck` and `Build and test (Windows)`. The Windows one is red; that is the point.

- [ ] **Step 5: Get the real number from the Windows machine**

Send to the `Pull the main` session (SendMessage, `to: "Pull the main"`):

> Run in the athenaeum checkout: `git fetch --all && git checkout windows-test-failures && git pull && git submodule update --init --recursive`, then `cargo test --workspace --no-run` followed by `cargo test --workspace -- --skip unclean_shutdown_mid_transfer_resumes_on_restart`. Paste the final `test result:` line of every crate that reports failures, plus the full sorted list of failed test names. Do not change any file.

Expected: a count for `athenaeum-core` near 39, plus counts for `athenaeum-tauri`, `athenaeum-web` and `perseus` that nobody has ever seen.

- [ ] **Step 6: SCOPE CHECKPOINT — stop and report**

Write the numbers into this plan under Task 1 and report to the owner before starting Task 2:
- the `athenaeum-core` count, and how it reconciles with the research document's 39;
- any failures outside `athenaeum-core`, with a recommendation on whether they belong in this cycle or a follow-up.

Do not silently absorb failures in the other crates. Spec D3 makes this checkpoint a requirement.

---

## Task 2: Family B — open a directory handle on Windows

Five tests fail at one call site with `open <dir> for mtime set: Access is denied. (os error 5)`. `set_mtime_ago` is a **test helper**, so this is entirely test-side.

**Files:**
- Modify: `crates/athenaeum-core/src/api/sync.rs:7157` (`set_mtime_ago`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Read the current helper**

Run: `sed -n '7153,7165p' crates/athenaeum-core/src/api/sync.rs`
Expected: a `set_mtime_ago` that calls `std::fs::File::open(path)` and then `set_modified`.

- [ ] **Step 2: Replace it**

Replace the body of `set_mtime_ago` with:

```rust
    /// Test helper: force `path`'s OWN mtime (no recursion) to `age` in the past.
    ///
    /// Windows cannot open a directory through the ordinary path — that needs
    /// `FILE_FLAG_BACKUP_SEMANTICS` — and `set_modified` calls `SetFileTime`,
    /// which needs `FILE_WRITE_ATTRIBUTES`; `File::open`'s `GENERIC_READ` grants
    /// neither. On unix `File::open` is enough, because `futimens` keys on
    /// ownership rather than on the open mode.
    fn set_mtime_ago(path: &Path, age: Duration) {
        #[cfg(windows)]
        let f = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_READ_ATTRIBUTES: u32 = 0x0080;
            const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            std::fs::OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)
                .unwrap_or_else(|e| panic!("open {} for mtime set: {e}", path.display()))
        };
        #[cfg(not(windows))]
        let f = std::fs::File::open(path)
            .unwrap_or_else(|e| panic!("open {} for mtime set: {e}", path.display()));

        f.set_modified(SystemTime::now() - age)
            .expect("set_modified");
    }
```

- [ ] **Step 3: Verify macOS is unaffected**

Run: `cargo test -p athenaeum-core --lib api::sync::tests::sweep 2>&1 | tail -5`
Expected: PASS — the `#[cfg(not(windows))]` branch is byte-for-byte the old code.

- [ ] **Step 4: Format and commit**

```bash
rustfmt crates/athenaeum-core/src/api/sync.rs
git add crates/athenaeum-core/src/api/sync.rs
git commit -m "$(cat <<'MSG'
test(sync): open directories for the mtime helper on Windows

Windows needs FILE_FLAG_BACKUP_SEMANTICS to open a directory handle at
all, and FILE_WRITE_ATTRIBUTES for SetFileTime; File::open grants
neither, so five orphan-sweep tests failed at the helper with "Access is
denied". The unix branch is unchanged.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KtRKsr5SfMysik5qCtRjLw
MSG
)"
```

- [ ] **Step 5: Verify on Windows**

Send to `Pull the main`:

> `git pull` in the athenaeum checkout, then `cargo test -p athenaeum-core --lib 2>&1 | tail -30`. Paste the `test result:` line and the list of remaining failed test names.

Expected: the five `sweep_*` / `leftovers_*` tests named in research family B are gone from the list, and the total drops by five. If `set_modified` now fails where `open` used to, the access mask is the remaining gap — say so rather than guessing at another flag.

---

## Task 3: Family G — a drift guard that CRLF cannot disarm, and `.gitattributes`

`every_terminal_writer_announces_or_is_a_named_exemption` splits its own source on the literal `"\n#[cfg(test)]\nmod tests"`. Under git's Windows default `core.autocrlf=true` the source carries CRLF, the split never matches, and the guard counts the test module too — 26 instead of 12. It cannot do its job on any Windows checkout.

**Files:**
- Modify: `crates/athenaeum-core/src/sync/receiver.rs:5988` (`write_sites`, the inner fn)
- Create: `.gitattributes`

- [ ] **Step 1: Replace the split with a line scan**

In `every_terminal_writer_announces_or_is_a_named_exemption`, replace the inner `write_sites` with:

```rust
        /// Count state-write call sites in a file's PRODUCTION half, ignoring
        /// comment lines (so prose mentioning the function never moves the number).
        ///
        /// Line-based rather than a split on a `"\n…"` literal: with
        /// `core.autocrlf=true` — git's Windows default — the embedded source
        /// carries CRLF, such a literal never matches, and the guard silently
        /// counts its own test module. `str::lines` strips the `\r`, so this is
        /// true on either checkout. `.gitattributes` pins the endings as well;
        /// this does not depend on that.
        fn write_sites(src: &str) -> usize {
            let mut count = 0usize;
            let mut lines = src.lines().peekable();
            while let Some(line) = lines.next() {
                if line.trim_end() == "#[cfg(test)]"
                    && lines
                        .peek()
                        .is_some_and(|next| next.trim_start().starts_with("mod tests"))
                {
                    break;
                }
                let t = line.trim_start();
                if !t.starts_with("//") && t.contains("set_inbound_state(") {
                    count += 1;
                }
            }
            count
        }
```

- [ ] **Step 2: Run it on macOS to prove the count is unchanged**

Run: `cargo test -p athenaeum-core --lib every_terminal_writer_announces_or_is_a_named_exemption -- --exact 2>&1 | tail -5`
Expected: PASS with the same expected value of 12. A failure here means the rewrite changed what is counted, not what platform it runs on — fix the scan, do not touch the 12.

- [ ] **Step 3: Commit the code fix on its own**

```bash
rustfmt crates/athenaeum-core/src/sync/receiver.rs
git add crates/athenaeum-core/src/sync/receiver.rs
git commit -m "$(cat <<'MSG'
test(sync): make the terminal-writer guard survive CRLF checkouts

The guard split its own source on a "\n#[cfg(test)]\nmod tests" literal,
which never matches under core.autocrlf=true — so on Windows it counted
its own test module (26 against an expected 12) and guarded nothing. A
line scan is true on either checkout.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KtRKsr5SfMysik5qCtRjLw
MSG
)"
```

- [ ] **Step 4: Add `.gitattributes` as a separate commit**

Create `.gitattributes`:

```gitattributes
# Line endings are pinned, not left to core.autocrlf.
#
# Several tests read their own source with include_str! and reason about it
# line by line; one of them (the terminal-writer guard in sync/receiver.rs)
# silently guarded nothing on Windows because the checkout carried CRLF. Much
# of this suite also compares paths and text, where a stray \r is a very quiet
# way to be wrong.
#
# There are no .bat/.cmd/.ps1 files in this repository, so nothing here wants
# CRLF in the working tree.
* text=auto
*.rs text eol=lf
*.toml text eol=lf
*.md text eol=lf
*.yml text eol=lf
*.yaml text eol=lf
*.json text eol=lf
*.ts text eol=lf
*.tsx text eol=lf
*.css text eol=lf
*.html text eol=lf
*.sh text eol=lf
```

Then:

```bash
git add .gitattributes
git commit -m "$(cat <<'MSG'
chore: pin line endings in .gitattributes

Kept a separate commit on purpose: this is the only change in the Windows
cycle that every contributor's working copy feels — the next checkout on
Windows re-evaluates line endings across the tree, and it may want
`git add --renormalize .`. A separate commit is one that can be reverted
on its own.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KtRKsr5SfMysik5qCtRjLw
MSG
)"
```

- [ ] **Step 5: Confirm the working tree did not churn on this machine**

Run: `git status --short`
Expected: clean. macOS checkouts are already LF, so pinning changes nothing here. If files do appear as modified, run `git add --renormalize .` and amend the `.gitattributes` commit.

- [ ] **Step 6: Verify on Windows**

Send to `Pull the main`:

> `git pull` in the athenaeum checkout, then `git add --renormalize .` — paste `git status --short` afterwards, and say whether anything was renormalised. Then `cargo test -p athenaeum-core --lib 2>&1 | tail -30` and paste the `test result:` line plus the remaining failures.

Expected: `every_terminal_writer_announces_or_is_a_named_exemption` passes. If the renormalise produced staged changes, that is expected once — commit them as `chore: renormalise line endings after .gitattributes` and say so.

---

## Task 4: Family C — path expectations built with `join`

Two tests in `calibration_library/paths.rs` compare `p.to_string_lossy()` against a forward-slash literal. The production code builds a correct native path; the assertion carries a POSIX one.

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/paths.rs:209`, `:227`

- [ ] **Step 1: Replace the `dark_path_shape` assertion**

Replace:

```rust
        assert_eq!(
            p.to_string_lossy(),
            "ZWO_ASI2600MM_Pro/MasterDark/master_dark_300s_-10C_g100_bin1x1_2026-06-28.fits"
        );
```

with:

```rust
        // Compared as a path, not as a string: `master_relative_path` joins
        // components, so the separator is the platform's and a forward-slash
        // literal is only right on unix.
        assert_eq!(
            p,
            std::path::Path::new("ZWO_ASI2600MM_Pro")
                .join("MasterDark")
                .join("master_dark_300s_-10C_g100_bin1x1_2026-06-28.fits")
        );
```

- [ ] **Step 2: Replace the `flat_includes_filter_and_missing_fields_collapse` assertion**

Replace:

```rust
        assert_eq!(
            p.to_string_lossy(),
            "cam/MasterFlat/master_flat_Ha_1.55s_2026-07-01.fits"
        );
```

with:

```rust
        assert_eq!(
            p,
            std::path::Path::new("cam")
                .join("MasterFlat")
                .join("master_flat_Ha_1.55s_2026-07-01.fits")
        );
```

- [ ] **Step 3: Check for siblings the research document did not list**

Run: `grep -n 'to_string_lossy(),' crates/athenaeum-core/src/calibration_library/paths.rs`
Expected: no remaining assertion in this file compares a path to a literal containing `/`. Fix any that do, the same way — the two named tests are the two that fail today, but a sibling that happens to assert a single-component path would fail the moment it grew one.

- [ ] **Step 4: Run and commit**

Run: `cargo test -p athenaeum-core --lib calibration_library::paths 2>&1 | tail -5`
Expected: PASS.

```bash
rustfmt crates/athenaeum-core/src/calibration_library/paths.rs
git add crates/athenaeum-core/src/calibration_library/paths.rs
git commit -m "$(cat <<'MSG'
test(calibration-library): compare master paths as paths, not as strings

The production code built a correct Windows path and the assertion
carried a POSIX one.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KtRKsr5SfMysik5qCtRjLw
MSG
)"
```

---

## Task 5: Families A and E — one fixture helper, and a guard so it stays used

The largest family. Fixtures seed the database with `tmp.path().canonicalize().unwrap()`, which on Windows is `\\?\C:\…` — a spelling `add_scan_root` would never persist, and one that also rejects a forward slash as a separator (spec §2). All 29 such sites in the crate sit below their file's `#[cfg(test)]` line.

**Files:**
- Create: `crates/athenaeum-core/src/test_support.rs`
- Modify: `crates/athenaeum-core/src/lib.rs`
- Modify: `crates/athenaeum-core/src/api/sync.rs`, `crates/athenaeum-core/src/api/scan_roots.rs`, `crates/athenaeum-core/src/api/files.rs`, `crates/athenaeum-core/src/archive/root.rs`

**Interfaces:**
- Produces: `crate::test_support::canonical_tempdir() -> (tempfile::TempDir, std::path::PathBuf)`. The `TempDir` must be bound, not dropped — dropping it deletes the directory. Task 6 adds a second function to the same file.

- [ ] **Step 1: Write the guard first — it is the failing test for this task**

Create `crates/athenaeum-core/src/test_support.rs`:

```rust
//! Shared test fixtures.
//!
//! Path fixtures in particular. A test that seeds the catalog must seed what
//! production would have written, and production normalises at the write
//! boundary — `normalize_path(canonicalize(...))`, see the invariant in
//! `docs/superpowers/specs/2026-09-07-windows-test-failures-design.md` §3.
//! `canonicalize` alone returns a `\\?\` verbatim path on Windows, which neither
//! compares equal to the stored spelling nor accepts a forward slash as a
//! separator.

use std::path::PathBuf;

/// A temporary directory, plus the spelling production would have stored for it.
///
/// Bind the `TempDir`: dropping it deletes the directory out from under the path.
///
/// ```ignore
/// let (_tmp, base) = crate::test_support::canonical_tempdir();
/// ```
pub(crate) fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize tempdir");
    let base = crate::api::scan_roots::normalize_path(&canonical);
    (dir, base)
}

/// The spelling production would have stored for an existing directory.
///
/// For a test that already holds a `TempDir` — from `test_ctx()`, say — and must
/// not create a second one.
pub(crate) fn canonical_path(path: &std::path::Path) -> PathBuf {
    let canonical = path.canonicalize().expect("canonicalize path");
    crate::api::scan_roots::normalize_path(&canonical)
}

/// No fixture may seed a raw `canonicalize()` result.
///
/// This is a drift guard in the same genre as
/// `sync::receiver::tests::every_terminal_writer_announces_or_is_a_named_exemption`.
/// Without it the next fixture repeats the mistake and only a Windows runner
/// notices — which is exactly how 39 tests accumulated unmeasured.
#[test]
fn fixtures_never_seed_a_raw_canonicalized_path() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();

    for entry in walkdir::WalkDir::new(&src).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // This file defines the sanctioned helper, so it necessarily contains
        // the call the guard forbids everywhere else.
        if path.file_name().and_then(|n| n.to_str()) == Some("test_support.rs") {
            continue;
        }
        let text = std::fs::read_to_string(path).expect("read source");
        // Drop comment lines so prose about the rule never trips it, then drop
        // all whitespace so a multi-line builder chain cannot hide.
        let squashed: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .flat_map(|l| l.chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        if squashed.contains("canonicalize().unwrap()")
            || squashed.contains("canonicalize().expect(")
        {
            offenders.push(
                path.strip_prefix(&src)
                    .unwrap_or(path)
                    .display()
                    .to_string(),
            );
        }
    }

    assert!(
        offenders.is_empty(),
        "these files seed a raw `canonicalize()` result: {offenders:?}\n\
         On Windows that is a `\\\\?\\` verbatim path. It never compares equal to \
         the normalised spelling production stores, and it takes a forward slash \
         as a filename character rather than a separator (error 123, \
         InvalidFilename). Use `crate::test_support::canonical_tempdir()`."
    );
}
```

- [ ] **Step 2: Declare the module**

In `crates/athenaeum-core/src/lib.rs`, immediately after `pub mod api;`, add:

```rust
// Shared test fixtures. Test-only — nothing outside `#[cfg(test)]` may use it.
#[cfg(test)]
pub(crate) mod test_support;
```

- [ ] **Step 3: Run the guard and watch it fail**

Run: `cargo test -p athenaeum-core --lib fixtures_never_seed_a_raw_canonicalized_path -- --exact 2>&1 | tail -20`
Expected: FAIL, listing `api/sync.rs`, `api/scan_roots.rs`, `api/files.rs`, `archive/root.rs`.

- [ ] **Step 4: Convert the fixtures, file by file**

For each offender, replace every

```rust
        let base = tmp.path().canonicalize().unwrap();
```

(and its variants — `let dir = tempfile::tempdir().unwrap(); … dir.path().canonicalize().unwrap()`) with the helper:

```rust
        let (_tmp, base) = crate::test_support::canonical_tempdir();
```

Three things to get right while converting:

1. **Keep the `TempDir` bound.** `let (_tmp, base) = …` — a bare `_` drops it immediately and the directory disappears mid-test.
2. **Some tests already hold a `TempDir` from `test_ctx()`.** Do not introduce a second one — use the other helper from step 1 on the handle they already have:
   ```rust
   let base = crate::test_support::canonical_path(tmp.path());
   ```
3. **`overview_counts_archived_sets_and_distinct_zip_bytes` (`api/scan_roots.rs:2849`) needs a second edit.** After the base is normalised, its children are still built with `format!("{arc}/lights.zip")`. Replace every such `format!` in that test with a join:
   ```rust
   let zip_a = arc.join("lights.zip").to_string_lossy().to_string();
   ```
   where `arc` is now a `PathBuf` rather than a `String`. This is the error-123 failure: a forward slash inside a verbatim path is a filename character. Normalising the base is what makes the slash legal again, but joining is what makes the test say what it means.

- [ ] **Step 5: Run the guard until it passes**

Run: `cargo test -p athenaeum-core --lib fixtures_never_seed_a_raw_canonicalized_path -- --exact 2>&1 | tail -20`
Expected: PASS, with no offenders listed.

- [ ] **Step 6: Run the whole crate on macOS**

Run: `cargo test -p athenaeum-core --lib 2>&1 | tail -5`
Expected: `1739 passed; 0 failed` (the baseline 1738 plus the new guard). Any failure here is a fixture converted wrongly — most likely a dropped `TempDir`.

- [ ] **Step 7: Format and commit**

```bash
rustfmt crates/athenaeum-core/src/test_support.rs crates/athenaeum-core/src/lib.rs \
        crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/api/scan_roots.rs \
        crates/athenaeum-core/src/api/files.rs crates/athenaeum-core/src/archive/root.rs
git add crates/athenaeum-core/src
git commit -m "$(cat <<'MSG'
test: seed path fixtures with the spelling production stores

Fixtures seeded the catalog with a raw canonicalize() result. On Windows
that is a \\?\ verbatim path: it never compares equal to the normalised
spelling add_scan_root persists, and it takes a forward slash as a
filename character rather than a separator — which is the error-123
failure in the archive-overview test as well.

One helper now yields the canonical spelling, and a drift guard fails the
suite if a fixture goes back to the raw call.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KtRKsr5SfMysik5qCtRjLw
MSG
)"
```

- [ ] **Step 8: Verify on Windows**

Send to `Pull the main`:

> `git pull`, then `cargo test -p athenaeum-core --lib 2>&1 | tail -40`. Paste the `test result:` line and every remaining failed test name.

Expected: families A and E are gone — roughly 18 fewer failures. What should remain is families D (6), F (4) and H (1). If any `switch_library_tests` / `candidate_tests` case still fails, it is not a fixture problem and needs its own diagnosis before anything else is changed.

---

## Task 6: Family D — diagnose before touching either side

Six `QueryReturnedNoRows` failures on the non-destructive in-place rescan path — the path that preserves `files.id` / `frames.id` so junction rows survive an edit. A path-keyed lookup returning nothing is also what a genuine separator or spelling bug looks like once it reaches SQLite. Spec D5 makes the diagnosis a gate.

**Files:**
- Modify: `crates/athenaeum-core/src/test_support.rs` (add the diagnostic)
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` — **only after the diagnosis says so**

- [ ] **Step 1: Add the path diagnostic to `test_support.rs`**

```rust
/// Print every spelling of a temporary directory this machine produces.
///
/// Diagnostic, not a check: `#[ignore]`d so it runs only when asked. The working
/// hypothesis for the family-D failures is that `%TEMP%` is handed out in 8.3
/// short form on Windows while `canonicalize` expands it, so `tmp.path()` and
/// its canonical spelling differ by more than the `\\?\` prefix.
#[test]
#[ignore = "diagnostic — run explicitly with --ignored --nocapture"]
fn diagnose_temp_dir_spellings() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize tempdir");
    eprintln!("env::temp_dir()  = {}", std::env::temp_dir().display());
    eprintln!("tempdir.path()   = {}", dir.path().display());
    eprintln!("canonicalize()   = {}", canonical.display());
    eprintln!(
        "normalize_path() = {}",
        crate::api::scan_roots::normalize_path(&canonical).display()
    );
}
```

- [ ] **Step 2: Add a temporary in-test dump to one failing rescan test**

In `crates/athenaeum-core/src/scanner/mod.rs`, inside `rescan_after_mtime_change_preserves_session_members` (`:2156`), immediately before the `SELECT f.id FROM frames f JOIN files fi ON fi.path = ?1` lookup, add:

```rust
        // TEMPORARY DIAGNOSTIC — removed in this task's step 5.
        eprintln!("lookup key = {}", f.to_str().unwrap());
        let stored: Vec<String> = conn
            .prepare("SELECT path FROM files")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();
        eprintln!("stored     = {stored:#?}");
```

- [ ] **Step 3: Commit the diagnostic so the Windows machine can pull it**

```bash
rustfmt crates/athenaeum-core/src/test_support.rs crates/athenaeum-core/src/scanner/mod.rs
git add crates/athenaeum-core/src/test_support.rs crates/athenaeum-core/src/scanner/mod.rs
git commit -m "$(cat <<'MSG'
test(scanner): temporary diagnostic for the Windows rescan lookups

Prints the lookup key against the stored files.path rows so the family-D
failures can be attributed to a side before either side is changed.
Removed in the fix commit.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KtRKsr5SfMysik5qCtRjLw
MSG
)"
```

- [ ] **Step 4: Run the diagnosis on Windows**

Send to `Pull the main`:

> `git pull`, then run both of these and paste the full output of each:
> 1. `cargo test -p athenaeum-core --lib diagnose_temp_dir_spellings -- --ignored --nocapture --exact`
> 2. `cargo test -p athenaeum-core --lib scanner::inplace_tests::rescan_after_mtime_change_preserves_session_members -- --nocapture --exact`

- [ ] **Step 5: Decide from the output, then fix**

Read the two spellings against each other:

- **If `stored` and `lookup key` differ only in how the temp directory is spelled** (8.3 short versus long, or `\\?\` versus plain), the fixture is wrong: convert the test to `crate::test_support::canonical_tempdir()` exactly as Task 5 did, and make the scan root the normalised base so the scanner walks and stores that spelling.
- **If `stored` and `lookup key` are the same string** and the lookup still returns nothing, the failure is not about spelling and the diagnosis is not finished — say so and investigate the query, not the path.
- **If `stored` holds a spelling the scanner produced that production code elsewhere could not match** — for example a mixed separator — that is a production bug on the non-destructive rescan path, meaning Windows rescans orphan junction rows. **Stop and raise it with the owner** (spec §7): it is a user-visible catalog fault and larger than this cycle assumes.

Whichever branch applies, remove the temporary diagnostic from `scanner/mod.rs` in the fix commit. Keep `diagnose_temp_dir_spellings` in `test_support.rs` — it is `#[ignore]`d, costs nothing, and is the first thing anyone will want the next time a Windows path question appears.

- [ ] **Step 6: Verify**

Run locally: `cargo test -p athenaeum-core --lib scanner:: 2>&1 | tail -5` — expected PASS on macOS.

Then send to `Pull the main`:

> `git pull`, then `cargo test -p athenaeum-core --lib 2>&1 | tail -40`. Paste the `test result:` line and the remaining failures.

Expected: the six family-D tests pass. Family H (`scan_repairs_moved_project_contribution`) may pass at the same time — check the list before starting Task 8.

---

## Task 7: Family F — a wire `rel_path` is not a native path

Four tests, one of them the escape-the-root security guard. This is the one production defect in the cycle. `land_payload` joins the forward-slash wire path directly, so on Windows the landed file's path carries a mixed separator — and that path is what goes into `files.path`, while the scanner walking the same tree would spell it with backslashes. One file, two catalog rows.

**Files:**
- Modify: `crates/athenaeum-core/src/sync/ingest.rs:443`, `:780`

**Interfaces:**
- Produces: `fn native_rel_path(rel_path: &str) -> PathBuf`, private to `sync::ingest`.

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod` in `crates/athenaeum-core/src/sync/ingest.rs`:

```rust
    /// A manifest `rel_path` is forward-slash by wire contract. Joining it onto a
    /// base unchanged produces a mixed-separator path on Windows: it works for
    /// I/O, but the string that reaches `files.path` is not the one the scanner
    /// writes for the same file when it walks the tree — a second catalog row.
    #[test]
    fn native_rel_path_uses_platform_separators() {
        assert_eq!(
            super::native_rel_path("sub/dir/x.fits"),
            std::path::Path::new("sub").join("dir").join("x.fits")
        );
        // A flat path is unchanged.
        assert_eq!(
            super::native_rel_path("x.fits"),
            std::path::Path::new("x.fits")
        );
        // `validate_rel_path` guarantees no backslash, no drive letter and no
        // `..`, so splitting on '/' is the whole conversion.
        assert!(crate::package::validate_rel_path("sub/dir/x.fits").is_ok());
        assert!(crate::package::validate_rel_path(r"sub\dir\x.fits").is_err());
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p athenaeum-core --lib native_rel_path_uses_platform_separators -- --exact 2>&1 | tail -10`
Expected: FAIL to compile — `cannot find function 'native_rel_path' in module 'super'`.

- [ ] **Step 3: Add the conversion**

In `crates/athenaeum-core/src/sync/ingest.rs`, above `land_payload`:

```rust
/// Convert a wire `rel_path` into a native relative path.
///
/// The wire contract is forward-slash (`package::validate_rel_path` rejects a
/// backslash, a drive letter and any non-`Normal` component), so splitting on
/// '/' is the whole conversion. Joining the raw string instead is wrong twice on
/// Windows: inside a `\\?\` verbatim base a forward slash is a filename
/// character rather than a separator, and the mixed-separator path that reaches
/// `files.path` does not match the backslash spelling the scanner writes when it
/// walks the same tree — one file, two catalog rows.
fn native_rel_path(rel_path: &str) -> PathBuf {
    rel_path.split('/').filter(|s| !s.is_empty()).collect()
}
```

- [ ] **Step 4: Use it at both join sites**

At `:780`, in `land_payload`:

```rust
    let dest = unique_path(&landing_base.join(native_rel_path(&record.rel_path)));
```

At `:443`, where the payload is read out of the package directory:

```rust
    let payload = package_dir.join(native_rel_path(&record.rel_path));
```

- [ ] **Step 5: Run the test and the ingest suite**

Run: `cargo test -p athenaeum-core --lib sync::ingest 2>&1 | tail -5`
Expected: PASS, including the new test.

- [ ] **Step 6: Format and commit**

```bash
rustfmt crates/athenaeum-core/src/sync/ingest.rs
git add crates/athenaeum-core/src/sync/ingest.rs
git commit -m "$(cat <<'MSG'
fix(sync): land a received file at a native path, not a wire path

A manifest rel_path is forward-slash by contract and was joined onto the
landing base unchanged. On Windows that lands the file at a
mixed-separator path, which is also what reaches files.path — so the
scanner walking the same tree writes a second row for the one file. It
also cannot work at all under a \\?\ base, where a forward slash is a
filename character.

Includes the escape-the-root guard among the tests it restores.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KtRKsr5SfMysik5qCtRjLw
MSG
)"
```

- [ ] **Step 7: Verify on Windows**

Send to `Pull the main`:

> `git pull`, then `cargo test -p athenaeum-core --lib 2>&1 | tail -40`. Paste the `test result:` line and any remaining failures. In particular confirm that `ingest_dot_only_device_name_lands_under_hex_slug_not_above_incoming_root` passes — that is the escape-the-root guard.

---

## Task 8: Family H — attribute the last one

`scanner::calibrated_light_scan_tests::scan_repairs_moved_project_contribution` (`scanner/mod.rs:3300`) panics on `Option::unwrap()` on `None`. The research document guessed it is the tail of family D.

**Files:**
- Modify: `crates/athenaeum-core/src/scanner/mod.rs:3300` — only if it is still failing

- [ ] **Step 1: Check whether Task 6 already fixed it**

Read the failure list from Task 7's Windows run.
Expected: if the test is absent from the list, it was family D. Tick this task, note that in the plan, and go to Task 9 — do not change code that already works.

- [ ] **Step 2: If it still fails, get the panic site**

Send to `Pull the main`:

> `git pull`, then `cargo test -p athenaeum-core --lib scanner::calibrated_light_scan_tests::scan_repairs_moved_project_contribution -- --exact --nocapture 2>&1 | tail -40` with `RUST_BACKTRACE=1` set. Paste everything.

- [ ] **Step 3: Fix on the side the backtrace names**

Apply the same rule as Task 6 step 5: a fixture that seeds a non-production spelling is a fixture fix (`crate::test_support::canonical_tempdir()`); a production lookup that cannot match what production itself wrote is a production fix, and if it is one, say so before changing it.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p athenaeum-core --lib scanner:: 2>&1 | tail -5` — expected PASS on macOS.

```bash
rustfmt crates/athenaeum-core/src/scanner/mod.rs
git add crates/athenaeum-core/src/scanner/mod.rs
git commit -m "$(cat <<'MSG'
test(scanner): fix the last Windows failure in the moved-contribution scan

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KtRKsr5SfMysik5qCtRjLw
MSG
)"
```

---

## Task 9: Make Windows a gate, and write down what was found

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `docs/superpowers/open-items.md`
- Modify: `docs/superpowers/specs/2026-09-07-windows-test-failures-design.md`

- [ ] **Step 1: Confirm Windows is actually green first**

Send to `Pull the main`:

> `git pull`, then `cargo test --workspace --no-run` followed by `cargo test --workspace -- --skip unclean_shutdown_mid_transfer_resumes_on_restart`. Paste every `test result:` line.

Expected: `0 failed` everywhere. Do not proceed on anything less — removing `continue-on-error` while the job is red turns every future pull request red.

- [ ] **Step 2: Remove `continue-on-error` and its explanatory comment**

In `.github/workflows/ci.yml`, delete the line `    continue-on-error: true` from `build-and-test-windows`, and replace the two-paragraph comment above the job with:

```yaml
  # The Rust suite ran on Linux only until 2026-09-07, which is how 39 Windows
  # failures accumulated unnoticed (.gitlab-ci.yml still contains `cargo test`
  # zero times — its build:windows job is `only: tags` and builds the bundle
  # without testing, deliberately unchanged). This job runs the Linux job's
  # exact command so the two cannot drift.
```

- [ ] **Step 3: Rewrite the Windows section of `open-items.md`**

Replace the whole `## Windows: 39 pre-existing test failures, and no CI that would catch them` section with a short one stating: the suite is green on Windows as of the date; `windows-latest` gates every pull request; what the cycle found that was a production defect (the wire `rel_path` join) and what was fixtures; and any check that is still owed by hand. Per that file's own rule, a check that passes is deleted rather than ticked.

- [ ] **Step 4: Close the spec**

Append a `## 10. Outcome` section to `docs/superpowers/specs/2026-09-07-windows-test-failures-design.md` recording:
- the real failure count T1 measured, and how it reconciled with the headline 39 (spec §5 asked for exactly this);
- what family D turned out to be (D5's open question);
- whether any crate outside `athenaeum-core` had failures, and what was decided at the T1 checkpoint.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/ci.yml docs/superpowers/open-items.md \
        docs/superpowers/specs/2026-09-07-windows-test-failures-design.md
git commit -m "$(cat <<'MSG'
ci: make the Windows job a gate, and record what the cycle found

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KtRKsr5SfMysik5qCtRjLw
MSG
)"
```

- [ ] **Step 6: Run the full release-grade gates before handing back**

```bash
cargo build --workspace
cargo test --workspace -- --skip unclean_shutdown_mid_transfer_resumes_on_restart
cargo check -p athenaeum-core --no-default-features
npx tsc --noEmit
```

All four must pass. The headless check is not optional: it is where a feature-gated break hides, and it is exactly what broke the first v0.5.5 tag.

- [ ] **Step 7: Hand back to the owner**

Report: the before/after Windows counts, the one production fix and its release-note line (spec §9), whether `.gitattributes` renormalised anything on the Windows checkout, and how the branch should land on `main`.

The draft pull request from Task 1 is a CI vehicle, not the landing mechanism. Do not merge it unilaterally — ask, then either mark it ready and merge it, or land the branch on `main` directly and close the PR with `gh pr close`.
