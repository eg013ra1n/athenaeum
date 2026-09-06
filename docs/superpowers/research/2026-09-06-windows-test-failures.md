# 39 Windows test failures — root-cause families

**Date:** 2026-09-06
**Found by:** a Windows compile-and-test run requested during the
integration-throughput cycle (`plans/2026-09-06-integration-throughput.md`).
**Relationship to that cycle:** none. Every failure below reproduces identically
on `origin/main` and is unaffected by that branch — proven by a baseline run,
see §1.
**Status:** nothing has been changed. This document is the starting point for
whoever picks the work up.

---

## 1. Why the number is trustworthy

`cargo test -p athenaeum-core --lib` on `x86_64-pc-windows-msvc` (rustc 1.96.1,
submodules at their true pins) reports:

```
branch perf/integration-throughput:  1674 passed; 39 failed; 9 ignored
origin/main (70de6a1d):              1639 passed; 39 failed; 9 ignored
```

The sorted failure lists are **identical, name for name**. The throughput branch
adds 35 passing tests and changes the failure set not at all. Attributing these
to recent work would have been the easy, wrong move; the baseline is what makes
the number mean something.

`cargo check --workspace --all-targets` on the same machine is **clean, exit 0**,
across all three crates and all targets.

## 2. Why nobody knew

**The Rust test suite has only ever run on Linux.** Verified, not inferred:

- `.gitlab-ci.yml` contains `cargo test` **zero** times. `build:windows` is
  `only: tags` and its script is `npm install` + `npm run tauri build` — it
  builds the bundle and never tests.
- `.github/workflows/ci.yml:34` is `runs-on: ubuntu-latest`, and `:96` holds the
  only `cargo test --workspace` in the repository.

So no job, on any trigger, has ever executed this suite on Windows or macOS.
These 39 have been failing behind that gap for an unknown length of time.

## 3. The families

Grouped by root cause rather than by module — the module grouping is misleading,
because one cause spans several modules and one module holds two causes. Counts
in brackets.

### A — `canonicalize()` returns a `\\?\` verbatim path [2]

`archive::root::tests::resolve_accepts_respelled_configured_root` (`archive/root.rs:159`),
`api::sync::tests::validate_transfer_dir_tolerates_an_offline_scan_root` (`api/sync.rs:11640`).

