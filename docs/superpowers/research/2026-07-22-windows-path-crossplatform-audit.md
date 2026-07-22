# Windows / Cross-Platform Path-Handling Audit

Date: 2026-07-22 · Trigger: owner smoke on Windows — (a) sending a directory with
nested files from the dual-pane file manager is impossible, (b) "problems with
calibration". Scope: full-repo audit of path handling (Rust core/tauri/web,
frontend, submodule touchpoints) plus the incoming Windows log
(`Bug_reports/Incoming/athenaeum-desktop.2026-07-18.jsonl`, machine
`C:\Users\astro`).

Verification status: every P0 finding was re-read line-by-line in the source
after the sweep; log evidence quoted where it exists.

---

## P0 — confirmed root causes of the reported symptoms

### F1. Directory send resolves 0 frames on Windows — hardcoded `/` in the descendant byte-range

`crates/athenaeum-core/src/db/operations.rs:3008` (`frame_ids_under_paths`):

```rust
let prefix = format!("{}/", path.trim_end_matches('/'));
let upper = path_prefix_upper(&prefix);
// WHERE f.path = ?1 OR (f.path >= ?2 AND (?3 IS NULL OR f.path < ?3))
```

`files.path` is stored with NATIVE separators (Windows = `\`; `add_scan_root`
canonicalizes and the scanner stores `WalkDir` native paths — no forward-slash
normalization exists on write). For folder `C:\Astro\M31`:

- `trim_end_matches('/')` is a no-op (and never trims a trailing `\`);
- `prefix = "C:\Astro\M31/"`, `upper = "C:\Astro\M310"` (`/` 0x2F incremented → `0` 0x30);
- every real descendant has `\` (0x5C) at the divergence byte: `>= prefix` holds
  (0x5C > 0x2F) but `< upper` fails (0x5C > 0x30) → **the range matches zero rows**.

Sole caller: `api/files.rs:564` `resolve_frame_ids_for_paths` — exactly the
Send-dialog path→frame-id resolution (`DualPaneFileBrowser.tsx:878`). Folder
selection → 0 ids → "No cataloged frames in the selection", dialog never opens.
Single-file selections survive via the exact-match branch `f.path = ?1`. This
reproduces the symptom precisely: files send, folders don't, macOS unaffected.

The correct pattern already exists in the SAME file: `get_files_by_directory`
(`operations.rs:819`) builds the prefix with `std::path::MAIN_SEPARATOR` — which
is why the pane LISTS files fine while the folder can't be SENT.

The unit test `frame_ids_under_paths_matches_file_and_folder`
(`operations.rs:3834`) uses only POSIX paths, so the bug was invisible to CI.

### F2. Directory rename silently desyncs the catalog on Windows — same defect

`crates/athenaeum-core/src/api/files.rs:736-738` (`rename_path`, dir branch):

```rust
let prefix_old = format!("{}/", old_str);
let prefix_new = format!("{}/", new_str);
let updated = crate::db::rename_files_path_prefix(&conn, &prefix_old, &prefix_new)?;
```

`rename_files_path_prefix` (`operations.rs:3611`) runs the same
byte-range + SUBSTR swap. On Windows the `/`-suffixed prefix matches nothing →
disk rename succeeds, **0 catalog rows updated**, no error (the "fatal
hot-sync" contract doesn't fire because updating nothing isn't an Err). Every
file under the renamed folder becomes a stale/missing catalog row. Data
integrity, worse than F1. The single-file branch (`WHERE path = ?3`, line 742)
is fine.

### F3. Calibration outputs: sanitizer leaks trailing dots → reconcile mismatch on Windows

`crates/athenaeum-core/src/archive/path_layout.rs:8-24` (`sanitize_for_filename`)
strips the Windows-illegal set `/ \ : * ? " < > |` + control chars and trims
`_` — but **not trailing dots**. Windows itself silently drops trailing dots
when creating a file/dir.

Consequence chain (light calibration): `OBJECT = "Sh2-155."` →
`api/lights.rs:964-974` builds `<library>/Sh2-155./…`, `create_dir_all`
actually creates `<library>/Sh2-155/…`, but
`light_calibrations.output_path` stores the dotted string
(`api/lights.rs:1001`). On every scan `reconcile_calibrated_light`
(`scanner/mod.rs:693`) compares `row.output_path == current_path` — never
equal → the artifact is forever mis-classified as *moved* (path rewritten every
scan) or *duplicate* (noisy scan warnings). Strong candidate for the reported
"problems with calibration" on Windows.

Adjacent gaps in the same sanitizer (verified absent repo-wide):

- **No Windows reserved-name guard** (`CON PRN AUX NUL COM1-9 LPT1-9`, any
  case, with/without extension): `OBJECT="NUL"` → `create_dir_all` fails at
  build/calibration time. Also missing in
  `export/models.rs:242` (`sanitize_display_folder_name`).
- **`.` / `..` pass through**: `sanitize_for_filename("..") == ".."`, and the
  `sanitized_or` fallback (`calibration_library/paths.rs:91-98`) only fires on
  EMPTY → `OBJECT=".."` escapes the calibration-library root via
  `library_dir.join("../…")`. Platform-independent but lives in the same fix.
- The codebase already knows the cure: `sync/receiver.rs:816-818`
  (`sanitize_batch_slug`) wraps the SAME sanitizer with `.trim_matches('.')`.
  Calibration/master/export builders call it bare.

### F4. Account sign-in on Windows: mandatory file lock on `device_key` (os error 33)

Log evidence (user's Windows machine, 2026-07-18 17:47):

```
account_sign_in_verify | error: device key: read device key
C:\Users\astro\AppData\Roaming\com.vsharifov.athenaeum\sync\device_key:
The process cannot access the file because another process has locked a
portion of the file. (os error 33)
```

Mechanism: the iroh node holds a process-lifetime `fd_lock::RwLock` write lock
ON THE KEY FILE ITSELF (`account/keys.rs:126-150`, `DeviceKeyLock`). On
unix `flock` is advisory — a parallel `std::fs::read` succeeds. On Windows
`fd_lock` maps to `LockFileEx`, which is **mandatory**: any other handle's read
of the locked range fails. Sign-in re-reads the key file
(`keys.rs:272`, `load_or_create_device_key`) while the node holds the lock →
os error 33 → sign-in unusable while sync is running. Cross-platform semantic
difference (advisory vs mandatory), not a code typo.

Fix directions (decide at planning): lock a **sidecar** file
(`device_key.lock`) instead of the key file; or never re-read the file while
the process holds the key (serve the cached `DeviceKey` from memory). Sidecar
is the smaller, more honest change.

---

### F4b. Master build dies on `UNIQUE constraint failed: files.path` and self-perpetuates (user report, log 2026-07-20)

User log (`485850ff-athenaeumdesktop.20260720.jsonl`): five consecutive
preflight master builds (sets 126, 118, 119, 120, 125) each fail with
`UNIQUE constraint failed: files.path: Error code 2067`, ~10 s apart
(serialized ComputeQueue); the light-calibration batch then fails wholesale —
452× "linked calibration set is not a built master — skipping" + 226× "no
calibration masters available", `ok_count: 0, failed: 226`.

Mechanism (verified in source):

1. `resolve_collision` (`calibration_library/paths.rs:125-139`) checks **disk
   only** (`abs.exists()`), never the catalog. A `files` row at path X whose
   disk file is gone → the un-suffixed X is returned as "free".
2. `register_master` (`calibration_library/register.rs:164`) does a plain
   `insert_file` — no upsert, no existing-row reconcile → UNIQUE fails.
3. On registration failure the build **deletes the freshly written master**
   (`api/masters.rs:830`) to avoid an unregistered orphan — which restores the
   exact divergence state. Every retry fails identically, forever; the state
   never self-heals and the good pixels are thrown away each time.
4. Master paths are deterministic (INSTRUME/type/filter/exptime/temp/gain/
   binning/set-date), so the leftover row is re-hit exactly on every attempt.

Origin of the DB↔disk divergence is not determinable from this log slice (it
starts at 19:01 with the state already established — need the user's DB or the
previous day's log). But the CLASS is guaranteed by the project's own rule
"files missing on disk are not orphans — never auto-delete catalog rows": any
flow where master files leave the disk while rows remain (user deletes/moves
masters in Explorer to redo them, disconnected volume, F3's Windows trailing-dot
mismatch seeding a phantom row) lands in this permanent-failure loop.
Platform-neutral mechanism; Windows adds extra seeding paths via F3.

Logging gap: the ERROR carries no `path` field — the colliding path is
invisible in the log, which is why this needed code archaeology. Add the
target path + param tokens to the failure event.

Fix direction (folded into the plan below as Task 2b):
- Make collision resolution catalog-aware: a candidate path is free only if it
  is absent on disk AND has no `files` row (pass the connection through).
- In `register_master`, if a `files` row exists at the target path with the
  disk file missing → reuse/update the row in place (scanner
  `reparse_and_update_in_place` idiom, preserves `files.id`/junctions) instead
  of inserting.
- Never delete the written master when the failure is a catalog conflict, and
  include the path in the error string so users report actionable messages.

## P1 — confirmed Windows defects, secondary severity

### F5. Calibration-library coverage hint never resolves on Windows (frontend)

`src/components/CalibrationFolderSection.tsx:99` — descendant check builds only
`r.path + '/'`; Windows `dir` uses `\` → the "covering scan root" hint silently
never shows for nested folders. The correct twin exists at
`DualPaneFileBrowser.tsx:265-266` (checks both `+'/'` and `+'\\'`). Cosmetic.

### F6. Unsanitized DATE-OBS segment in light-cal output path

`api/lights.rs:698-707` (`date_part`) → `calibration_library/paths.rs:109-120`
takes the date "as-is" (doc says "already filesystem-safe") — true only for ISO
`DATE-OBS`. A malformed `05/07/2026` nests directories; a value with `:` in the
first 10 chars is Windows-illegal → `create_dir_all` fails. Sanitize the
segment like every other token.

### F7. Re-calibration overwrite can hit a Windows sharing violation

Re-runs overwrite `output_path` in place via sibling-tmp + `std::fs::rename`
(`fits_writer/writer.rs:73`; maps to `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`).
Fine — unless the destination FITS is open in a viewer/indexer/AV scan; Windows
then fails the replace (POSIX wouldn't). No retry/backoff → the frame errors.
Low-cost hardening: bounded retry with delay on `PermissionDenied`/sharing
violations, or surface a per-frame "file in use" error message.

### F8. Export symlink mode on Windows — VERIFIED NON-ISSUE (2026-07-22 re-check)

`export/file_organizer.rs:360-364` cfg-gating is correct AND the UI already
gates the toggle: `src/components/export/ExportTab.tsx:300`
`symlinksAvailable = isTauri && !isWindows` — on Windows the checkbox is
replaced by an explanatory line, so the privilege-failing branch is
unreachable from the UI. No action needed; do not re-flag.

### F9. `PathPolicy::check` is case-sensitive (web transport only)

`api/mod.rs:99` — `Path::starts_with` is component-wise (separator-correct) but
byte-case-exact; Windows FS is case-insensitive → a differently-cased request
path yields spurious Forbidden. Desktop uses AllowAll; only the Docker/web
`AllowedRoots` policy is exposed, and Docker is Linux — dormant today, real if
web ever runs natively on Windows.

---

## P2 — latent, mostly platform-independent (fix opportunistically)

- **Prefix checks without separator boundary** (sibling-name bleed `M31` vs
  `M31extra`):
  - `src/pages/FileManager.tsx:871,916` — folder-delete membership feeding the
    Black-Hole move: a false match RELOCATES extra files. Highest-consequence
    item in this class.
  - `src/components/DirectoryTree.tsx:49,164,209,217`,
    `src/components/duplicates/DuplicateGroupCard.tsx:51` — display-level.
  - Rust: `db/operations.rs:78-95` (`scan_root_prefix_predicate`), `:381`, `:459`
    — root-scoped sweeps; native separators so no Windows delta, but same bleed.
- **`relinking/mod.rs:57-61,195-199`** — `LIKE '{root}%'` with no trailing
  separator, ASCII-case-insensitive LIKE, and unescaped `%`/`_` in the root
  path. Final header-fingerprint match filters false candidates, so impact is
  widened candidate sets / mis-scoped accounting.
- **Byte-exact path identity everywhere** — `files.path UNIQUE` under BINARY
  collation, `get_file_by_path` `WHERE path = ?1`, `archive/root.rs:44`.
  Windows is case-insensitive; drive-letter case or 8.3-vs-long forms create
  duplicate rows/misses. On a single machine everything derives from
  `canonicalize()` + `normalize_path`, so it holds in practice; it's the
  broadest LATENT issue and needs a deliberate normalization decision, not a
  spot fix.
- **`light_calibrations.output_path` portability** — native-separator string
  compared byte-exact in reconcile. Same-machine fine; only matters if those
  rows ever travel across platforms (they don't today).
- **Receiver slug sanitization** (`sanitize_batch_slug`/`sanitize_slug`) trims
  dots but has no reserved-name guard either — same F3 adjacent gap, receiver
  side.

## Verified correct — do NOT re-flag in future cycles

- **Transfer wire contract**: sender rel_path is forward-slash by construction
  (`sync.rs:2034-2112` joins `Component::Normal` with `/`); `validate_rel_path`
  (`package/mod.rs:66-92`) rejects `\`, drive letters, non-Normal components
  (dedicated cross-platform test); receiver lands via
  `landing_base.join(rel)` — `/` is valid on Windows. `FileTree.tsx` splitting
  on `/` is correct BY CONTRACT with this invariant.
- **Zip layer**: `path_in_zip` forces `/` (`path_layout.rs:88-102`); restore
  joins `/`-form onto native `PathBuf`, uses `copy` (no EXDEV).
- **file_op planner/executor**: `device_id_for` is properly cfg-gated
  (unix `dev()` vs windows volume-root hash); cross-volume =
  CopyVerifyDelete, no EXDEV exposure.
- **fits_writer**: tmp is a sibling in the target dir → rename always
  same-volume.
- **`normalize_path`** (`api/scan_roots.rs:64-79`) strips `\\?\` / `\\?\UNC\`
  verbatim prefixes and is applied to scan roots AND the calibration-library
  dir — the canonicalize-mismatch trap is already defused.
- **Scanner `path_has_root_prefix`** checks both separators and drive-root
  trailing forms.
- **Frontend dualpane helpers** (`dualpane/types.ts:150-198`), DirectoryTree
  splitting, breadcrumbs, mkdir/rename joins — all separator-detecting.
  ~15 basename/dirname sites across the app split on `/[/\\]/`.
- **`ATHENAEUM_ALLOWED_PATHS`** splits on comma — drive-letter-safe.
- **API layers** pass paths opaquely (JSON body values, never URL segments).
- **ATH_C\* header parse** `splitn(2, ' ')` — Windows paths with spaces and
  backslashes parse correctly.
- The 2026-07-18 log's `noq_udp` os error 10040 (UDP datagram size) and relay
  disconnects are network-layer noise, unrelated to paths.

---

## Proposed fix plan (for the next planning pass)

Task 1 — **P0 separator fixes** (small, surgical):
- `frame_ids_under_paths`: build the descendant prefix from the separator the
  path string actually uses (`if path.contains('\\') { '\\' } else { '/' }`,
  matching the frontend helpers) or `MAIN_SEPARATOR` like
  `get_files_by_directory`; trim BOTH trailing separators. Prefer
  string-derived: it stays testable from macOS CI with Windows-shaped fixtures.
- `rename_path` dir branch: same treatment for `prefix_old`/`prefix_new`.
- Tests: extend `frame_ids_under_paths_matches_file_and_folder` and the
  `rename_files_path_prefix` tests with `C:\…` backslash fixtures (pure string
  logic — runs anywhere).

Task 2 — **Sanitizer hardening** (shared fn, one place):
- In `sanitize_for_filename`: after the existing pass, trim trailing `.` (and
  keep trailing-space handling), map reserved device names (case-insensitive,
  with/without extension) to a suffixed form (`NUL_`), and collapse `.`/`..`
  to the fallback. Update `sanitize_batch_slug` to drop its now-redundant
  local trim. Sweep `sanitize_display_folder_name` for the same.
- Tests: table-driven cases (`"Sh2-155."`, `"NUL"`, `"com3.fits"`, `".."`,
  `"NGC 7000 "`).
- Note: existing DB rows created with dotted paths on Windows stay mismatched;
  reconcile branch 2 (*moved*) will self-heal them to the real on-disk path
  after the sanitizer fix — verify this in the plan.

Task 2b — **Catalog-aware master registration** (from F4b):
- `resolve_collision` gains a DB check (disk-free AND no `files` row);
  `register_master` reconciles an existing missing-on-disk row in place;
  failure path stops deleting the written master on catalog conflicts; error
  and ERROR log carry the target path.
- Tests: build with a pre-seeded phantom `files` row at the target path
  (file absent on disk) → build succeeds by reuse; same with file present →
  `_2` suffix; error text contains the path.

Task 3 — **device_key lock sidecar**: move `DeviceKeyLock` to
`device_key.lock` sentinel so reads of the key file never contend on Windows;
keep the exclusive-bind semantics. Test: acquire lock, then `fs::read` the key
file in-process (fails today on Windows, passes after).

Task 4 — **Small Windows items**: `CalibrationFolderSection.tsx:99` both-sep
check (copy the DualPane pattern); DATE-OBS segment sanitize; recalibration
rename retry-on-sharing-violation; export symlink toggle hidden on Windows.

Task 5 (separate, opt-in) — **Boundary/identity debt**: FileManager Black-Hole
prefix boundary (add separator-or-equal check), DirectoryTree/DuplicateGroupCard
boundaries, relinking LIKE escape + boundary, root-sweep predicates. The
byte-exact path-identity/collation question needs its own design decision —
out of scope for the Windows hotfix cycle.

## Verdict

The two reported Windows symptoms have concrete, verified root causes:
directory send fails because of ONE hardcoded `/` in
`frame_ids_under_paths` (operations.rs:3008); the calibration trouble is most
plausibly the trailing-dot sanitizer leak breaking scanner reconcile (plus the
sign-in-blocking device_key mandatory lock seen directly in the user's log).
The transfer wire, zip, file_op, and scanner layers are already
cross-platform-clean — the debt is concentrated in a handful of DB
prefix-predicates and one shared sanitizer, all with existing correct patterns
in-repo to copy.