`std::fs::canonicalize` on Windows returns a verbatim `\\?\`-prefixed path, which
never string-compares equal to the non-canonicalized spelling. **Production-side
shaped, not test-side**, and it touches a standing architectural rule: the
file-move hot path deliberately matches the scanner's own non-canonicalized
spelling and must not gain a `canonicalize`. Something here is canonicalizing,
and on Windows that changes the spelling rather than normalising it.

### B — setting mtime on a DIRECTORY [5]

All five at one call site, `api/sync.rs:7159`, all
`open <dir> for mtime set: Access is denied. (os error 5)`:
`leftovers_after_moving_only_the_outgoing_folder`,
`leftovers_spare_a_young_rowless_payload_dir`,
`sweep_age_gates_rowless_dirs_never_racing_a_concurrent_enqueue`,
`sweep_keeps_a_dir_whose_top_level_mtime_is_old_but_has_a_fresh_file`,
`sweep_removes_only_rowless_orphans_and_orphan_tags`.

Windows cannot open a directory handle through the ordinary path; it needs
`FILE_FLAG_BACKUP_SEMANTICS` (`OpenOptionsExt::custom_flags(0x0200_0000)`).
**One helper, one flag, five tests.** The cheapest win in the list.

### C — hardcoded forward slashes in expectations [2]

`calibration_library::paths::tests::dark_path_shape` (`paths.rs:209`),
`…::flat_includes_filter_and_missing_fields_collapse` (`paths.rs:227`).
The production code built a correct Windows path and the assertion carried a
POSIX one. **The only purely test-side text in the whole list** — which is worth
stating, because "they're all forward-slash expectations" was the first guess and
it was wrong.

### D — `QueryReturnedNoRows` in the in-place rescan path [6] — handle with care

`scanner::inplace_tests::` `rescan_after_mtime_change_preserves_session_members`
(`scanner/mod.rs:2156`), `rescan_updates_bayer_offsets_and_roworder_in_place`
(`:2314`), `rescan_syncs_stored_header_blob_with_frames_columns` (`:2386`),
`parallel_rescan_after_mtime_change_preserves_session_members` (`:2462`),
`parallel_rescan_preserves_user_override_edits` (`:2554`); plus
`rescan_recovers_orphaned_files_row_with_no_frames` (`:2249`), which fails
differently but neighbours them.

A path-keyed lookup coming back empty is **also exactly what a real separator or
spelling bug looks like once it reaches SQLite**. This is the path where the
non-destructive rescan preserves `files.id` / `frames.id` so junction rows
survive an edit — a genuine failure here would mean Windows rescans orphan those
rows rather than preserving them. **Establish which side is wrong before touching
either side.**

### E — scan-root containment and prefix matching [16]

Eleven in `api::scan_roots::switch_library_tests` and `candidate_tests`
(`scan_roots.rs:2067`–`2492`), three in `overview_tests` (`:2735`, `:2785`,
`:2865`), one in `missing_files_tests` (`:3077`). Containment and prefix
comparisons over path strings — the same underlying question as family A: which
spelling is canonical. One of them differs and is test-side:
`overview_counts_archived_sets_and_distinct_zip_bytes` (`:2865`) fails with
`Os { code: 123, kind: InvalidFilename }` — a path literal that simply cannot
exist on Windows.

### F — wire `rel_path` not split as a separator [4] — one is a security guard

`sync::ingest_tests::ingest_lands_files_and_rows` (`ingest_tests.rs:631`),
`…::ingest_lands_under_resolved_device_name` (`:454`),
`…::ingest_dot_only_device_name_lands_under_hex_slug_not_above_incoming_root` (`:410`),
`sync::receiver::tests::v2_receive_lands_under_batch_and_settles_files_with_history`
(`receiver.rs:5387`).

A wire-format `rel_path` carrying `/` is not being split as a separator on
Windows, so the nesting is not preserved. **The third of these is the
escape-the-root guard** — it asserts a dot-only device name cannot land above the
incoming root. A guard that cannot run on a platform is not a guard on that
platform; this one should not stay red indefinitely.

### G — a hardcoded `\n` delimiter meets `core.autocrlf` [1]

`sync::receiver::tests::every_terminal_writer_announces_or_is_a_named_exemption`
(`receiver.rs:6003`), `left: 26, right: 12`.

Not a path bug. The helper splits `include_str!("receiver.rs")` on the literal
`"\n#[cfg(test)]\nmod tests"`. With git's Windows default `core.autocrlf=true`
the embedded source has CRLF, the split never matches, `.next()` returns the
whole file including the test module, and the count picks up test-side call
sites: 26 instead of 12.

So this **drift guard cannot do its job on any Windows checkout**, for a reason
unrelated to what it guards. Fix is CRLF-tolerant splitting, or — better and
broader — **the repository has no `.gitattributes` at all**. Given how much of
this suite compares paths and text, pinning `*.rs eol=lf` is worth doing on its
own merits, independently of this test.

### H — unattributed [1]

`scanner::calibrated_light_scan_tests::scan_repairs_moved_project_contribution`
(`scanner/mod.rs:3300`), `Option::unwrap()` on `None`. "Moved" in the name and it
neighbours family D, so probably the same lookup returning nothing — unconfirmed.

## 4. Suggested order

1. **B** — five tests, one flag, no design question.
2. **G** — one line, and it restores a guard that currently cannot run. Consider
   adding `.gitattributes` in the same change.
3. **C** and the one test-side straggler in **E** (`:2865`) — pure text.
4. **F** — behavioural, and includes a security guard.
5. **A / D / E** — the real work, and the only part where the production side may
   be wrong. Decide which spelling is canonical FIRST; the no-`canonicalize` rule
   on the move hot path is a constraint on that decision, not a detail.

## 5. What this does not cover

`athenaeum-tauri` and `athenaeum-web` tests were not run — only
`-p athenaeum-core --lib`. macOS has the same zero CI coverage for the suite and
has never been checked either.
