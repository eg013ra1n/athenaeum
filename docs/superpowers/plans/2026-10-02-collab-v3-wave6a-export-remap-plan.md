# Collab v3 wave 6a — project export and filter remap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The project WBPP export lands every frame in `<title>/<publisher>/camera_<instrume>/<canonical filter>/` with the canonical name in its `FILTER` card (a patched copy when the file's own card differs; symlinks for the rest in symlink mode). The coordinator remaps filters from the app. An attested frame set passes through the calibrated-lights export as is.

**Architecture:**
- Two new byte-level header patchers in `fits_writer`, one for FITS and one for monolithic XISF, rewrite only headers and stream every data byte verbatim.
- The export gets its own small project organizer that reuses the existing claims, sanitizers and progress event. It is fed by a reshaped collector that reads the canonical filter from the frame row.
- The remap is a paced, cancellable background task over the hub's existing per-frame `PATCH`.
- The live feed worker learns to fold a burst of pull-needing manifest events into one pull.

**Tech Stack:** Rust (`athenaeum-core`, Tauri and Axum hosts), `quick-xml` 0.41 (already a core dependency), `tokio`, `wiremock` (the fake hub), React + TypeScript (Vitest), ts-rs generated types.

**Spec:** `docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md` (owner-approved 2026-10-02, fable-reviewed; §10 export, §11 remap, §13 R3, §19 review record). Read §2 (rulings W1–W9) before any task.

## Global Constraints

- **Branch:** `collab-wave6` (holds the spec). Merge and push happen only on the owner's word.
- **Commits:** as the user (`eg013ra1n` / `vilen.sharifov@gmail.com`). Every message ends with:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK`.
- **Two backends in sync.** Every new command lands on BOTH hosts in the same task:
  - Tauri: `crates/athenaeum-tauri/src/commands/collab.rs`, registered in `invoke_handler![]` in `crates/athenaeum-tauri/src/lib.rs`.
  - Axum: `crates/athenaeum-web/src/routes/collab.rs`, registered in `build_router` in `crates/athenaeum-web/src/routes/mod.rs`.
  - Real logic lives in `athenaeum-core`.
- **Command boundary attributes.** Every command and route wears `#[tracing::instrument(skip_all, err)]` (web: `err(Debug)`).
- **Serde.** Wire structs use `#[serde(rename_all = "camelCase")]`. New TS-facing types derive `ts_rs::TS` and are registered in `crates/athenaeum-core/src/ts_export.rs`. Regenerate with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`; never hand-edit generated files under `src/types/`.
- **Frontend boundaries.** No `@tauri-apps/*` import outside `src/api/`. Design tokens only (`text-content`, `text-content-faint`, `text-error`, `text-warning`, `text-success`, `bg-surface…`). New dialogs use `DialogShell`. Listeners follow the StrictMode-safe `api.listen` pattern in `CLAUDE.md`.
- **Errors and logging.**
  - Never swallow an error: log at `warn!`/`error!` (Rust) or `console.error` (TS) before degrading.
  - `tracing` only: message = a short stable phrase, data in snake_case fields.
  - Zero `println!` in production code.
- **The replica is sacred.** No code path in this plan opens a landed project file (`project_frames_local.landed_path`) or an own original for writing. Patchers refuse `src == dest`.
- **Build discipline (this Mac):**
  - `CARGO_BUILD_JOBS=4`, `--test-threads=4`, one compile at a time.
  - Format touched files with `rustfmt <files>`, never `cargo fmt -p`. clippy is not a gate.
  - Collab live tests run with `--test-threads=2` when run alone.
- **Command count.** This plan adds exactly 3 commands (`get_collab_filter_buckets`, `remap_collab_filter`, `cancel_collab_remap`): 294 → 297.
- **Copy rules:** UI strings, docs, comments and commit messages are in English. Never name a third-party project in the repo. Timestamps in the UI go through `formatTimestamp`.

## Review Focus

1. **Exporting into a tree that already holds a symlink to the replica, in copy mode.** The old link must be replaced by a regular file. The copy must never be written THROUGH the link into the replica. Pinned in Task 5.
2. **A remap that moves a frame to another filter folder, then a re-export.** The old copy stays on disk and is named once in a warning. The new copy lands in the new folder. An unchanged frame triggers no warning. Pinned in Task 5.
3. **An XISF whose patched header outgrows the first attachment.** Every data block (not only the image) is byte-identical at its shifted position, and the result still reads with the app's own XISF parser. Pinned in Task 2.
4. **A remap stopped part-way (cancel, a missing capability, a rate limit), or a second remap started meanwhile.**
   - The finished event reports done/failed/total truthfully.
   - The second remap is refused.
   - A 429 waits and retries instead of failing the frame.
   Pinned in Task 9.
5. **Feed coalescing with events of two projects and a `hello` interleaved.** Order across projects and non-project work is preserved, no event is lost, and the `pending` counter returns to zero. Pinned in Task 11.

## Plan rulings (refine the spec where the code forced a choice)

- **P1 — The project export gets a sibling organizer, not a flag inside `organize_files_wbpp`.** It lives in `export/project_organizer.rs`, reusing `DestClaims` (made `pub(crate)`), the sanitizers and `ExportProgressEvent`. Spec §10.1 says "a layout mode, not a fork". This honours its intent: the personal organizer, its skip-if-exists behaviour and its generation path stay byte-for-byte as they are. The project layout has no calibration levels and no generation, and its placement rules (link / copy / patched copy, compare-and-replace) differ in every branch.
- **P2 — The collector returns plain `ProjectExportFrame` rows** instead of building `ExportData`. Nothing else consumed that `ExportData`.
- **P3 — A read command `get_collab_filter_buckets`** returns the Filters section's buckets (and the running remap's progress). `ProjectFrameView` carries no `filterRaw`/`instrume`. 6a adds 3 commands, not 2; the wave total becomes 306.
- **P4 — FITS edit semantics.**
  - `CardEdit::Set` applies to the image HDU (inserting before END when absent) and to every other HDU that already carries the keyword.
  - `SetIfPresent` applies only where the keyword exists.
  - `Remove` drops the keyword everywhere.

  This is how the spec's "the image HDU, and the primary too if it carries FILTER" is realized.
- **P5 — The XISF patcher splices the XML text at the parser's byte offsets** instead of re-serializing the document. Every byte outside the edited elements and the shifted `location` numbers stays identical. This is stricter than the spec's "re-serialize".
- **P6 — R3 export half (spec §13) via an `attested` flag on `ExportReadiness`.** `check_mode_ready(CalibratedLights)` returns `Ok` for it before the master/link arms.
  - Zeroing the tally instead would also open raw-with-masters for an attested set that has links.
  - The stacking plan does not read `attested` in 6a (that is 6b).
  - The send path keeps `PayloadKind::CalibratedLight` with `generate = false` for such lights.
- **P7 — Feed coalescing is drain-and-merge.**
  - When the worker takes a `project` event that needs a manifest pull, it drains what is already queued.
  - It merges each following event of the same project that is contiguous (`prev` = the merged `version`) and carries only `frames` changes, into one pull-forcing event (`more: true`, `frames: None`, `version` = the last merged).
  - Anything else is kept, in order, in a carry queue the loop serves before the channel.
  - `pending` is decremented once per original event.
- **P8 — The remap runs on `tokio::spawn`.** Its registry is keyed `"{db path}|{project_id}"`, as the publish run's is. A second remap of the same project is refused with `Conflict(REMAP_BUSY_MSG)`.
- **P9 — Remap error handling.**
  - `Forbidden` (403) stops the task: "This account lacks thresholds.edit."
  - `Http{status: 400}` stops it: the dictionary changed under the remap.
  - `RateLimited` (429) waits 5 s and retries the same frame, up to 3 times, before counting it failed.
  - `Http{status: 404}` and other per-frame errors count as failed and the task continues.
- **P10 — Moderation visibility and layout.**
  - The Moderation tab is visible when `canModerate || canRemap`.
  - Its review and excluded sections render only with `canModerate`.
  - The new Filters section renders only with `canRemap`, last.
- **P11 — Result counts.** `ExportResult` and `ExportCompleteEvent` gain `linked`, `copied`, `rewritten`, `failed` (`i32`, `#[serde(default)]`); the personal export leaves them `0`.
- **P12 — `FramesChangeKind::Remapped` is emitted for the caller's OWN frames only**, and drives the history-only notification. Remaps of other members' frames reach the page through the existing `collab-project-synced` reload and notify nothing (spec §11.2).

## File map

| File | Change |
| ---- | ---- |
| `crates/athenaeum-core/src/fits_writer/header_patch.rs` | **new** — FITS header patcher + `read_image_keyword` |
| `crates/athenaeum-core/src/fits_writer/xisf_patch.rs` | **new** — XISF header patcher + `read_xisf_keyword` |
| `crates/athenaeum-core/src/fits_writer/stamp.rs` | **deleted** (insert-only, unused) |
| `crates/athenaeum-core/src/fits_writer/mod.rs` | module list / re-exports |
| `crates/athenaeum-core/src/fits_writer/xisf_writer.rs` | `escape_xml` → `pub(crate)` |
| `crates/athenaeum-core/src/export/filter_header.rs` | **new** — the FILTER rule (read, decide, write a patched copy) |
| `crates/athenaeum-core/src/export/project_collector.rs` | reshaped to `ProjectExportFrame` |
| `crates/athenaeum-core/src/export/project_organizer.rs` | **new** — project layout, placement, export record |
| `crates/athenaeum-core/src/export/file_organizer.rs` | `DestClaims` → `pub(crate)` |
| `crates/athenaeum-core/src/export/mod.rs` | module list / re-exports |
| `crates/athenaeum-core/src/export/models.rs` | `ExportResult`/`ExportCompleteEvent` counts |
| `crates/athenaeum-core/src/export/data_collector.rs` | attested pass-through in `apply_calibrated_lights` |
| `crates/athenaeum-core/src/api/lights.rs` | `ExportReadiness.attested`, `check_mode_ready` arm |
| `crates/athenaeum-core/src/api/collab_exchange.rs` | export runner, `Remapped`, `classify_frame_change` |
| `crates/athenaeum-core/src/api/collab_remap.rs` | **new** — buckets, remap task, cancel |
| `crates/athenaeum-core/src/api/collab.rs` | card caps, `project_dictionary` → `pub(crate)` |
| `crates/athenaeum-core/src/api/mod.rs` | `pub mod collab_remap;` |
| `crates/athenaeum-core/src/collab/hub_client.rs` | `patch_frame_filter` |
| `crates/athenaeum-core/src/collab/fake_hub.rs` | `PATCH …/frames/{uuid}` route |
| `crates/athenaeum-core/src/api/collab_live/workers.rs` | coalescing |
| `crates/athenaeum-core/src/api/collab_live/live_tests.rs` | 500-event coalescing e2e |
| `crates/athenaeum-core/src/ts_export.rs` | new types |
| `crates/athenaeum-tauri/src/commands/collab.rs`, `src/lib.rs` | 3 commands |
| `crates/athenaeum-web/src/routes/collab.rs`, `src/routes/mod.rs` | 3 routes |
| `src/components/collab/ProjectExportDialog.tsx` (+ test) | counts |
| `src/hooks/useExportProgress.ts` | counts in the project export notification |
| `src/utils/exportCounts.ts` | **new** — `exportCountsText`, shared by the dialog and the hook |
| `src/components/export/ExportTab.tsx` (+ test) | `attested` arm + hint |
| `src/components/collab/project/FilterRemapSection.tsx` (+ test) | **new** |
| `src/components/collab/project/ModerationTab.tsx` (+ test) | sections by capability |
| `src/pages/ProjectDetail.tsx` (+ test) | tab gate, props |
| `src/hooks/useCollabNotifications.ts` (+ test) | `remapped`, `collab-remap-finished` |
| docs: `docs/export/README.md`, `docs/export/calibrated-lights.md`, `docs/transfers/README.md`, `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`, `docs/superpowers/open-items.md`, `CLAUDE.md` | Task 13 |

---

### Task 0: Baseline

**Files:** none.

- [ ] **Step 1: Confirm the branch and a clean tree**

Run: `git -C /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum status -sb`
Expected: `## collab-wave6` and no changes.

- [ ] **Step 2: Record the baseline gates (numbers go into the SDD ledger)**

Run, one at a time:
```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
CARGO_BUILD_JOBS=4 cargo check --workspace
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 fits_writer export:: collab_exchange
npx tsc --noEmit -p .
npx vitest run src/components/collab src/hooks src/pages/ProjectDetail.test.tsx src/components/export
```
Expected: all green. Record pass counts. Any red test here is pre-existing; note its name and do not fix it in this plan.

---

### Task 1: FITS header patcher

**Files:**
- Create: `crates/athenaeum-core/src/fits_writer/header_patch.rs`
- Delete: `crates/athenaeum-core/src/fits_writer/stamp.rs`
- Modify: `crates/athenaeum-core/src/fits_writer/mod.rs` (module list: `mod stamp;` → `pub mod header_patch;`; drop `pub use stamp::stamp_extra_card;`)

**Interfaces:**
- Produces:
  - `pub enum CardEdit { Set(Card), SetIfPresent(Card), Remove(String) }`
  - `pub struct PatchReport { pub hdus: usize, pub image_hdu: usize, pub grown_blocks: usize }`
  - `pub fn patch_cards(src: &Path, dest: &Path, edits: &[CardEdit]) -> Result<PatchReport, FitsWriteError>`
  - `pub fn read_image_keyword(path: &Path, key: &str) -> Result<Option<String>, FitsWriteError>`
  - `pub(crate) fn refuse_same_file(src: &Path, dest: &Path) -> Result<(), FitsWriteError>`
  - `pub(crate) fn sibling_tmp(dest: &Path) -> PathBuf`

- [ ] **Step 1: Confirm `stamp_extra_card` has no production caller**

Run: `grep -rn "stamp_extra_card" crates/ --include=*.rs`
Expected: hits only in `fits_writer/stamp.rs` and `fits_writer/mod.rs`. If anything else calls it, stop and report: the spec's deletion premise is wrong.

- [ ] **Step 2: Write the failing tests** (bottom of the new `header_patch.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::{write_fits_f32, CardValue};

    fn rec(s: &str) -> [u8; CARD_SIZE] {
        let mut r = [b' '; CARD_SIZE];
        r[..s.len()].copy_from_slice(s.as_bytes());
        r
    }
    fn kv(k: &str, v: &str) -> String {
        format!("{k:<8}= {v:>20}")
    }
    fn ks(k: &str, v: &str) -> String {
        format!("{k:<8}= '{v:<8}'")
    }
    /// One HDU: the cards, END, blank-padded to a block; the data zero-padded.
    fn hdu(cards: &[String], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in cards {
            out.extend_from_slice(&rec(c));
        }
        out.extend_from_slice(&rec("END"));
        out.resize(out.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE, b' ');
        let start = out.len();
        out.extend_from_slice(data);
        out.resize(start + data.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE, 0);
        out
    }
    fn filter_card(v: &str) -> Card {
        Card::new("FILTER", CardValue::Str(v.into())).unwrap()
    }
    /// The bytes of each HDU's data unit, in order.
    fn data_units(path: &Path) -> Vec<Vec<u8>> {
        let bytes = std::fs::read(path).unwrap();
        let mut f = File::open(path).unwrap();
        let (layouts, _, _) = read_layouts(&mut f).unwrap();
        layouts
            .iter()
            .map(|h| {
                let s = h.data_offset as usize;
                bytes[s..(s + h.data_len_padded as usize).min(bytes.len())].to_vec()
            })
            .collect()
    }

    #[test]
    fn replaces_filter_in_place_and_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        let data: Vec<f32> = (0..16).map(|v| v as f32).collect();
        write_fits_f32(&src, 4, 4, 1, &data, &[filter_card("Red")]).unwrap();
        let before = std::fs::read(&src).unwrap();
        let dest = dir.path().join("b.fits");
        let card = filter_card("R").with_comment("was 'Red'");
        let report = patch_cards(&src, &dest, &[CardEdit::Set(card)]).unwrap();
        assert_eq!(report.grown_blocks, 0);
        assert_eq!(read_image_keyword(&dest, "FILTER").unwrap().as_deref(), Some("R"));
        assert_eq!(data_units(&src), data_units(&dest));
        assert_eq!(std::fs::read(&src).unwrap(), before, "src is never written");
        assert_eq!(std::fs::metadata(&src).unwrap().len(), std::fs::metadata(&dest).unwrap().len());
    }

    #[test]
    fn inserts_a_missing_card_before_end() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        write_fits_f32(&src, 2, 2, 1, &[0.0; 4], &[]).unwrap();
        let dest = dir.path().join("b.fits");
        patch_cards(&src, &dest, &[CardEdit::Set(filter_card("Ha"))]).unwrap();
        assert_eq!(read_image_keyword(&dest, "FILTER").unwrap().as_deref(), Some("Ha"));
    }

    #[test]
    fn grows_by_one_block_when_the_end_block_is_full() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("full.fits");
        // 35 cards + END = 36 records = exactly one block.
        let mut cards = vec![kv("SIMPLE", "T"), kv("BITPIX", "8"), kv("NAXIS", "1"), kv("NAXIS1", "4")];
        for i in 0..31 {
            cards.push(kv(&format!("PAD{i:02}"), "1"));
        }
        assert_eq!(cards.len(), 35);
        std::fs::write(&src, hdu(&cards, &[1, 2, 3, 4])).unwrap();
        let dest = dir.path().join("b.fits");
        let report = patch_cards(&src, &dest, &[CardEdit::Set(filter_card("L"))]).unwrap();
        assert_eq!(report.grown_blocks, 1);
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), std::fs::metadata(&src).unwrap().len() + 2880);
        assert_eq!(data_units(&src), data_units(&dest));
        assert_eq!(read_image_keyword(&dest, "FILTER").unwrap().as_deref(), Some("L"));
    }

    #[test]
    fn drops_later_duplicates_and_honours_set_if_present_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("dup.fits");
        let cards = vec![
            kv("SIMPLE", "T"), kv("BITPIX", "8"), kv("NAXIS", "1"), kv("NAXIS1", "2"),
            ks("FILTER", "Red"), ks("FILTER", "Rouge"), kv("CHECKSUM", "1"),
        ];
        std::fs::write(&src, hdu(&cards, &[7, 7])).unwrap();
        let dest = dir.path().join("b.fits");
        let ath = Card::new("ATH_FILT", CardValue::Str("R".into())).unwrap();
        patch_cards(
            &src,
            &dest,
            &[CardEdit::Set(filter_card("R")), CardEdit::SetIfPresent(ath), CardEdit::Remove("CHECKSUM".into())],
        )
        .unwrap();
        let text = String::from_utf8_lossy(&std::fs::read(&dest).unwrap()[..2880]).into_owned();
        assert_eq!(text.matches("FILTER  =").count(), 1);
        assert!(!text.contains("ATH_FILT"), "SetIfPresent never inserts");
        assert!(!text.contains("CHECKSUM"));
    }

    #[test]
    fn patches_the_compressed_image_extension_and_the_primary_and_streams_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("mef.fits");
        let primary = hdu(&[kv("SIMPLE", "T"), kv("BITPIX", "8"), kv("NAXIS", "0"), kv("EXTEND", "T"), ks("FILTER", "Red")], &[]);
        let comp = hdu(
            &[
                ks("XTENSION", "BINTABLE"), kv("BITPIX", "8"), kv("NAXIS", "2"), kv("NAXIS1", "4"),
                kv("NAXIS2", "2"), kv("PCOUNT", "0"), kv("GCOUNT", "1"), kv("TFIELDS", "1"),
                kv("ZIMAGE", "T"), ks("FILTER", "Red"),
            ],
            &[1, 2, 3, 4, 5, 6, 7, 8],
        );
        let other = hdu(&[ks("XTENSION", "IMAGE"), kv("BITPIX", "8"), kv("NAXIS", "1"), kv("NAXIS1", "3"), kv("PCOUNT", "0"), kv("GCOUNT", "1")], &[9, 9, 9]);
        let mut bytes = primary.clone();
        bytes.extend_from_slice(&comp);
        bytes.extend_from_slice(&other);
        bytes.extend_from_slice(b"trailing bytes");
        std::fs::write(&src, &bytes).unwrap();
        let dest = dir.path().join("b.fits");
        let report = patch_cards(&src, &dest, &[CardEdit::Set(filter_card("R"))]).unwrap();
        assert_eq!((report.hdus, report.image_hdu), (3, 1));
        let out = std::fs::read(&dest).unwrap();
        assert!(String::from_utf8_lossy(&out[..2880]).contains("FILTER  = 'R"), "primary carried FILTER, so it is set");
        assert_eq!(read_image_keyword(&dest, "FILTER").unwrap().as_deref(), Some("R"));
        let other_at = primary.len() + comp.len();
        assert_eq!(&out[other_at..other_at + other.len()], &other[..], "an untouched HDU is byte-identical");
        assert!(out.ends_with(b"trailing bytes"));
        assert_eq!(data_units(&src), data_units(&dest));
    }

    #[test]
    fn a_long_value_uses_continue_and_reads_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        write_fits_f32(&src, 2, 2, 1, &[0.0; 4], &[]).unwrap();
        let dest = dir.path().join("b.fits");
        let long = "N".repeat(90);
        patch_cards(&src, &dest, &[CardEdit::Set(filter_card(&long))]).unwrap();
        assert_eq!(read_image_keyword(&dest, "FILTER").unwrap().as_deref(), Some(long.as_str()));
    }

    #[test]
    fn refuses_to_patch_a_file_onto_itself() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        write_fits_f32(&src, 2, 2, 1, &[0.0; 4], &[]).unwrap();
        let err = patch_cards(&src, &src, &[CardEdit::Set(filter_card("R"))]).unwrap_err();
        assert!(err.to_string().contains("onto itself"), "{err}");
    }

    #[test]
    fn a_file_without_simple_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x.fits");
        std::fs::write(&src, vec![b'x'; 2880]).unwrap();
        assert!(patch_cards(&src, &dir.path().join("y.fits"), &[]).is_err());
    }
}
```

- [ ] **Step 3: Run them and see them fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib fits_writer::header_patch -- --test-threads=4`
Expected: compile errors (`patch_cards` not defined).

- [ ] **Step 4: Implement `header_patch.rs`** (above the tests)

```rust
//! Byte-level FITS header patching of a COPY (collab v3 wave 6, spec §10.3).
//!
//! [`patch_cards`] copies `src` to `dest` HDU by HDU: each header is
//! re-rendered with the edits applied, each data unit — and anything after
//! the last HDU — is streamed verbatim (no pixel decode). `src` is opened
//! read-only and `src == dest` is refused: the project export patches a copy
//! of a landed replica, never the replica itself.
//!
//! Edit semantics (plan ruling P4): `Set` applies to the image HDU (inserted
//! before END when absent) and to every other HDU that already carries the
//! keyword; `SetIfPresent` only where it exists; `Remove` everywhere. The
//! first card of a keyword is replaced in place; later duplicates go.
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::card::{format_card, Card, FitsWriteError, BLOCK_SIZE, CARD_SIZE};

type Record = [u8; CARD_SIZE];

/// The most header blocks one HDU may span before the file is malformed.
const MAX_HEADER_BLOCKS: usize = 64;

/// One header edit (plan ruling P4).
#[derive(Debug, Clone)]
pub enum CardEdit {
    Set(Card),
    SetIfPresent(Card),
    Remove(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchReport {
    pub hdus: usize,
    /// The HDU that holds the image (0 = primary).
    pub image_hdu: usize,
    /// Header blocks added across all HDUs.
    pub grown_blocks: usize,
}

struct HduLayout {
    cards: Vec<Record>,
    header_blocks: usize,
    data_offset: u64,
    data_len_padded: u64,
}

fn keyword_of(rec: &Record) -> &str {
    std::str::from_utf8(&rec[..8]).unwrap_or("").trim_end()
}

fn is_value_card(rec: &Record) -> bool {
    &rec[8..10] == b"= "
}

/// `(start, len)` of every value card of `key`, each with its CONTINUE tail.
fn spans_of(cards: &[Record], key: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < cards.len() {
        if keyword_of(&cards[i]) == key && is_value_card(&cards[i]) {
            let mut len = 1;
            while i + len < cards.len() && keyword_of(&cards[i + len]) == "CONTINUE" {
                len += 1;
            }
            out.push((i, len));
            i += len;
        } else {
            i += 1;
        }
    }
    out
}

/// The first `key` card's value: a string unquoted (`''` → `'`, CONTINUE
/// records joined, trailing blanks trimmed); anything else the trimmed text
/// before a `/` comment.
fn value_of(cards: &[Record], key: &str) -> Option<String> {
    let (start, len) = *spans_of(cards, key).first()?;
    let head = String::from_utf8_lossy(&cards[start][10..]).into_owned();
    if !head.trim_start().starts_with('\'') {
        return Some(head.split('/').next().unwrap_or("").trim().to_string());
    }
    let mut out = String::new();
    for rec in &cards[start..start + len] {
        let text = String::from_utf8_lossy(&rec[10..]).into_owned();
        let Some(rest) = text.trim_start().strip_prefix('\'') else { break };
        let mut piece = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\'' {
                piece.push(c);
            } else if chars.peek() == Some(&'\'') {
                chars.next();
                piece.push('\'');
            } else {
                break;
            }
        }
        match piece.strip_suffix('&') {
            Some(cont) => out.push_str(cont),
            None => {
                out.push_str(&piece);
                break;
            }
        }
    }
    Some(out.trim_end().to_string())
}

fn int_of(cards: &[Record], key: &str) -> Option<i64> {
    value_of(cards, key)?.parse().ok()
}

fn malformed(msg: impl Into<String>) -> FitsWriteError {
    FitsWriteError::Malformed(msg.into())
}

/// The padded byte length of an HDU's data unit (FITS 4.0 §4.4.1.1).
fn data_len_padded(cards: &[Record]) -> Result<u64, FitsWriteError> {
    let bitpix = int_of(cards, "BITPIX").ok_or_else(|| malformed("an HDU without BITPIX"))?;
    let naxis = int_of(cards, "NAXIS").unwrap_or(0);
    if naxis <= 0 {
        return Ok(0);
    }
    let mut n: u64 = 1;
    for k in 1..=naxis {
        let v = int_of(cards, &format!("NAXIS{k}")).ok_or_else(|| malformed(format!("NAXIS{k} missing")))?;
        n = n.checked_mul(v.max(0) as u64).ok_or_else(|| malformed("NAXISn overflow"))?;
    }
    let pcount = int_of(cards, "PCOUNT").unwrap_or(0).max(0) as u64;
    let gcount = int_of(cards, "GCOUNT").unwrap_or(1).max(1) as u64;
    let bytes = (bitpix.unsigned_abs() / 8) * gcount * (pcount + n);
    Ok(bytes.div_ceil(BLOCK_SIZE as u64) * BLOCK_SIZE as u64)
}

/// Every HDU of the file, plus where the last one ends and the file length.
/// Bytes after the last HDU that do not start an `XTENSION` header are
/// "trailing" and copied verbatim by the patcher.
fn read_layouts(f: &mut File) -> Result<(Vec<HduLayout>, u64, u64), FitsWriteError> {
    let file_len = f.metadata()?.len();
    let mut layouts: Vec<HduLayout> = Vec::new();
    let mut offset = 0u64;
    while offset < file_len {
        f.seek(SeekFrom::Start(offset))?;
        let mut cards = Vec::new();
        let mut blocks = 0usize;
        let mut found_end = false;
        while !found_end {
            let mut block = [0u8; BLOCK_SIZE];
            if f.read_exact(&mut block).is_err() {
                if layouts.is_empty() {
                    return Err(malformed("truncated primary header"));
                }
                break;
            }
            blocks += 1;
            for chunk in block.chunks_exact(CARD_SIZE) {
                let mut rec = [0u8; CARD_SIZE];
                rec.copy_from_slice(chunk);
                if keyword_of(&rec) == "END" {
                    found_end = true;
                    break;
                }
                cards.push(rec);
            }
            if blocks > MAX_HEADER_BLOCKS {
                return Err(malformed("no END card within 64 header blocks"));
            }
        }
        let first = cards.first().map(keyword_of);
        if layouts.is_empty() {
            if first != Some("SIMPLE") {
                return Err(malformed("not a FITS file (no SIMPLE card)"));
            }
            if !found_end {
                return Err(malformed("primary header has no END card"));
            }
        } else if !found_end || first != Some("XTENSION") {
            break; // trailing bytes, not an HDU
        }
        let data_offset = offset + (blocks * BLOCK_SIZE) as u64;
        let data_len_padded = data_len_padded(&cards)?;
        layouts.push(HduLayout { cards, header_blocks: blocks, data_offset, data_len_padded });
        offset = data_offset + data_len_padded;
    }
    Ok((layouts, offset.min(file_len), file_len))
}

/// The HDU holding the image: the primary when it has axes, else the first
/// tile-compressed (`BINTABLE` + `ZIMAGE = T`) or `IMAGE` extension with axes.
fn image_hdu(layouts: &[HduLayout]) -> usize {
    let naxis = |h: &HduLayout| int_of(&h.cards, "NAXIS").unwrap_or(0);
    if layouts.first().is_some_and(|h| naxis(h) > 0) {
        return 0;
    }
    layouts
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, h)| {
            let x = value_of(&h.cards, "XTENSION").unwrap_or_default();
            let zimage = value_of(&h.cards, "ZIMAGE").is_some_and(|v| v == "T");
            (x == "BINTABLE" && zimage) || (x == "IMAGE" && naxis(h) > 0)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn apply_edits(cards: &[Record], edits: &[CardEdit], is_image: bool) -> Result<Vec<Record>, FitsWriteError> {
    let mut out = cards.to_vec();
    for edit in edits {
        match edit {
            CardEdit::Set(card) | CardEdit::SetIfPresent(card) => {
                let present = spans_of(&out, &card.keyword);
                let insert_ok = matches!(edit, CardEdit::Set(_)) && is_image;
                if present.is_empty() && !insert_ok {
                    continue;
                }
                let records = format_card(card)?;
                match present.first() {
                    Some(&(start, len)) => {
                        for &(s, l) in present.iter().skip(1).rev() {
                            out.drain(s..s + l);
                        }
                        out.splice(start..start + len, records);
                    }
                    None => out.extend(records),
                }
            }
            CardEdit::Remove(key) => {
                for &(s, l) in spans_of(&out, key).iter().rev() {
                    out.drain(s..s + l);
                }
            }
        }
    }
    Ok(out)
}

fn render_header(cards: &[Record]) -> Vec<u8> {
    let mut out = Vec::with_capacity((cards.len() + 1) * CARD_SIZE);
    for c in cards {
        out.extend_from_slice(c);
    }
    let mut end = [b' '; CARD_SIZE];
    end[..3].copy_from_slice(b"END");
    out.extend_from_slice(&end);
    let len = out.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE;
    out.resize(len, b' ');
    out
}

/// Refuse a patch whose destination IS the source (a landed replica must
/// never be rewritten in place). Only resolvable when `dest` exists.
pub(crate) fn refuse_same_file(src: &Path, dest: &Path) -> Result<(), FitsWriteError> {
    if let (Ok(a), Ok(b)) = (std::fs::canonicalize(src), std::fs::canonicalize(dest)) {
        if a == b {
            return Err(malformed(format!("refusing to patch {} onto itself", src.display())));
        }
    }
    Ok(())
}

/// A unique temp path beside `dest` (pid + sequence, as the writers use).
pub(crate) fn sibling_tmp(dest: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".tmp-patch.{}.{}", std::process::id(), seq));
    dest.with_file_name(name)
}

/// Copy `src` to `dest` with `edits` applied to the headers.
pub fn patch_cards(src: &Path, dest: &Path, edits: &[CardEdit]) -> Result<PatchReport, FitsWriteError> {
    refuse_same_file(src, dest)?;
    let mut f = File::open(src)?;
    let (layouts, hdus_end, file_len) = read_layouts(&mut f)?;
    let image = image_hdu(&layouts);
    let tmp = sibling_tmp(dest);
    let written = (|| -> Result<usize, FitsWriteError> {
        let mut w = BufWriter::new(File::create(&tmp)?);
        let mut grown = 0usize;
        for (i, h) in layouts.iter().enumerate() {
            let cards = apply_edits(&h.cards, edits, i == image)?;
            let header = render_header(&cards);
            grown += (header.len() / BLOCK_SIZE).saturating_sub(h.header_blocks);
            w.write_all(&header)?;
            f.seek(SeekFrom::Start(h.data_offset))?;
            let n = h.data_len_padded.min(file_len.saturating_sub(h.data_offset));
            std::io::copy(&mut Read::by_ref(&mut f).take(n), &mut w)?;
        }
        if hdus_end < file_len {
            f.seek(SeekFrom::Start(hdus_end))?;
            std::io::copy(&mut f, &mut w)?;
        }
        w.flush()?;
        Ok(grown)
    })();
    match written {
        Ok(grown_blocks) => {
            super::writer::rename_replace(&tmp, dest)?;
            Ok(PatchReport { hdus: layouts.len(), image_hdu: image, grown_blocks })
        }
        Err(e) => {
            if let Err(rm) = std::fs::remove_file(&tmp) {
                tracing::warn!(path = %tmp.display(), error = %rm, "header patch: temp file not removed");
            }
            Err(e)
        }
    }
}

/// The image HDU's value of `key`, else the primary's.
pub fn read_image_keyword(path: &Path, key: &str) -> Result<Option<String>, FitsWriteError> {
    let mut f = File::open(path)?;
    let (layouts, _, _) = read_layouts(&mut f)?;
    let image = image_hdu(&layouts);
    Ok(value_of(&layouts[image].cards, key).or_else(|| value_of(&layouts[0].cards, key)))
}
```

Then edit `fits_writer/mod.rs`: replace `mod stamp;` with `pub mod header_patch;`, delete `pub use stamp::stamp_extra_card;`, and `git rm crates/athenaeum-core/src/fits_writer/stamp.rs`.

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib fits_writer -- --test-threads=4`
Expected: all `header_patch` tests PASS; the other `fits_writer` tests still PASS.

- [ ] **Step 6: Commit**

```bash
rustfmt crates/athenaeum-core/src/fits_writer/header_patch.rs crates/athenaeum-core/src/fits_writer/mod.rs
git add -A crates/athenaeum-core/src/fits_writer
git commit -m "feat(fits): header patcher for a copy — replace or insert cards, multi-HDU, data streamed verbatim

Replaces the unused insert-only stamp_extra_card.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK"
```

---

### Task 2: XISF header patcher

**Files:**
- Create: `crates/athenaeum-core/src/fits_writer/xisf_patch.rs`
- Modify:
  - `crates/athenaeum-core/src/fits_writer/mod.rs` (`pub mod xisf_patch;`)
  - `crates/athenaeum-core/src/fits_writer/xisf_writer.rs` (`fn escape_xml` → `pub(crate) fn escape_xml`)

**Interfaces:**
- Consumes: `header_patch::{refuse_same_file, sibling_tmp}` (Task 1), `writer::rename_replace`, `xisf_writer::escape_xml`.
- Produces:
  - `pub enum XisfEdit { Keyword { name: String, value: String, comment: String, insert: bool }, PropertyIfPresent { id: String, value: String } }`. Here `value` is the FITS-formatted value (a string already in single quotes, e.g. `'R'`).
  - `pub struct XisfPatchReport { pub images: usize, pub shifted_by: u64 }`
  - `pub fn patch_xisf_keywords(src: &Path, dest: &Path, edits: &[XisfEdit]) -> Result<XisfPatchReport, FitsWriteError>`
  - `pub fn read_xisf_keyword(path: &Path, name: &str) -> Result<Option<String>, FitsWriteError>`

- [ ] **Step 1: Write the failing tests** (bottom of `xisf_patch.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::{write_xisf_f32, Card, CardValue};

    /// A monolithic XISF with the header region ending at 4096; `{POS}`,
    /// `{POS2}` and `{SIZE}` in `xml` are filled in; `data` (and `data2`)
    /// follow at 4096 (and right after).
    fn xisf(xml: &str, data: &[u8], data2: &[u8]) -> Vec<u8> {
        let pos2 = 4096 + data.len();
        let xml = xml
            .replace("{POS2}", &pos2.to_string())
            .replace("{POS}", "4096")
            .replace("{SIZE2}", &data2.len().to_string())
            .replace("{SIZE}", &data.len().to_string());
        let mut out = Vec::new();
        out.extend_from_slice(b"XISF0100");
        out.extend_from_slice(&((4096 - 16) as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(xml.as_bytes());
        out.resize(4096, b' ');
        out.extend_from_slice(data);
        out.extend_from_slice(data2);
        out
    }
    fn blocks(path: &Path) -> Vec<Vec<u8>> {
        let bytes = std::fs::read(path).unwrap();
        let mut f = File::open(path).unwrap();
        let (xml, _) = read_header(&mut f).unwrap();
        locations(&xml)
            .into_iter()
            .map(|(_, p, s)| bytes[p as usize..(p + s) as usize].to_vec())
            .collect()
    }
    fn filter_edit(v: &str, comment: &str) -> XisfEdit {
        XisfEdit::Keyword { name: "FILTER".into(), value: format!("'{v}'"), comment: comment.into(), insert: true }
    }

    #[test]
    fn sets_the_keyword_in_writer_output_and_keeps_the_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.xisf");
        let card = Card::new("FILTER", CardValue::Str("Red".into())).unwrap();
        write_xisf_f32(&src, 2, 2, 1, &[1.0, 2.0, 3.0, 4.0], &[card]).unwrap();
        let before = std::fs::read(&src).unwrap();
        let dest = dir.path().join("b.xisf");
        let report = patch_xisf_keywords(&src, &dest, &[filter_edit("R", "was 'Red'")]).unwrap();
        assert_eq!(report.shifted_by, 0);
        assert_eq!(read_xisf_keyword(&dest, "FILTER").unwrap().as_deref(), Some("R"));
        assert_eq!(blocks(&src), blocks(&dest));
        assert_eq!(std::fs::read(&src).unwrap(), before, "src is never written");
    }

    #[test]
    fn inserts_a_missing_keyword() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.xisf");
        write_xisf_f32(&src, 2, 2, 1, &[0.0; 4], &[]).unwrap();
        let dest = dir.path().join("b.xisf");
        patch_xisf_keywords(&src, &dest, &[filter_edit("Ha", "was unset")]).unwrap();
        assert_eq!(read_xisf_keyword(&dest, "FILTER").unwrap().as_deref(), Some("Ha"));
    }

    #[test]
    fn grows_past_the_first_attachment_and_shifts_every_block() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("two.xisf");
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xisf version="1.0" xmlns="<XISF namespace URI>">
  <Image geometry="2:2:1" sampleFormat="UInt8" location="attachment:{POS}:{SIZE}">
    <FITSKeyword name="FILTER" value="'Red'" comment=""/>
  </Image>
  <Property id="Extra:Blob" type="ByteArray" location="attachment:{POS2}:{SIZE2}"/>
</xisf>
"#;
        std::fs::write(&src, xisf(xml, &[1, 2, 3, 4], &[5, 6, 7])).unwrap();
        let dest = dir.path().join("b.xisf");
        let report = patch_xisf_keywords(&src, &dest, &[filter_edit("R", &"x".repeat(6000))]).unwrap();
        assert!(report.shifted_by >= 4096 && report.shifted_by % 4096 == 0, "{report:?}");
        assert_eq!(blocks(&src), blocks(&dest), "every block byte-identical at its new position");
        assert_eq!(read_xisf_keyword(&dest, "FILTER").unwrap().as_deref(), Some("R"));
        let text = crate::fits_parser::extract_xisf_header(&dest).unwrap();
        assert!(text.contains("FILTER"), "the app's own XISF reader still reads it: {text}");
    }

    #[test]
    fn updates_the_filter_property_in_both_forms() {
        let dir = tempfile::tempdir().unwrap();
        for prop in [
            r#"<Property id="Instrument:Filter:Name" type="String" value="Red"/>"#,
            r#"<Property id="Instrument:Filter:Name" type="String">Red</Property>"#,
        ] {
            let src = dir.path().join("p.xisf");
            let xml = format!(
                "<?xml version=\"1.0\"?>\n<xisf version=\"1.0\">\n  <Image geometry=\"1:1:1\" sampleFormat=\"UInt8\" location=\"attachment:{{POS}}:{{SIZE}}\"/>\n  {prop}\n</xisf>\n"
            );
            std::fs::write(&src, xisf(&xml, &[42], &[])).unwrap();
            let dest = dir.path().join("q.xisf");
            patch_xisf_keywords(
                &src,
                &dest,
                &[filter_edit("R", "was 'Red'"), XisfEdit::PropertyIfPresent { id: "Instrument:Filter:Name".into(), value: "R".into() }],
            )
            .unwrap();
            let mut f = File::open(&dest).unwrap();
            let (out, _) = read_header(&mut f).unwrap();
            assert!(out.contains(r#"id="Instrument:Filter:Name" type="String" value="R""#), "{out}");
            assert!(!out.contains(">Red<"));
            assert_eq!(read_xisf_keyword(&dest, "FILTER").unwrap().as_deref(), Some("R"), "self-closing Image got the keyword");
            assert_eq!(blocks(&src), blocks(&dest));
        }
    }

    #[test]
    fn a_distributed_xisf_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("d.xisf");
        let xml = "<?xml version=\"1.0\"?>\n<xisf version=\"1.0\">\n  <Image geometry=\"1:1:1\" sampleFormat=\"UInt8\" location=\"path(@header_dir/d.xisb):0:1\"/>\n</xisf>\n";
        std::fs::write(&src, xisf(xml, &[], &[])).unwrap();
        let err = patch_xisf_keywords(&src, &dir.path().join("e.xisf"), &[filter_edit("R", "")]).unwrap_err();
        assert!(err.to_string().contains("distributed XISF"), "{err}");
    }

    #[test]
    fn entities_round_trip_and_same_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.xisf");
        write_xisf_f32(&src, 1, 1, 1, &[0.0], &[]).unwrap();
        let dest = dir.path().join("b.xisf");
        patch_xisf_keywords(&src, &dest, &[filter_edit("O&III<1>", "was \"x\"")]).unwrap();
        assert_eq!(read_xisf_keyword(&dest, "FILTER").unwrap().as_deref(), Some("O&III<1>"));
        assert!(patch_xisf_keywords(&src, &src, &[]).unwrap_err().to_string().contains("onto itself"));
    }
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib fits_writer::xisf_patch -- --test-threads=4`
Expected: compile errors.

- [ ] **Step 3: Implement `xisf_patch.rs`**

```rust
//! XISF header patching of a COPY (collab v3 wave 6, spec §10.4; plan P5).
//!
//! Monolithic XISF 1.0 only: `XISF0100`, a u32 LE header length, 4 reserved
//! bytes, the XML header, then the attached data blocks at the absolute
//! positions the header's `location="attachment:<pos>:<size>"` attributes
//! name. The XML text is SPLICED at the parser's byte offsets, so every byte
//! outside the edited elements stays identical. When the new header no
//! longer fits before the first block, every block shifts by the smallest
//! multiple of 4096 that makes room and every `location` is rewritten
//! (iterated to a fixed point — the digit count changes the length). Data
//! blocks are copied verbatim (compression and block checksums untouched).
//! A distributed XISF (`path(…)`/`url(…)` locations) is refused.
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::card::FitsWriteError;
use super::header_patch::{refuse_same_file, sibling_tmp};
use super::xisf_writer::escape_xml;

const SIGNATURE: &[u8; 8] = b"XISF0100";
const PREFIX: usize = 16;
const ALIGN: u64 = 4096;
const MAX_HEADER: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum XisfEdit {
    /// In every `<Image>`: replace the first `<FITSKeyword name=…>` (later
    /// duplicates dropped); insert it as the Image's first child when absent
    /// and `insert` is true. `value` is FITS-formatted (`'R'` for a string).
    Keyword { name: String, value: String, comment: String, insert: bool },
    /// Replace every String `<Property id=…>`; never inserted.
    PropertyIfPresent { id: String, value: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XisfPatchReport {
    pub images: usize,
    pub shifted_by: u64,
}

struct ImageSite {
    start_tag_end: usize,
    self_closing: Option<Range<usize>>,
    keywords: Vec<(String, Range<usize>)>,
}

struct Parsed {
    images: Vec<ImageSite>,
    properties: Vec<(String, Range<usize>)>,
}

fn malformed(msg: impl Into<String>) -> FitsWriteError {
    FitsWriteError::Malformed(msg.into())
}

/// XML attribute text → plain text (the five named entities and `&#…;`).
fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let ent = &tail[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=semi]),
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

fn attr(e: &BytesStart<'_>, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .map(|a| xml_unescape(&String::from_utf8_lossy(&a.value)))
}

fn check_location(e: &BytesStart<'_>) -> Result<(), FitsWriteError> {
    if let Some(loc) = attr(e, b"location") {
        let l = loc.trim_start();
        if l.starts_with("path(") || l.starts_with("url(") {
            return Err(malformed("distributed XISF (data blocks in external files) is not supported"));
        }
    }
    Ok(())
}

fn parse(xml: &str) -> Result<Parsed, FitsWriteError> {
    let mut reader = Reader::from_str(xml);
    let mut images: Vec<ImageSite> = Vec::new();
    let mut properties = Vec::new();
    let mut in_image: Option<usize> = None;
    let mut open_property: Option<(String, usize)> = None;
    loop {
        let start = reader.buffer_position() as usize;
        let ev = reader.read_event().map_err(|e| malformed(format!("XISF header XML: {e}")))?;
        let end = reader.buffer_position() as usize;
        match ev {
            Event::Start(e) => {
                check_location(&e)?;
                match e.name().as_ref() {
                    b"Image" => {
                        images.push(ImageSite { start_tag_end: end, self_closing: None, keywords: Vec::new() });
                        in_image = Some(images.len() - 1);
                    }
                    b"Property" => open_property = attr(&e, b"id").map(|id| (id, start)),
                    _ => {}
                }
            }
            Event::Empty(e) => {
                check_location(&e)?;
                match e.name().as_ref() {
                    b"Image" => images.push(ImageSite { start_tag_end: end, self_closing: Some(start..end), keywords: Vec::new() }),
                    b"FITSKeyword" => {
                        if let (Some(i), Some(name)) = (in_image, attr(&e, b"name")) {
                            images[i].keywords.push((name, start..end));
                        }
                    }
                    b"Property" => {
                        if let Some(id) = attr(&e, b"id") {
                            properties.push((id, start..end));
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"Image" => in_image = None,
                b"Property" => {
                    if let Some((id, s)) = open_property.take() {
                        properties.push((id, s..end));
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(Parsed { images, properties })
}

fn keyword_element(name: &str, value: &str, comment: &str) -> String {
    format!(
        "<FITSKeyword name=\"{}\" value=\"{}\" comment=\"{}\"/>",
        escape_xml(name),
        escape_xml(value),
        escape_xml(comment)
    )
}

fn property_element(id: &str, value: &str) -> String {
    format!("<Property id=\"{}\" type=\"String\" value=\"{}\"/>", escape_xml(id), escape_xml(value))
}

fn apply(xml: &str, parsed: &Parsed, edits: &[XisfEdit]) -> String {
    let mut splices: Vec<(Range<usize>, String)> = Vec::new();
    for img in &parsed.images {
        let mut inserted: Vec<String> = Vec::new();
        for edit in edits {
            let XisfEdit::Keyword { name, value, comment, insert } = edit else { continue };
            let found: Vec<&Range<usize>> = img.keywords.iter().filter(|(n, _)| n == name).map(|(_, r)| r).collect();
            match found.split_first() {
                Some((first, rest)) => {
                    splices.push(((*first).clone(), keyword_element(name, value, comment)));
                    for r in rest {
                        splices.push(((*r).clone(), String::new()));
                    }
                }
                None if *insert => inserted.push(keyword_element(name, value, comment)),
                None => {}
            }
        }
        if !inserted.is_empty() {
            let children: String = inserted.iter().map(|el| format!("\n    {el}")).collect();
            match &img.self_closing {
                Some(r) => {
                    let tag = &xml[r.clone()];
                    let open = tag.trim_end().strip_suffix("/>").unwrap_or(tag).trim_end();
                    splices.push((r.clone(), format!("{open}>{children}\n  </Image>")));
                }
                None => splices.push((img.start_tag_end..img.start_tag_end, children)),
            }
        }
    }
    for edit in edits {
        if let XisfEdit::PropertyIfPresent { id, value } = edit {
            for (pid, r) in &parsed.properties {
                if pid == id {
                    splices.push((r.clone(), property_element(id, value)));
                }
            }
        }
    }
    splices.sort_by(|a, b| b.0.start.cmp(&a.0.start));
    let mut out = xml.to_string();
    for (r, text) in splices {
        out.replace_range(r, &text);
    }
    out
}

/// `(range of the attribute value, pos, size)` of every attachment location.
fn locations(xml: &str) -> Vec<(Range<usize>, u64, u64)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = xml[from..].find("location=") {
        let at = from + i + "location=".len();
        from = at;
        let Some(q) = xml[at..].chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
        let body_start = at + 1;
        let Some(len) = xml[body_start..].find(q) else { continue };
        let body = &xml[body_start..body_start + len];
        if let Some(rest) = body.strip_prefix("attachment:") {
            let mut parts = rest.split(':');
            if let (Some(Ok(p)), Some(Ok(s))) = (parts.next().map(|p| p.trim().parse()), parts.next().map(|s| s.trim().parse())) {
                out.push((body_start..body_start + len, p, s));
            }
        }
        from = body_start + len;
    }
    out
}

fn shift(xml: &str, delta: u64) -> String {
    let mut out = xml.to_string();
    for (r, p, s) in locations(xml).into_iter().rev() {
        out.replace_range(r, &format!("attachment:{}:{}", p + delta, s));
    }
    out
}

/// The XML header (trailing blanks/NULs trimmed) and the stored header length.
fn read_header(f: &mut File) -> Result<(String, usize), FitsWriteError> {
    let mut prefix = [0u8; PREFIX];
    f.read_exact(&mut prefix)?;
    if &prefix[..8] != SIGNATURE {
        return Err(malformed("not a monolithic XISF 1.0 file"));
    }
    let len = u32::from_le_bytes(prefix[8..12].try_into().expect("4 bytes")) as usize;
    if len > MAX_HEADER {
        return Err(malformed("XISF header length exceeds 64 MiB"));
    }
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)?;
    let text = String::from_utf8(buf).map_err(|e| malformed(format!("XISF header is not UTF-8: {e}")))?;
    let trimmed = text.trim_end_matches(|c: char| c == '\0' || c.is_whitespace()).to_string();
    Ok((trimmed, len))
}

/// Copy `src` to `dest` with `edits` applied to the XML header.
pub fn patch_xisf_keywords(src: &Path, dest: &Path, edits: &[XisfEdit]) -> Result<XisfPatchReport, FitsWriteError> {
    refuse_same_file(src, dest)?;
    let mut f = File::open(src)?;
    let (xml, old_len) = read_header(&mut f)?;
    let parsed = parse(&xml)?;
    let edited = apply(&xml, &parsed, edits);
    let first_block = locations(&xml).iter().map(|(_, p, _)| *p).min();
    let (out_xml, region, copy_from, delta) = match first_block {
        None => {
            let region = (PREFIX + edited.len()) as u64;
            (edited, region, (PREFIX + old_len) as u64, 0)
        }
        Some(first) => {
            let mut delta = 0u64;
            let mut out = shift(&edited, 0);
            for _ in 0..8 {
                out = shift(&edited, delta);
                let need = (PREFIX + out.len()) as u64;
                if need <= first + delta {
                    break;
                }
                delta = (need - first).div_ceil(ALIGN) * ALIGN;
            }
            if (PREFIX + out.len()) as u64 > first + delta {
                return Err(malformed("XISF header shift did not converge"));
            }
            (out, first + delta, first, delta)
        }
    };
    let header_len = u32::try_from(region - PREFIX as u64).map_err(|_| malformed("XISF header too large"))?;
    let tmp = sibling_tmp(dest);
    let written = (|| -> Result<(), FitsWriteError> {
        let mut w = BufWriter::new(File::create(&tmp)?);
        w.write_all(SIGNATURE)?;
        w.write_all(&header_len.to_le_bytes())?;
        w.write_all(&[0u8; 4])?;
        w.write_all(out_xml.as_bytes())?;
        let pad = region as usize - PREFIX - out_xml.len();
        w.write_all(&vec![b' '; pad])?;
        f.seek(SeekFrom::Start(copy_from))?;
        std::io::copy(&mut f, &mut w)?;
        w.flush()?;
        Ok(())
    })();
    match written {
        Ok(()) => {
            super::writer::rename_replace(&tmp, dest)?;
            Ok(XisfPatchReport { images: parsed.images.len(), shifted_by: delta })
        }
        Err(e) => {
            if let Err(rm) = std::fs::remove_file(&tmp) {
                tracing::warn!(path = %tmp.display(), error = %rm, "xisf patch: temp file not removed");
            }
            Err(e)
        }
    }
}

/// The first `<Image>`'s `<FITSKeyword name=…>` value (quotes stripped,
/// `''` → `'`, trailing blanks trimmed).
pub fn read_xisf_keyword(path: &Path, name: &str) -> Result<Option<String>, FitsWriteError> {
    let mut f = File::open(path)?;
    let (xml, _) = read_header(&mut f)?;
    let parsed = parse(&xml)?;
    let Some(img) = parsed.images.first() else { return Ok(None) };
    let Some((_, r)) = img.keywords.iter().find(|(n, _)| n == name) else { return Ok(None) };
    let mut reader = Reader::from_str(&xml[r.clone()]);
    if let Ok(Event::Empty(e)) = reader.read_event() {
        if let Some(v) = attr(&e, b"value") {
            let v = v.trim();
            let v = match v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
                Some(inner) => inner.replace("''", "'"),
                None => v.to_string(),
            };
            return Ok(Some(v.trim_end().to_string()));
        }
    }
    Ok(None)
}
```

Note for the implementer: `Reader::buffer_position()` returns `u64` in quick-xml 0.41; the `as usize` casts above are intended. If `read_event()` is not available on a `Reader<&[u8]>` built by `from_str` in 0.41, use `read_event_into(&mut Vec::new())` and keep the same position bookkeeping.

- [ ] **Step 4: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib fits_writer -- --test-threads=4`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/fits_writer/xisf_patch.rs crates/athenaeum-core/src/fits_writer/mod.rs crates/athenaeum-core/src/fits_writer/xisf_writer.rs
git add crates/athenaeum-core/src/fits_writer
git commit -m "feat(fits): XISF header patcher for a copy — splice keywords, shift every attachment when the header grows

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK"
```

---

### Task 3: The FILTER rule

**Files:**
- Create: `crates/athenaeum-core/src/export/filter_header.rs`
- Modify: `crates/athenaeum-core/src/export/mod.rs` (`pub mod filter_header;`)

**Interfaces:**
- Consumes: Task 1 `patch_cards`, `read_image_keyword`, `CardEdit`; Task 2 `patch_xisf_keywords`, `read_xisf_keyword`, `XisfEdit`.
- Produces:
  - `pub enum ImageFileKind { Fits, Xisf }`
  - `pub fn sniff_kind(path: &Path) -> anyhow::Result<ImageFileKind>`
  - `pub fn read_file_filter(path: &Path) -> anyhow::Result<(ImageFileKind, Option<String>)>`
  - `pub fn needs_rewrite(actual: Option<&str>, canonical: &str) -> bool`
  - `pub fn write_filter_copy(src: &Path, dest: &Path, kind: ImageFileKind, canonical: &str, actual: Option<&str>) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::header_patch::read_image_keyword;
    use crate::fits_writer::{write_fits_f32, write_xisf_f32, Card, CardValue};

    fn card(k: &str, v: &str) -> Card {
        Card::new(k, CardValue::Str(v.into())).unwrap()
    }

    #[test]
    fn the_rule_compares_exactly_after_trimming() {
        assert!(!needs_rewrite(Some("R"), "R"));
        assert!(!needs_rewrite(Some("R   "), "R"));
        assert!(needs_rewrite(Some("r"), "R"), "case-sensitive");
        assert!(needs_rewrite(Some("Red"), "R"));
        assert!(needs_rewrite(None, "None"), "an absent card differs from the canonical None");
        assert!(!needs_rewrite(None, ""));
    }

    #[test]
    fn a_fits_copy_carries_the_canonical_filter_and_the_raw_in_the_comment() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        write_fits_f32(&src, 2, 2, 1, &[0.0; 4], &[card("FILTER", "Red"), card("ATH_FILT", "Red"), Card::new("CHECKSUM", CardValue::Str("x".into())).unwrap()]).unwrap();
        let (kind, actual) = read_file_filter(&src).unwrap();
        assert_eq!((kind, actual.as_deref()), (ImageFileKind::Fits, Some("Red")));
        let dest = dir.path().join("b.fits");
        write_filter_copy(&src, &dest, kind, "R", actual.as_deref()).unwrap();
        assert_eq!(read_image_keyword(&dest, "FILTER").unwrap().as_deref(), Some("R"));
        assert_eq!(read_image_keyword(&dest, "ATH_FILT").unwrap().as_deref(), Some("R"));
        let head = String::from_utf8_lossy(&std::fs::read(&dest).unwrap()[..2880]).into_owned();
        assert!(head.contains("/ was 'Red'"), "{head}");
        assert!(!head.contains("CHECKSUM"));
    }

    #[test]
    fn an_overlong_comment_is_truncated_never_the_value() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        let raw = "R".repeat(60);
        write_fits_f32(&src, 2, 2, 1, &[0.0; 4], &[card("FILTER", &raw)]).unwrap();
        let dest = dir.path().join("b.fits");
        write_filter_copy(&src, &dest, ImageFileKind::Fits, "Red-Long-Canonical-Name", Some(&raw)).unwrap();
        assert_eq!(read_image_keyword(&dest, "FILTER").unwrap().as_deref(), Some("Red-Long-Canonical-Name"));
    }

    #[test]
    fn an_xisf_copy_carries_the_canonical_filter() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.xisf");
        write_xisf_f32(&src, 1, 1, 1, &[0.0], &[card("FILTER", "H-alpha")]).unwrap();
        let (kind, actual) = read_file_filter(&src).unwrap();
        assert_eq!(kind, ImageFileKind::Xisf);
        let dest = dir.path().join("b.xisf");
        write_filter_copy(&src, &dest, kind, "Ha", actual.as_deref()).unwrap();
        assert_eq!(read_file_filter(&dest).unwrap().1.as_deref(), Some("Ha"));
    }

    #[test]
    fn an_unknown_file_kind_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.bin");
        std::fs::write(&p, b"not an image").unwrap();
        assert!(read_file_filter(&p).is_err());
    }
}
```

- [ ] **Step 2: Run, see the compile failure**

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib export::filter_header -- --test-threads=4`

- [ ] **Step 3: Implement**

```rust
//! The project export's FILTER rule (collab v3 wave 6, spec §10.2, W4):
//! every exported project file carries its frame's canonical filter in the
//! `FILTER` card. A file whose own card differs (or is absent) is exported
//! as a patched COPY; the source is only ever read.
use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

use crate::fits_writer::card::format_card;
use crate::fits_writer::header_patch::{patch_cards, read_image_keyword, CardEdit};
use crate::fits_writer::xisf_patch::{patch_xisf_keywords, read_xisf_keyword, XisfEdit};
use crate::fits_writer::{Card, CardValue, FitsWriteError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFileKind {
    Fits,
    Xisf,
}

/// The container, from the first bytes (never the extension).
pub fn sniff_kind(path: &Path) -> Result<ImageFileKind> {
    let mut head = [0u8; 8];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .with_context(|| format!("read {}", path.display()))?;
    match &head {
        b"XISF0100" => Ok(ImageFileKind::Xisf),
        b"SIMPLE  " => Ok(ImageFileKind::Fits),
        _ => Err(anyhow!("{} is neither FITS nor XISF", path.display())),
    }
}

/// The file's own `FILTER` (the image HDU's, else the primary's; XISF: the first Image's).
pub fn read_file_filter(path: &Path) -> Result<(ImageFileKind, Option<String>)> {
    let kind = sniff_kind(path)?;
    let value = match kind {
        ImageFileKind::Fits => read_image_keyword(path, "FILTER"),
        ImageFileKind::Xisf => read_xisf_keyword(path, "FILTER"),
    }
    .map_err(|e| anyhow!("{}: {e}", path.display()))?;
    Ok((kind, value))
}

/// W4: exact, case-sensitive, after trimming; an absent card is "".
pub fn needs_rewrite(actual: Option<&str>, canonical: &str) -> bool {
    actual.map(str::trim).unwrap_or("") != canonical
}

fn was_comment(actual: Option<&str>) -> String {
    match actual.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => format!("was '{raw}'"),
        None => "was unset".to_string(),
    }
}

/// A string card whose comment is shortened (never the value) until the
/// record(s) fit 80 columns.
fn fitting_card(key: &str, value: &str, comment: &str) -> Result<Card> {
    let mut comment: Vec<char> = comment.chars().collect();
    loop {
        let text: String = comment.iter().collect();
        let mut card = Card::new(key, CardValue::Str(value.to_string()))?;
        if !text.is_empty() {
            card = card.with_comment(&text);
        }
        match format_card(&card) {
            Ok(_) => return Ok(card),
            Err(FitsWriteError::CommentTooLong(_)) if !comment.is_empty() => {
                let keep = comment.len().saturating_sub(4);
                comment.truncate(keep);
                if keep > 0 {
                    comment.extend("...".chars());
                }
            }
            Err(e) => return Err(anyhow!("{key} card: {e}")),
        }
    }
}

/// Write `dest` as a copy of `src` whose FILTER is `canonical` (ATH_FILT too
/// when present; a FITS CHECKSUM dropped — DATASUM stays valid).
pub fn write_filter_copy(src: &Path, dest: &Path, kind: ImageFileKind, canonical: &str, actual: Option<&str>) -> Result<()> {
    let comment = was_comment(actual);
    match kind {
        ImageFileKind::Fits => {
            let edits = [
                CardEdit::Set(fitting_card("FILTER", canonical, &comment)?),
                CardEdit::SetIfPresent(Card::new("ATH_FILT", CardValue::Str(canonical.to_string()))?),
                CardEdit::Remove("CHECKSUM".to_string()),
            ];
            patch_cards(src, dest, &edits).map(|_| ())
        }
        ImageFileKind::Xisf => {
            let quoted = format!("'{}'", canonical.replace('\'', "''"));
            let edits = [
                XisfEdit::Keyword { name: "FILTER".into(), value: quoted.clone(), comment, insert: true },
                XisfEdit::Keyword { name: "ATH_FILT".into(), value: quoted, comment: String::new(), insert: false },
                XisfEdit::PropertyIfPresent { id: "Instrument:Filter:Name".into(), value: canonical.to_string() },
            ];
            patch_xisf_keywords(src, dest, &edits).map(|_| ())
        }
    }
    .map_err(|e| anyhow!("{}: {e}", src.display()))
}
```

Check before relying on it: `Card::new` returns `Result<Card, FitsWriteError>`, which converts to `anyhow::Error`; `FitsWriteError` implements `std::error::Error`.

- [ ] **Step 4: Run the tests** — `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib export::filter_header -- --test-threads=4` → PASS.

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/export/filter_header.rs crates/athenaeum-core/src/export/mod.rs
git add crates/athenaeum-core/src/export
git commit -m "feat(export): the project FILTER rule — read the file's card, write a patched copy

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YEaoHWL2V7MEXaXHgsiVwK"
```

---

### Task 4: Reshape the project collector

**Files:**
- Modify:
  - `crates/athenaeum-core/src/export/project_collector.rs` (whole module body and its tests)
  - `crates/athenaeum-core/src/export/mod.rs` (re-export `ProjectExportFrame`)

**Interfaces:**
- Produces:
  - `pub struct ProjectExportFrame { pub frame_uuid: String, pub landed_path: String, pub file_name: String, pub instrume: Option<String>, pub filter: String, pub byte_size: i64 }`
  - `pub struct ProjectExportData { pub title: String, pub publishers: Vec<(String, Vec<ProjectExportFrame>)>, pub warnings: Vec<String> }`
  - unchanged signature: `pub fn collect_project_export_data(conn: &Connection, project_id: &str, own_display: &str) -> Result<ProjectExportData>`

- [ ] **Step 1: Rewrite the tests to the new shape, and add the canonical-filter test**

In the module's `tests`, keep `test_conn`, `seed_project`, `mk_frame` and `manifest`. Replace `find` and `all_frames` with:
```rust
fn find<'a>(d: &'a ProjectExportData, display: &str) -> &'a [ProjectExportFrame] {
    &d.publishers.iter().find(|(p, _)| p == display).expect("publisher present").1
}
```
Update every existing test to assert on `ProjectExportFrame` fields (`landed_path`, `file_name`, `instrume`, `filter`, `byte_size`) instead of `ExportData` groups. Then add:
```rust
#[test]
fn the_filter_is_the_row_canonical_never_filter_raw() {
    let conn = test_conn();
    seed_project(&conn, "p");
    let mut row = mk_frame(&conn, "p", "u1", FrameOrigin::Replica, "Alice", Some("/c/a.fits"), manifest("Red", "CamA"));
    row.filter_canonical = "R".into();
    crate::db::collab_frames::record_own(&conn, &row).unwrap();
    let d = collect_project_export_data(&conn, "p", "Me").unwrap();
    let f = &find(&d, "Alice")[0];
    assert_eq!(f.filter, "R");
    assert_eq!(f.instrume.as_deref(), Some("CamA"));
    assert_eq!(f.landed_path, "/c/a.fits");
    assert_eq!(f.file_name, "a.fits");
    assert_eq!(f.frame_uuid, "u1");
}
```
If `mk_frame` persists through `record_own` already and sets `filter_canonical` from the manifest, give it a `filter_canonical` parameter rather than writing the row twice. Keep the helper's other behaviour.

- [ ] **Step 2: Run, see failures** — `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib export::project_collector -- --test-threads=4`.

- [ ] **Step 3: Replace the module body**

Keep the module doc, extended with: "Wave 6 (spec §10.1): the filter is the row's canonical (remaps included), never `filterRaw`." Keep `exportable`, `basename`, `meta_str`. Drop `export_frame`, `add_frame`, `build_dataset`, `meta_f64` and the `ExportData`/`ExportFrame`/`ExportGroup`/`CalibrationSubgroup`/`CalibrationSummary`/`CameraType`/`MasterCreationPlan` imports.

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectExportFrame {
    pub frame_uuid: String,
    pub landed_path: String,
    /// The landed file's basename (the export keeps it).
    pub file_name: String,
    pub instrume: Option<String>,
    /// The canonical filter (`project_frames_local.filter_canonical`).
    pub filter: String,
    /// The hub's byte size of the current content version.
    pub byte_size: i64,
}

#[derive(Debug, Clone)]
pub struct ProjectExportData {
    pub title: String,
    /// `(publisher display, frames)`, own first, then replica publishers in first-seen order.
    pub publishers: Vec<(String, Vec<ProjectExportFrame>)>,
    pub warnings: Vec<String>,
}

pub fn collect_project_export_data(conn: &Connection, project_id: &str, own_display: &str) -> Result<ProjectExportData> {
    let project = get_project(conn, project_id)?.ok_or_else(|| anyhow!("project not in the local cache"))?;
    let mut rows: Vec<LocalFrameRow> = list_for_project(conn, project_id)?.into_iter().filter(exportable).collect();
    rows.sort_by_key(|r| r.origin != FrameOrigin::Own);
    let mut order: Vec<String> = Vec::new();
    let mut by_display: HashMap<String, Vec<ProjectExportFrame>> = HashMap::new();
    let mut warnings = Vec::new();
    for row in rows {
        let display = match row.origin {
            FrameOrigin::Own => own_display.to_string(),
            FrameOrigin::Replica => row.publisher_display.clone(),
        };
        let manifest: serde_json::Value = match serde_json::from_str(&row.manifest_json) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(project_id, frame_uuid = %row.frame_uuid, error = %e, "project export: manifest row is not valid json; frame skipped");
                warnings.push(format!("skipped {}: unreadable frame metadata", row.file_name));
                continue;
            }
        };
        let landed = row.landed_path.clone().unwrap_or_default();
        let meta = manifest.get("meta").cloned().unwrap_or(serde_json::Value::Null);
        let frame = ProjectExportFrame {
            frame_uuid: row.frame_uuid.clone(),
            file_name: basename(&landed),
            landed_path: landed,
            instrume: meta_str(&meta, "instrume"),
            filter: row.filter_canonical.clone(),
            byte_size: row.byte_size,
        };
        if !by_display.contains_key(&display) {
            order.push(display.clone());
        }
        by_display.entry(display).or_default().push(frame);
    }
    if order.is_empty() {
        return Err(anyhow!("nothing to export for this project"));
    }
    let publishers: Vec<(String, Vec<ProjectExportFrame>)> = order
        .into_iter()
        .map(|d| {
            let frames = by_display.remove(&d).unwrap_or_default();
            (d, frames)
        })
        .collect();
    tracing::info!(project_id, publishers = publishers.len(), skipped = warnings.len(), "collected project export data");
    Ok(ProjectExportData { title: project.title, publishers, warnings })
}
```

`export/mod.rs`: re-export `ProjectExportFrame` beside the existing `collect_project_export_data, ProjectExportData`.

- [ ] **Step 4: Check the build**

`api/collab_exchange.rs`'s runner still expects `ExportData` and stops compiling here; Task 6 rewrites it. To keep this task's commit compiling, change the runner's loop in this task to a stub that returns `Err(ApiError::Internal("project export is being rebuilt (wave 6 Task 6)".into()))`, and mark `export_project_lays_per_publisher_tree` `#[ignore = "rebuilt in wave 6 Task 6"]`. Task 6 removes both.

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib export::project_collector -- --test-threads=4` → PASS; `CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core` → clean.

- [ ] **Step 5: Commit** — `refactor(export): the project collector returns frames with the canonical filter` (same trailers).

---

### Task 5: The project organizer

**Files:**
- Create: `crates/athenaeum-core/src/export/project_organizer.rs`
- Modify:
  - `crates/athenaeum-core/src/export/file_organizer.rs`: `struct DestClaims` and its `claim` method → `pub(crate)`.
  - `crates/athenaeum-core/src/export/mod.rs`: `pub mod project_organizer;`.

**Interfaces:**
- Consumes: Task 3 (`read_file_filter`, `needs_rewrite`, `write_filter_copy`), Task 4 (`ProjectExportFrame`), `DestClaims::claim`, `sanitize_display_folder_name`, `sanitize_folder_name`, `ExportProgressEvent`, `crate::fits_writer::writer::rename_replace`, `crate::fits_writer::header_patch::sibling_tmp`.
- Produces:
  - `pub const PROJECT_EXPORT_SENTINEL: i64 = -1;`
  - `pub const EXPORT_RECORD_FILE: &str = ".athenaeum-export.json";`
  - `pub struct PlaceCounts { pub linked: i32, pub copied: i32, pub rewritten: i32, pub failed: i32 }`
  - `pub struct ProjectOrganizeResult { pub counts: PlaceCounts, pub warnings: Vec<String>, pub cancelled: bool }`
  - `pub fn organize_project_export(title_dir: &Path, project_id: &str, publishers: &[(String, Vec<ProjectExportFrame>)], use_symlinks: bool, emitter: Option<&dyn ProgressEmitter>, cancel_flag: &AtomicBool) -> anyhow::Result<ProjectOrganizeResult>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::header_patch::read_image_keyword;
    use crate::fits_writer::{write_fits_f32, Card, CardValue};

    fn landed(dir: &Path, name: &str, filter: Option<&str>) -> (PathBuf, i64) {
        let p = dir.join("Collab").join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let cards: Vec<Card> = filter.map(|f| Card::new("FILTER", CardValue::Str(f.into())).unwrap()).into_iter().collect();
        write_fits_f32(&p, 2, 2, 1, &[1.0, 2.0, 3.0, 4.0], &cards).unwrap();
        let size = std::fs::metadata(&p).unwrap().len() as i64;
        (p, size)
    }
    fn frame(uuid: &str, path: &Path, size: i64, filter: &str) -> ProjectExportFrame {
        ProjectExportFrame {
            frame_uuid: uuid.into(),
            landed_path: path.to_string_lossy().into_owned(),
            file_name: path.file_name().unwrap().to_string_lossy().into_owned(),
            instrume: Some("CamA".into()),
            filter: filter.into(),
            byte_size: size,
        }
    }
    fn run(title: &Path, pubs: &[(String, Vec<ProjectExportFrame>)], links: bool) -> ProjectOrganizeResult {
        organize_project_export(title, "p1", pubs, links, None, &AtomicBool::new(false)).unwrap()
    }

    #[test]
    fn lays_out_by_publisher_camera_filter_and_rewrites_only_what_differs() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let (h, hs) = landed(tmp.path(), "h.fits", Some("H-alpha"));
        let r_before = std::fs::read(&r).unwrap();
        let h_before = std::fs::read(&h).unwrap();
        let title = tmp.path().join("out").join("T");
        let res = run(&title, &[("Alice".into(), vec![frame("u1", &r, rs, "R"), frame("u2", &h, hs, "Ha")])], false);
        assert_eq!(res.counts, PlaceCounts { linked: 0, copied: 1, rewritten: 1, failed: 0 });
        let rd = title.join("Alice/camera_cama/R/r.fits");
        let hd = title.join("Alice/camera_cama/Ha/h.fits");
        assert_eq!(std::fs::read(&rd).unwrap(), r_before, "an equal FILTER is copied as is");
        assert_eq!(read_image_keyword(&hd, "FILTER").unwrap().as_deref(), Some("Ha"));
        assert_eq!(std::fs::read(&h).unwrap(), h_before, "the landed file is never modified");
        assert!(title.join(EXPORT_RECORD_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_mode_links_the_unchanged_and_copies_the_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let (h, hs) = landed(tmp.path(), "h.fits", Some("Red"));
        let title = tmp.path().join("T");
        let res = run(&title, &[("Alice".into(), vec![frame("u1", &r, rs, "R"), frame("u2", &h, hs, "R2")])], true);
        assert_eq!(res.counts, PlaceCounts { linked: 1, copied: 0, rewritten: 1, failed: 0 });
        let rd = title.join("Alice/camera_cama/R/r.fits");
        assert_eq!(std::fs::read_link(&rd).unwrap(), r);
        let hd = title.join("Alice/camera_cama/R2/h.fits");
        assert!(!std::fs::symlink_metadata(&hd).unwrap().file_type().is_symlink());
    }

    /// Review Focus 1: a previous export left a SYMLINK to the replica at the
    /// destination; a copy-mode re-export must replace the link with a file
    /// and never write through it into the replica.
    #[cfg(unix)]
    #[test]
    fn copy_mode_replaces_a_previous_symlink_without_writing_through_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("Red"));
        let before = std::fs::read(&r).unwrap();
        let title = tmp.path().join("T");
        let pubs = vec![("Alice".to_string(), vec![frame("u1", &r, rs, "Red")])];
        run(&title, &pubs, true); // the file's FILTER equals the canonical → a symlink
        let dest = title.join("Alice/camera_cama/Red/r.fits");
        assert!(std::fs::symlink_metadata(&dest).unwrap().file_type().is_symlink());
        let res = run(&title, &pubs, false); // the user re-exports WITHOUT symlinks
        assert_eq!(res.counts.copied, 1);
        assert!(!std::fs::symlink_metadata(&dest).unwrap().file_type().is_symlink(), "the link became a file");
        assert_eq!(std::fs::read(&r).unwrap(), before, "the replica is untouched");
    }

    /// Review Focus 1, rewrite variant: a symlink to the replica sits where a
    /// FILTER-patched copy must now go; the patched copy replaces the link
    /// and the replica keeps its raw FILTER.
    #[cfg(unix)]
    #[test]
    fn a_patched_copy_replaces_a_symlink_without_touching_the_replica() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("Red"));
        let before = std::fs::read(&r).unwrap();
        let title = tmp.path().join("T");
        let dest_dir = title.join("Alice/camera_cama/R");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let dest = dest_dir.join("r.fits");
        std::os::unix::fs::symlink(&r, &dest).unwrap();
        let res = run(&title, &[("Alice".into(), vec![frame("u1", &r, rs, "R")])], false);
        assert_eq!(res.counts.rewritten, 1);
        assert!(!std::fs::symlink_metadata(&dest).unwrap().file_type().is_symlink());
        assert_eq!(read_image_keyword(&dest, "FILTER").unwrap().as_deref(), Some("R"));
        assert_eq!(std::fs::read(&r).unwrap(), before, "the replica keeps FILTER = 'Red'");
    }

    #[test]
    fn a_reexport_keeps_correct_files_and_replaces_wrong_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let title = tmp.path().join("T");
        let pubs = vec![("Alice".to_string(), vec![frame("u1", &r, rs, "R")])];
        run(&title, &pubs, false);
        let dest = title.join("Alice/camera_cama/R/r.fits");
        std::fs::write(&dest, b"truncated").unwrap();
        let res = run(&title, &pubs, false);
        assert_eq!(res.counts.copied, 1);
        assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&r).unwrap());
        assert!(res.warnings.is_empty(), "{:?}", res.warnings);
    }

    #[test]
    fn a_remap_moves_the_frame_and_the_old_copy_is_warned_never_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let (h, hs) = landed(tmp.path(), "h.fits", Some("Ha"));
        let title = tmp.path().join("T");
        run(&title, &[("Alice".into(), vec![frame("u1", &h, hs, "Ha")])], false);
        let old = title.join("Alice/camera_cama/Ha/h.fits");
        let res = run(&title, &[("Alice".into(), vec![frame("u1", &h, hs, "OIII")])], false);
        assert!(old.exists(), "never deleted (W6)");
        assert!(title.join("Alice/camera_cama/OIII/h.fits").exists());
        assert_eq!(res.warnings.len(), 1, "{:?}", res.warnings);
        assert!(res.warnings[0].contains("Alice/camera_cama/Ha/h.fits"));
        // A third, identical export warns about nothing.
        let res3 = run(&title, &[("Alice".into(), vec![frame("u1", &h, hs, "OIII")])], false);
        assert!(res3.warnings.is_empty(), "{:?}", res3.warnings);
    }

    #[test]
    fn a_size_mismatch_or_an_unknown_file_fails_only_that_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let junk = tmp.path().join("Collab/junk.fits");
        std::fs::write(&junk, b"junk").unwrap();
        let title = tmp.path().join("T");
        let res = run(
            &title,
            &[("Alice".into(), vec![frame("u1", &r, rs + 1, "R"), frame("u2", &junk, 4, "R"), frame("u3", &r, rs, "R")])],
            false,
        );
        assert_eq!(res.counts.failed, 2);
        assert_eq!(res.counts.copied, 1);
        assert!(res.warnings.iter().any(|w| w.contains("changed during export")), "{:?}", res.warnings);
    }

    #[test]
    fn a_cancel_before_the_first_frame_reports_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let flag = AtomicBool::new(true);
        let res = organize_project_export(&tmp.path().join("T"), "p1", &[("Alice".into(), vec![frame("u1", &r, rs, "R")])], false, None, &flag).unwrap();
        assert!(res.cancelled);
        assert_eq!(res.counts, PlaceCounts::default());
    }
}
```

- [ ] **Step 2: Run, see the compile failure** — `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib export::project_organizer -- --test-threads=4`.

- [ ] **Step 3: Implement `project_organizer.rs`**

```rust
//! The project export's organizer (collab v3 wave 6, spec §10; plan P1).
//!
//! Layout `<title>/<publisher>/camera_<instrume>/<canonical filter>/<file>`.
//! Per frame: a file whose own FILTER equals the canonical name is copied
//! (or symlinked in symlink mode); any other is written as a patched copy
//! (W4/W5). An existing destination is kept when it already is what this
//! export would write, else replaced via temp + rename — never written
//! THROUGH (a previous symlink is removed first, so a copy never lands in a
//! replica). `<title>/.athenaeum-export.json` records, per frame uuid, the
//! path placed; a re-export names paths no longer placed in ONE warning and
//! never deletes them (W6).
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};

use crate::events::{emit_event, ProgressEmitter};
use crate::export::file_organizer::DestClaims;
use crate::export::filter_header::{needs_rewrite, read_file_filter, write_filter_copy};
use crate::export::models::{sanitize_display_folder_name, sanitize_folder_name, ExportProgressEvent};
use crate::export::project_collector::ProjectExportFrame;
use crate::fits_writer::header_patch::sibling_tmp;

pub const PROJECT_EXPORT_SENTINEL: i64 = -1;
pub const EXPORT_RECORD_FILE: &str = ".athenaeum-export.json";
const STALE_LISTED: usize = 20;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaceCounts {
    pub linked: i32,
    pub copied: i32,
    pub rewritten: i32,
    pub failed: i32,
}

#[derive(Debug, Clone)]
pub struct ProjectOrganizeResult {
    pub counts: PlaceCounts,
    pub warnings: Vec<String>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placed {
    Linked,
    Copied,
    Rewritten,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportRecord {
    version: u32,
    project_id: String,
    /// frame uuid → path placed, relative to the title folder, `/`-separated.
    frames: BTreeMap<String, String>,
}

fn rel_dir(publisher: &str, f: &ProjectExportFrame) -> String {
    let camera = f
        .instrume
        .as_deref()
        .map(sanitize_folder_name)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let filter = if f.filter.trim().is_empty() { "unknown".to_string() } else { sanitize_display_folder_name(&f.filter) };
    format!("{}/camera_{camera}/{filter}", sanitize_display_folder_name(publisher))
}

fn is_symlink(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

fn copy_atomic(src: &Path, dest: &Path) -> Result<()> {
    let tmp = sibling_tmp(dest);
    if let Err(e) = fs::copy(src, &tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(anyhow!("copy {} → {}: {e}", src.display(), dest.display()));
    }
    crate::fits_writer::writer::rename_replace(&tmp, dest).with_context(|| format!("rename onto {}", dest.display()))
}

fn link_atomic(src: &Path, dest: &Path) -> Result<()> {
    let tmp = sibling_tmp(dest);
    #[cfg(unix)]
    std::os::unix::fs::symlink(src, &tmp).with_context(|| format!("symlink {}", src.display()))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(src, &tmp).with_context(|| format!("symlink {}", src.display()))?;
    crate::fits_writer::writer::rename_replace(&tmp, dest).with_context(|| format!("rename onto {}", dest.display()))
}

fn place_one(src: &Path, dest: &Path, canonical: &str, byte_size: i64, use_symlinks: bool) -> Result<Placed> {
    let len = fs::metadata(src).with_context(|| format!("stat {}", src.display()))?.len();
    if len as i64 != byte_size {
        return Err(anyhow!("changed during export (size {len} differs from the published {byte_size})"));
    }
    let (kind, actual) = read_file_filter(src)?;
    if !needs_rewrite(actual.as_deref(), canonical) {
        if use_symlinks {
            if is_symlink(dest) && fs::read_link(dest).is_ok_and(|t| t == src) {
                return Ok(Placed::Linked);
            }
            link_atomic(src, dest)?;
            return Ok(Placed::Linked);
        }
        if !is_symlink(dest) && fs::metadata(dest).is_ok_and(|m| m.len() == len) {
            return Ok(Placed::Copied);
        }
        if is_symlink(dest) {
            fs::remove_file(dest).with_context(|| format!("remove the old link {}", dest.display()))?;
        }
        copy_atomic(src, dest)?;
        let written = fs::metadata(dest)?.len();
        if written != len {
            return Err(anyhow!("changed during export (copied {written} of {len} bytes)"));
        }
        return Ok(Placed::Copied);
    }
    if !is_symlink(dest) && dest.exists() {
        if let Ok((_, Some(v))) = read_file_filter(dest) {
            if v == canonical {
                return Ok(Placed::Rewritten);
            }
        }
    }
    if is_symlink(dest) {
        fs::remove_file(dest).with_context(|| format!("remove the old link {}", dest.display()))?;
    }
    write_filter_copy(src, dest, kind, canonical, actual.as_deref())?;
    Ok(Placed::Rewritten)
}

fn read_record(title_dir: &Path) -> (ExportRecord, Option<String>) {
    let path = title_dir.join(EXPORT_RECORD_FILE);
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(r) => (r, None),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "project export: unreadable export record; treated as empty");
                (ExportRecord::default(), Some(format!("The previous export record could not be read ({e}); stale files are not listed this time.")))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (ExportRecord::default(), None),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "project export: export record not readable");
            (ExportRecord::default(), Some(format!("The previous export record could not be read ({e}).")))
        }
    }
}

fn write_record(title_dir: &Path, record: &ExportRecord) -> Result<()> {
    let path = title_dir.join(EXPORT_RECORD_FILE);
    let tmp = sibling_tmp(&path);
    fs::write(&tmp, serde_json::to_vec_pretty(record)?)?;
    crate::fits_writer::writer::rename_replace(&tmp, &path).with_context(|| format!("write {}", path.display()))
}

fn stale_warning(prev: &ExportRecord, placed: &BTreeMap<String, String>, cancelled: bool) -> Option<String> {
    let stale: Vec<&String> = prev
        .frames
        .iter()
        .filter(|(uuid, path)| match placed.get(*uuid) {
            Some(now) => now != *path,
            None => !cancelled,
        })
        .map(|(_, path)| path)
        .collect();
    if stale.is_empty() {
        return None;
    }
    let listed: Vec<&str> = stale.iter().take(STALE_LISTED).map(|s| s.as_str()).collect();
    let more = stale.len().saturating_sub(STALE_LISTED);
    let tail = if more > 0 { format!(" (+{more} more)") } else { String::new() };
    Some(format!(
        "{} file(s) from the previous export are no longer part of it and were left in place: {}{tail}",
        stale.len(),
        listed.join(", ")
    ))
}

pub fn organize_project_export(
    title_dir: &Path,
    project_id: &str,
    publishers: &[(String, Vec<ProjectExportFrame>)],
    use_symlinks: bool,
    emitter: Option<&dyn ProgressEmitter>,
    cancel_flag: &AtomicBool,
) -> Result<ProjectOrganizeResult> {
    let total: usize = publishers.iter().map(|(_, f)| f.len()).sum();
    let mut counts = PlaceCounts::default();
    let mut warnings = Vec::new();
    let mut placed: BTreeMap<String, String> = BTreeMap::new();
    let mut claims = DestClaims::default();
    let mut cancelled = false;
    let mut done = 0usize;
    let mut last_emit = Instant::now();
    fs::create_dir_all(title_dir).with_context(|| format!("create {}", title_dir.display()))?;
    let (prev, record_warning) = read_record(title_dir);
    warnings.extend(record_warning);
    'outer: for (publisher, frames) in publishers {
        for f in frames {
            if cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                break 'outer;
            }
            let dir = rel_dir(publisher, f);
            let name = claims.claim(&dir, &f.file_name);
            let rel = format!("{dir}/{name}");
            let dest_dir = dir.split('/').fold(title_dir.to_path_buf(), |p, c| p.join(c));
            fs::create_dir_all(&dest_dir).with_context(|| format!("create {}", dest_dir.display()))?;
            let dest = dest_dir.join(&name);
            match place_one(Path::new(&f.landed_path), &dest, &f.filter, f.byte_size, use_symlinks) {
                Ok(p) => {
                    match p {
                        Placed::Linked => counts.linked += 1,
                        Placed::Copied => counts.copied += 1,
                        Placed::Rewritten => counts.rewritten += 1,
                    }
                    placed.insert(f.frame_uuid.clone(), rel);
                }
                Err(e) => {
                    counts.failed += 1;
                    tracing::warn!(project_id, frame_uuid = %f.frame_uuid, path = %f.landed_path, error = %e, "project export: frame failed");
                    warnings.push(format!("Failed to export {}: {e:#}", f.file_name));
                }
            }
            done += 1;
            if let Some(e) = emitter {
                if done == total || last_emit.elapsed().as_millis() >= 100 {
                    last_emit = Instant::now();
                    let percent = if total > 0 { done as f64 / total as f64 * 100.0 } else { 100.0 };
                    emit_event(e, "export-progress", &ExportProgressEvent {
                        frame_set_id: PROJECT_EXPORT_SENTINEL,
                        current: done,
                        total,
                        percent,
                        current_file: Some(name.clone()),
                        phase: "copying".to_string(),
                    });
                }
            }
        }
    }
    warnings.extend(stale_warning(&prev, &placed, cancelled));
    let mut frames = if cancelled { prev.frames } else { BTreeMap::new() };
    frames.extend(placed);
    if let Err(e) = write_record(title_dir, &ExportRecord { version: 1, project_id: project_id.to_string(), frames }) {
        tracing::warn!(project_id, error = %e, "project export: export record not written");
        warnings.push(format!("The export record was not written ({e:#}); the next export cannot list stale files."));
    }
    tracing::info!(project_id, linked = counts.linked, copied = counts.copied, rewritten = counts.rewritten, failed = counts.failed, cancelled, "project export organized");
    Ok(ProjectOrganizeResult { counts, warnings, cancelled })
}
```

- [ ] **Step 4: Run** — `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib export:: -- --test-threads=4` → the new tests PASS and the existing `file_organizer` tests are unchanged and PASS.

- [ ] **Step 5: Commit** — `feat(export): project organizer — canonical filter folders, mixed symlink/copy, export record` (same trailers).

---

### Task 6: Runner, result counts, guard, dialog

**Files:**
- Modify:
  - `crates/athenaeum-core/src/export/models.rs` (`ExportResult`, `ExportCompleteEvent`)
  - every `ExportResult {` / `ExportCompleteEvent {` literal in the workspace
  - `crates/athenaeum-core/src/api/collab_exchange.rs` (`export_project_for_wbpp` and its tests)
  - generated `src/types/export.ts` (regenerated, never hand-edited)
  - `src/components/collab/ProjectExportDialog.tsx` + `ProjectExportDialog.test.tsx`
  - `src/hooks/useExportProgress.ts`

**Interfaces:**
- Consumes: Task 4 `collect_project_export_data`, Task 5 `organize_project_export`, `PROJECT_EXPORT_SENTINEL`.
- Produces: `ExportResult { …, linked: i32, copied: i32, rewritten: i32, failed: i32 }` and the same four fields on `ExportCompleteEvent`, all `#[serde(default)]`.

- [ ] **Step 1: Add the counts to the models**

In `export/models.rs`, append to both `ExportResult` and `ExportCompleteEvent`:
```rust
    /// Project export only (plan P11): files symlinked / copied as is /
    /// written as a FILTER-patched copy / failed. `0` for a frame-set export.
    #[serde(default)]
    pub linked: i32,
    #[serde(default)]
    pub copied: i32,
    #[serde(default)]
    pub rewritten: i32,
    #[serde(default)]
    pub failed: i32,
```
Then run `grep -rn -E "ExportResult \{|ExportCompleteEvent \{" crates/ --include=*.rs` and append `linked: 0, copied: 0, rewritten: 0, failed: 0,` to every literal outside the project runner, including the web host's early-return literal in `routes/collab.rs` and the frame-set export paths. Check with `CARGO_BUILD_JOBS=4 cargo check --workspace` → clean.

- [ ] **Step 2: Write the failing runner tests** (in `collab_exchange.rs` `mod tests`, replacing the `#[ignore]`d one from Task 4)

```rust
#[tokio::test]
async fn export_project_lays_canonical_filter_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
    let landed = tmp.path().join("Collab").join("Alice").join("L_0001.fits");
    std::fs::create_dir_all(landed.parent().unwrap()).unwrap();
    crate::fits_writer::write_fits_f32(&landed, 2, 2, 1, &[0.25, 0.5, 0.75, 1.0], &[]).unwrap();
    let size = std::fs::metadata(&landed).unwrap().len() as i64;
    {
        let db = db(&ctx).unwrap();
        let conn = db.conn();
        seed_project(&conn, "p-1", "[]");
        let view: crate::collab::hub_client::FrameViewWire = serde_json::from_value(serde_json::json!({
            "frameUuid": "u-1", "publisherAccountId": "acc-alice", "publisherDisplayName": "Alice", "own": false,
            "fileName": "L_0001.fits", "contentVersion": 1, "blake3": "b".repeat(64), "byteSize": size,
            "xxh3": "0123456789abcdef", "filterRaw": "Lum", "filterCanonical": "L", "channel": "mono",
            "exptimeSec": 300.0, "meta": {"instrume": "CamA"}, "gateVersion": 0, "accepted": true,
            "state": "published", "manifestVersion": 1, "createdAt": "2026-07-13T00:00:00Z"
        })).unwrap();
        crate::db::collab_frames::upsert_from_manifest(&conn, "p-1", &view).unwrap();
        crate::db::collab_frames::set_landed(&conn, "p-1", "u-1", &landed.to_string_lossy(), "1:1").unwrap();
    }
    let before = std::fs::read(&landed).unwrap();
    let out = tmp.path().join("out");
    let r = export_project_for_wbpp(&ctx, "p-1", &out.to_string_lossy(), false, None).await.unwrap();
    assert!(r.success, "{:?}", r.error);
    assert_eq!((r.files_organized, r.rewritten, r.failed), (1, 1, 0));
    let dest = out.join("T").join("Alice").join("camera_cama").join("L").join("L_0001.fits");
    assert_eq!(crate::fits_writer::header_patch::read_image_keyword(&dest, "FILTER").unwrap().as_deref(), Some("L"));
    assert_eq!(std::fs::read(&landed).unwrap(), before, "the replica is untouched");
}

#[tokio::test]
async fn a_second_project_export_while_one_runs_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
    ctx.active_exports.lock().unwrap().insert(
        crate::export::project_organizer::PROJECT_EXPORT_SENTINEL,
        crate::services::ExportHandle { cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)) },
    );
    let err = export_project_for_wbpp(&ctx, "p-1", "/tmp/x", false, None).await.unwrap_err();
    assert!(matches!(err, ApiError::Conflict(_)), "{err:?}");
    assert!(ctx.active_exports.lock().unwrap().contains_key(&-1), "the running export's handle is left alone");
}
```

- [ ] **Step 3: Run, see them fail** — `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib collab_exchange::tests::export_project -- --test-threads=4`, and the `a_second_project_export` test.

- [ ] **Step 4: Rewrite `export_project_for_wbpp`**

Replace the function body. Keep the doc and the signature. Use `crate::export::project_organizer::PROJECT_EXPORT_SENTINEL` and drop the local `SENTINEL` const. The changes:

1. Registration refuses a running export:
```rust
{
    let mut exports = ctx.active_exports.lock().map_err(|e| ApiError::Internal(format!("active_exports lock poisoned: {e}")))?;
    if exports.contains_key(&SENTINEL) {
        tracing::warn!(project_id, "project export refused: another project export is running");
        return Err(ApiError::Conflict("Another project export is running.".into()));
    }
    exports.insert(SENTINEL, crate::services::ExportHandle { cancel_flag: cancel_flag.clone() });
}
```
2. Collecting stays on `spawn_blocking` (unchanged). The organizing replaces the per-publisher loop, also on `spawn_blocking`:
```rust
let title_dir = Path::new(output_dir).join(sanitize_display_folder_name(&data.title));
let organized = {
    let pubs = data.publishers.clone();
    let flag = cancel_flag.clone();
    let em = emitter.clone();
    let pid = project_id.to_string();
    let dir = title_dir.clone();
    tokio::task::spawn_blocking(move || {
        crate::export::project_organizer::organize_project_export(&dir, &pid, &pubs, use_symlinks, em.as_deref(), &flag)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("organize join error: {e}")))?
    .map_err(|e| ApiError::Internal(format!("organize project export: {e:#}")))?
};
let mut warnings = data.warnings.clone();
warnings.extend(organized.warnings);
let c = organized.counts;
let files_organized = c.linked + c.copied + c.rewritten;
let cancelled = organized.cancelled || cancel_flag.load(Ordering::Relaxed);
Ok(ExportResult {
    success: !cancelled,
    output_dir: output_dir.to_string(),
    files_organized,
    scripts_generated: Vec::new(),
    warnings,
    error: cancelled.then(|| "Export cancelled".to_string()),
    linked: c.linked,
    copied: c.copied,
    rewritten: c.rewritten,
    failed: c.failed,
})
```
3. `load_wbpp_config` is no longer used by this runner. Delete its call and keep the function only if another caller remains (`grep -n load_wbpp_config`).
4. The terminal `ExportCompleteEvent` copies the four counts from the result (zeros on `Err`).
5. The final info log adds `linked`, `copied`, `rewritten`, `failed`.

Delete the Task 4 stub and the `#[ignore]`d test.

- [ ] **Step 5: Regenerate TS and run the Rust tests**

```bash
TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib collab_exchange::tests -- --test-threads=4
CARGO_BUILD_JOBS=4 cargo check --workspace
```
Expected: `src/types/export.ts` gains the four fields; tests PASS; the workspace checks clean.

- [ ] **Step 6: Dialog and notification**

`src/utils/exportCounts.ts` (new):
```ts
/** "1 linked · 2 with FILTER rewritten" — only the non-zero counts (plan P11). */
export function exportCountsText(c: { linked: number; copied: number; rewritten: number; failed: number }): string {
  return [
    c.linked > 0 && `${c.linked} linked`,
    c.copied > 0 && `${c.copied} copied`,
    c.rewritten > 0 && `${c.rewritten} with FILTER rewritten`,
    c.failed > 0 && `${c.failed} failed`,
  ]
    .filter(Boolean)
    .join(' · ');
}
```
`ProjectExportDialog.tsx`: in the success block, under the "Export complete" line, add `{exportCountsText(result) && <p className="mt-0.5 text-[11.5px] text-content-faint">{exportCountsText(result)}</p>}`.
`useExportProgress.ts`: in the `export-complete` notify, when `payload.frameSetId === -1` and `exportCountsText(payload)` is non-empty, append ` · ${exportCountsText(payload)}` to `detail`.

Test (`ProjectExportDialog.test.tsx`, following the file's existing invoke mock): resolve `export_collab_project` with `{ success: true, filesOrganized: 3, linked: 1, copied: 0, rewritten: 2, failed: 0, outputDir: '/o', warnings: [], scriptsGenerated: [], error: null }` and assert the text `1 linked · 2 with FILTER rewritten` is shown.

Run: `npx tsc --noEmit -p . && npx vitest run src/components/collab/ProjectExportDialog.test.tsx src/hooks`
Expected: clean, PASS.

- [ ] **Step 7: Commit** — `feat(collab): project export in canonical filter folders with FILTER-patched copies; one at a time` (same trailers).

---

### Task 7: R3 export half — an attested set passes through

**Files:**
- Modify:
  - `crates/athenaeum-core/src/api/lights.rs` (`ExportReadiness`, `readiness_from_data`, `check_mode_ready`, tests)
  - every `ExportReadiness {` literal
  - `crates/athenaeum-core/src/export/data_collector.rs` (`apply_calibrated_lights`, tests)
  - `crates/athenaeum-core/src/api/frame_set_send.rs` (test)
  - `src/components/export/ExportTab.tsx` (`modeBlocker`, hint) + its test
  - generated `src/types/models.ts`

**Interfaces:**
- Produces: `ExportReadiness.attested: bool` (`#[serde(default)]`); `check_mode_ready(r, CalibratedLights)` returns `Ok(())` when `r.attested`.

- [ ] **Step 1: Failing tests**

In `api/lights.rs` tests (use the existing `seed_db`, `seed_frame_set`, `seed_light`, `seed_set`, `add_link`):
```rust
#[test]
fn an_attested_set_is_ready_for_calibrated_lights_without_masters() {
    let conn = seed_db();
    let session = seed_frame_set(&conn, 1);
    seed_light(&conn, 10, session);
    seed_set(&conn, 100, "Dark", false); // a RAW dark set, no master
    add_link(&conn, 10, 100, "Dark");
    let r = compute_export_readiness(&conn, 1).unwrap();
    assert!(check_mode_ready(&r, ExportMode::CalibratedLights).is_err(), "not attested: build masters first");
    crate::db::collab::set_frames_set_attestation(&conn, 1, true).unwrap();
    let r = compute_export_readiness(&conn, 1).unwrap();
    assert!(r.attested);
    assert_eq!(check_mode_ready(&r, ExportMode::CalibratedLights), Ok(()));
    assert!(check_mode_ready(&r, ExportMode::RawWithMasters).is_err(), "raw modes keep their own gate");
}
```
In `export/data_collector.rs` tests (use `mem`, `seed_frame_set`, `seed_light`):
```rust
#[test]
fn apply_calibrated_lights_passes_an_attested_set_through() {
    let conn = mem();
    let session = seed_frame_set(&conn, 1);
    seed_light(&conn, 10, session, Some("Ha"));
    crate::db::collab::set_frames_set_attestation(&conn, 1, true).unwrap();
    let mut data = crate::export::collect_export_data(&conn, 1).unwrap();
    let names_before: Vec<String> = lights_by_id(&data).values().map(|f| f.filename.clone()).collect();
    apply_export_mode(&conn, &mut data, ExportMode::CalibratedLights, Some(&CalibratedLightOptions::default())).unwrap();
    for f in lights_by_id(&data).values() {
        assert_eq!(f.debayer_calibrated, None, "copied as is");
    }
    let names_after: Vec<String> = lights_by_id(&data).values().map(|f| f.filename.clone()).collect();
    assert_eq!(names_before, names_after, "original file names");
}
```
Adapt `lights_by_id`'s exact return type to the helper's real signature (`data_collector.rs` ~2629). In `api/frame_set_send.rs` tests, after `seed(conn, dir)` and attesting set 1:
```rust
let entries = frame_set_entries(&ctx, 1, ExportMode::CalibratedLights, &CalibratedLightOptions::default()).unwrap();
assert!(entries.iter().all(|e| !e.generate), "an attested set's lights are copies");
assert!(entries.iter().all(|e| e.kind == PayloadKind::CalibratedLight));
```

- [ ] **Step 2: Run, see them fail.**

- [ ] **Step 3: Implement**
  - `ExportReadiness`: add
    ```rust
    /// R3 (wave 6 §13): the set is attested "calibrated by an external
    /// tool" — the calibrated-lights mode copies its lights as they are.
    #[serde(default)]
    pub attested: bool,
    ```
    Then `grep -rn "ExportReadiness {" crates/` and add `attested: false,` to every literal except the one `readiness_from_data` builds.
  - `readiness_from_data`: before building the struct,
    ```rust
    let attested = crate::db::collab::frames_set_attested(conn, set_id).unwrap_or_else(|e| {
        tracing::warn!(set_id, error = %e, "attestation read failed; treated as not attested");
        false
    });
    ```
    and set `attested` in the literal.
  - `check_mode_ready`: insert as the FIRST `CalibratedLights` arm: `ExportMode::CalibratedLights if r.attested => Ok(()),`.
  - `apply_calibrated_lights`: right after `drop_calibration_nodes(data);`:
    ```rust
    if crate::db::collab::frames_set_attested(conn, data.frame_set_id)? {
        tracing::info!(frames_set_id = data.frame_set_id, "calibrated-lights export of an attested set: lights copied as they are");
        return Ok(Vec::new());
    }
    ```
    The read error propagates here (an export must not silently calibrate or skip).
  - `ExportTab.tsx`, `modeBlocker`: at the top of the `calibratedLights` branch, `if (r.attested) return null;`. The mode hint: when `readiness?.attested` and the mode is `calibratedLights`, show "This set is attested as calibrated by an external tool — its lights are copied as they are." instead of the generation hint. If `modeBlocker` is not exported, export it and add an `ExportTab.test.tsx` case: `modeBlocker({ ...ready, rawSetsWithoutMaster: 1, attested: true }, 'calibratedLights') === null`.

- [ ] **Step 4: Regenerate TS, run** — `TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract`, then the Rust tests above (the `api::lights`, `export::data_collector`, `api::frame_set_send` modules; the last needs `--features render` as its module does), then `npx tsc --noEmit -p . && npx vitest run src/components/export`. Expected: PASS.

- [ ] **Step 5: Commit** — `feat(export): an attested frame set passes through the calibrated-lights export (R3)` (same trailers).

---

### Task 8: Fake hub PATCH and the hub client remap call

**Files:**
- Modify:
  - `crates/athenaeum-core/src/collab/fake_hub.rs` (route + handler + tests)
  - `crates/athenaeum-core/src/collab/hub_client.rs` (`patch_frame_filter`)

**Interfaces:**
- Produces: `CollabClient::patch_frame_filter(&self, token: &str, project_id: &str, frame_uuid: &str, canonical: &str) -> Result<(), AccountClientError>`; fake route `PATCH /api/v1/projects/{pid}/frames/{uuid}` (body `{accepted?, acceptedReason?, filterCanonical?}`) mirroring `athenaeum-hub/src/routes/frames.rs::patch_frame`.

- [ ] **Step 1: Failing test** (in `fake_hub.rs` tests, or a new `#[cfg(test)] mod patch_tests` there)

```rust
#[tokio::test]
async fn patch_remaps_a_frame_and_enforces_caps_and_the_dictionary() {
    let hub = FakeHub::start().await;
    hub.add_account("tok-c", "acc-c", "Coord", &B64.encode([1u8; 32]), None);
    hub.add_account("tok-m", "acc-m", "Member", &B64.encode([2u8; 32]), None);
    hub.add_project("p1", "m31", &[("acc-c", "send_receive", true), ("acc-m", "send_receive", false)], false);
    hub.seed_frames("p1", "acc-m", &["u1"], "published");
    let client = crate::collab::hub_client::CollabClient::new(&hub.uri()).unwrap();
    let v0 = hub.version("p1");
    client.patch_frame_filter("tok-c", "p1", "u1", "R").await.unwrap();
    assert_eq!(hub.frame("p1", "u1").unwrap().filter_canonical, "R");
    assert_eq!(hub.version("p1"), v0 + 1, "one bump per PATCH");
    assert!(matches!(client.patch_frame_filter("tok-m", "p1", "u1", "G").await, Err(AccountClientError::Forbidden)));
    assert!(matches!(client.patch_frame_filter("tok-c", "p1", "u1", "NotInDict").await, Err(AccountClientError::Http { status: 400, .. })));
    assert!(matches!(client.patch_frame_filter("tok-c", "p1", "nope", "R").await, Err(AccountClientError::Http { status: 404, .. })));
}
```
This uses the fake's default dictionary (it contains `R`, `G`, `B`, `L`, `Ha`, `OIII`, `SII`; confirm by reading `FakeProject`'s default and adjust the canonical names in the test to entries that exist). `CollabClient::new`'s exact constructor and the `B64` import follow the existing `collab_exchange.rs` fixtures.

- [ ] **Step 2: Run, see it fail** (404 from the fake, method missing).

- [ ] **Step 3: Implement**

`hub_client.rs`, beside `set_frame_acceptance`:
```rust
/// `PATCH /projects/{id}/frames/{uuid}` with `filterCanonical` — the
/// coordinator's filter remap (requires `thresholds.edit`; the canonical
/// name must be in the project dictionary). A manifest edit, never a
/// re-transfer. Accepts 200 or 204.
pub async fn patch_frame_filter(&self, token: &str, project_id: &str, frame_uuid: &str, canonical: &str) -> Result<(), AccountClientError> {
    let resp = self
        .http
        .patch(self.url(&format!("/projects/{project_id}/frames/{frame_uuid}")))
        .bearer_auth(token)
        .json(&serde_json::json!({ "filterCanonical": canonical }))
        .send()
        .await
        .map_err(net)?;
    let status = resp.status();
    if status == StatusCode::OK || status == StatusCode::NO_CONTENT {
        return Ok(());
    }
    Err(classify(status, resp, "remap frame filter").await)
}
```
`fake_hub.rs`: in `route()`'s authenticated `match`, add `("PATCH", ["projects", pid, "frames", uuid]) => patch_frame(st, &acct, pid, uuid, req),`. Then:
```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FramePatch {
    accepted: Option<bool>,
    accepted_reason: Option<String>,
    filter_canonical: Option<String>,
}

/// Mirrors the hub's `patch_frame`: `accepted` needs `data.moderate`,
/// `filterCanonical` needs `thresholds.edit`; one bump and one `frames`
/// event per PATCH.
fn patch_frame(st: &mut FakeHubState, acct: &FakeAccount, pid: &str, uuid: &str, req: &Request) -> ResponseTemplate {
    let body: FramePatch = match req.body_json() {
        Ok(b) => b,
        Err(e) => return error(422, format!("bad patch body: {e}")),
    };
    if body.accepted.is_none() && body.filter_canonical.is_none() {
        return error(400, "accepted or filterCanonical is required");
    }
    let Some(p) = st.projects.get_mut(pid) else { return not_found_project() };
    let Some(member) = p.member(&acct.account_id).cloned() else { return empty(403) };
    if (body.accepted.is_some() && !member.has_cap("data.moderate"))
        || (body.filter_canonical.is_some() && !member.has_cap("thresholds.edit"))
    {
        return empty(403);
    }
    if let Some(f) = &body.filter_canonical {
        if !p.dictionary.iter().any(|d| &d.canonical == f) {
            return error(400, format!("filter {f:?} is not in the project dictionary"));
        }
    }
    let reason = body.accepted_reason.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    if body.accepted == Some(false) && reason.as_ref().is_none_or(|r| r.chars().count() > 500) {
        return error(400, "acceptedReason (1..=500 chars) is required when excluding a frame");
    }
    if !p.frames.contains_key(uuid) {
        return error(404, "frame not found");
    }
    let prev = p.version;
    let version = p.bump();
    let frame = p.frames.get_mut(uuid).expect("checked above");
    frame.manifest_version = version;
    if let Some(f) = body.filter_canonical {
        frame.filter_canonical = f;
    }
    if let Some(a) = body.accepted {
        frame.accepted = a;
        frame.accepted_reason = if a { None } else { reason };
    }
    st.publish_bump(pid, prev, &["frames"], &[uuid.to_string()]);
    empty(204)
}
```
Check the fake's real names before use: `not_found_project`, `error`, `empty`, `req.body_json`, `p.dictionary`, `p.frames`. `Option::is_none_or` needs Rust ≥ 1.82; the toolchain is 1.96.

- [ ] **Step 4: Run** — `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib collab::fake_hub -- --test-threads=4` → PASS; also run `api::collab::tests` with the filter `exclude` to confirm the wiremock-based exclude tests are unaffected.

- [ ] **Step 5: Commit** — `feat(collab): hub client filter remap call; fake hub PATCH frames` (same trailers).

---

### Task 9: The remap task and its three commands

**Files:**
- Create: `crates/athenaeum-core/src/api/collab_remap.rs`
- Modify:
  - `crates/athenaeum-core/src/api/mod.rs` (`pub mod collab_remap;`)
  - `crates/athenaeum-core/src/api/collab.rs` (`fn project_dictionary` → `pub(crate) fn`)
  - `crates/athenaeum-core/src/ts_export.rs` (4 types after `FilterMappingSheet`)
  - `crates/athenaeum-tauri/src/commands/collab.rs` and `src/lib.rs`
  - `crates/athenaeum-web/src/routes/collab.rs` and `src/routes/mod.rs`
  - generated `src/types/models.ts`

**Interfaces:**
- Consumes: Task 8 `patch_frame_filter`; `crate::api::account::hub_credentials`, `crate::api::collab_exchange::live_project`, `crate::api::collab::project_dictionary`, `crate::db::collab_frames::list_for_project`, `crate::api::db`.
- Produces:
  - `pub const COLLAB_REMAP_PROGRESS_EVENT: &str = "collab-remap-progress";`
  - `pub const COLLAB_REMAP_FINISHED_EVENT: &str = "collab-remap-finished";`
  - `pub const REMAP_BUSY_MSG: &str`
  - `CollabRemapProgress { project_id, canonical, done: u32, failed: u32, total: u32 }`
  - `CollabRemapFinished { project_id, canonical, done, failed, total, cancelled: bool, error: Option<String> }`
  - `FilterBucket { publisher_account_id, publisher, instrume, filter_raw, canonical: Option<String>, frame_uuids: Vec<String> }`
  - `FilterBucketSheet { project_id, dictionary: Vec<DictionaryEntry>, buckets: Vec<FilterBucket>, running: Option<CollabRemapProgress> }`
  - `pub fn get_collab_filter_buckets(ctx: &ServiceContext, project_id: &str) -> Result<FilterBucketSheet, ApiError>`
  - `pub async fn remap_collab_filter(ctx: Arc<ServiceContext>, project_id: String, frame_uuids: Vec<String>, canonical: String, emitter: Option<Arc<dyn ProgressEmitter>>) -> Result<(), ApiError>`. It returns once the task is admitted; the work runs in the background.
  - `pub fn cancel_collab_remap(ctx: &ServiceContext, project_id: &str) -> Result<(), ApiError>`
  - Commands: `get_collab_filter_buckets { projectId }`, `remap_collab_filter { projectId, frameUuids, canonical }`, `cancel_collab_remap { projectId }`.

- [ ] **Step 1: Failing tests** (`collab_remap.rs` `mod tests`; a minimal fixture over the fake hub)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_exchange::test_support::{test_ctx, wire_hub};
    use crate::collab::fake_hub::FakeHub;
    use base64::Engine;
    use std::sync::Mutex as StdMutex;

    struct Rec(StdMutex<Vec<(String, serde_json::Value)>>);
    impl ProgressEmitter for Rec {
        fn emit_json(&self, name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((name.to_string(), payload));
        }
    }

    async fn fixture(coordinator: bool) -> (tempfile::TempDir, Arc<ServiceContext>, FakeHub) {
        let hub = FakeHub::start().await;
        let (tmp, ctx) = test_ctx();
        wire_hub(&ctx, &hub.uri(), "tok");
        let b64 = base64::engine::general_purpose::STANDARD;
        hub.add_account("tok", "acc-me", "Me", &b64.encode([7u8; 32]), None);
        hub.add_account("tok-o", "acc-o", "Other", &b64.encode([8u8; 32]), None);
        hub.add_project("p1", "m31", &[("acc-me", "send_receive", coordinator), ("acc-o", "send_receive", false)], false);
        let uuids: Vec<String> = (0..12).map(|i| format!("u{i:02}")).collect();
        let refs: Vec<&str> = uuids.iter().map(String::as_str).collect();
        hub.seed_frames_with("p1", "acc-o", &refs, "published", |f| f.filter_raw = "Red".into());
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        crate::api::collab_exchange::sync_manifest(&ctx, "p1", None, None).await.unwrap();
        (tmp, Arc::new(ctx), hub)
    }

    async fn finished(rec: &Rec) -> CollabRemapFinished {
        for _ in 0..400 {
            if let Some((_, v)) = rec.0.lock().unwrap().iter().find(|(n, _)| n == COLLAB_REMAP_FINISHED_EVENT) {
                return serde_json::from_value(v.clone()).unwrap();
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("no {COLLAB_REMAP_FINISHED_EVENT}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn buckets_group_by_publisher_camera_raw_filter() {
        let (_t, ctx, _hub) = fixture(true).await;
        let sheet = get_collab_filter_buckets(&ctx, "p1").unwrap();
        assert_eq!(sheet.buckets.len(), 1);
        let b = &sheet.buckets[0];
        assert_eq!((b.filter_raw.as_str(), b.frame_uuids.len()), ("Red", 12));
        assert!(!sheet.dictionary.is_empty());
        assert!(sheet.running.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_remap_patches_every_frame_and_reports_once() {
        let (_t, ctx, hub) = fixture(true).await;
        let uuids = get_collab_filter_buckets(&ctx, "p1").unwrap().buckets[0].frame_uuids.clone();
        let rec = Arc::new(Rec(StdMutex::new(Vec::new())));
        remap_collab_filter(ctx.clone(), "p1".into(), uuids.clone(), "R".into(), Some(rec.clone())).await.unwrap();
        let f = finished(&rec).await;
        assert_eq!((f.done, f.failed, f.total, f.cancelled), (12, 0, 12, false));
        for u in &uuids {
            assert_eq!(hub.frame("p1", u).unwrap().filter_canonical, "R");
        }
        assert_eq!(rec.0.lock().unwrap().iter().filter(|(n, _)| n == COLLAB_REMAP_FINISHED_EVENT).count(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_remap_is_refused_and_cancel_stops_the_first() {
        let (_t, ctx, _hub) = fixture(true).await;
        let uuids = get_collab_filter_buckets(&ctx, "p1").unwrap().buckets[0].frame_uuids.clone();
        let rec = Arc::new(Rec(StdMutex::new(Vec::new())));
        remap_collab_filter(ctx.clone(), "p1".into(), uuids.clone(), "R".into(), Some(rec.clone())).await.unwrap();
        cancel_collab_remap(&ctx, "p1").unwrap();
        let second = remap_collab_filter(ctx.clone(), "p1".into(), uuids, "G".into(), None).await;
        // Either refused while the first runs, or admitted after it finished — never two at once:
        let f = finished(&rec).await;
        assert!(f.cancelled || f.done == 12);
        if let Err(e) = second {
            assert!(matches!(e, ApiError::Conflict(_)), "{e:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_member_without_thresholds_edit_stops_at_the_first_forbidden() {
        let (_t, ctx, _hub) = fixture(false).await;
        let uuids = get_collab_filter_buckets(&ctx, "p1").unwrap().buckets[0].frame_uuids.clone();
        let rec = Arc::new(Rec(StdMutex::new(Vec::new())));
        remap_collab_filter(ctx.clone(), "p1".into(), uuids, "R".into(), Some(rec.clone())).await.unwrap();
        let f = finished(&rec).await;
        assert_eq!(f.done, 0);
        assert!(f.error.as_deref().unwrap_or("").contains("thresholds.edit"), "{f:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_canonical_outside_the_dictionary_is_refused_up_front() {
        let (_t, ctx, _hub) = fixture(true).await;
        let err = remap_collab_filter(ctx.clone(), "p1".into(), vec!["u00".into()], "Nope".into(), None).await.unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
    }
}
```
A 429 path is covered by a unit test of the retry helper (Step 3's `patch_with_retry`). Make the wait injectable: `patch_with_retry(…, wait: Duration)`, and have the test drive a `wiremock` server that answers 429 twice then 204, asserting success with `wait = 1 ms`. Use `wiremock::{Mock, MockServer, ResponseTemplate, matchers::method}` with `.up_to_n_times(2)` for the 429s, as `api/collab.rs`'s exclude tests do.

If the seeded frames' `instrume` lives only in `meta` and `seed_frames_with` leaves it empty, the bucket's camera is `""`; the test above does not depend on it.

- [ ] **Step 2: Run, see them fail.**

- [ ] **Step 3: Implement `collab_remap.rs`**

```rust
//! The coordinator's filter remap (collab v3 wave 6, spec §11; plan P3, P8,
//! P9). Buckets = publisher × camera × raw filter over the local manifest
//! mirror; a remap PATCHes each frame's `filterCanonical` on the hub, paced
//! (4 in flight, ≤ 10 per second), cancellable, one per project at a time.
//! No local row is written: the manifest edit comes back through the feed.
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::account::client::AccountClientError;
use crate::api::{db, ApiError};
use crate::collab::filters::DictionaryEntry;
use crate::collab::hub_client::CollabClient;
use crate::events::{emit_event, ProgressEmitter};
use crate::services::ServiceContext;

pub const COLLAB_REMAP_PROGRESS_EVENT: &str = "collab-remap-progress";
pub const COLLAB_REMAP_FINISHED_EVENT: &str = "collab-remap-finished";
pub const REMAP_BUSY_MSG: &str = "A filter remap is already running for this project.";
const IN_FLIGHT: usize = 4;
const MIN_SPACING: Duration = Duration::from_millis(100);
const RATE_LIMIT_WAIT: Duration = Duration::from_secs(5);
const RATE_LIMIT_RETRIES: u32 = 3;
const THROTTLE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabRemapProgress {
    pub project_id: String,
    pub canonical: String,
    pub done: u32,
    pub failed: u32,
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabRemapFinished {
    pub project_id: String,
    pub canonical: String,
    pub done: u32,
    pub failed: u32,
    pub total: u32,
    pub cancelled: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FilterBucket {
    pub publisher_account_id: String,
    pub publisher: String,
    pub instrume: String,
    pub filter_raw: String,
    /// The bucket's canonical filter, `None` when its frames disagree.
    pub canonical: Option<String>,
    pub frame_uuids: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FilterBucketSheet {
    pub project_id: String,
    pub dictionary: Vec<DictionaryEntry>,
    pub buckets: Vec<FilterBucket>,
    pub running: Option<CollabRemapProgress>,
}

struct RemapState {
    cancel: AtomicBool,
    progress: Mutex<CollabRemapProgress>,
}

fn registry() -> &'static Mutex<HashMap<String, Arc<RemapState>>> {
    static R: OnceLock<Mutex<HashMap<String, Arc<RemapState>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn key(ctx: &ServiceContext, project_id: &str) -> Result<String, ApiError> {
    Ok(format!("{}|{project_id}", db(ctx)?.path().display()))
}

pub fn get_collab_filter_buckets(ctx: &ServiceContext, project_id: &str) -> Result<FilterBucketSheet, ApiError> {
    let database = db(ctx)?;
    let conn = database.conn();
    let project = crate::api::collab_exchange::live_project(&conn, project_id)?;
    let dictionary = crate::api::collab::project_dictionary(&project)?;
    let rows = crate::db::collab_frames::list_for_project(&conn, project_id)?;
    let mut by_key: BTreeMap<(String, String, String), FilterBucket> = BTreeMap::new();
    for row in rows {
        let manifest: serde_json::Value = serde_json::from_str(&row.manifest_json).unwrap_or_else(|e| {
            tracing::warn!(project_id, frame_uuid = %row.frame_uuid, error = %e, "remap buckets: manifest row is not valid json");
            serde_json::Value::Null
        });
        let filter_raw = manifest.get("filterRaw").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let instrume = manifest.pointer("/meta/instrume").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let k = (row.publisher_display.clone(), instrume.clone(), filter_raw.clone());
        let b = by_key.entry(k).or_insert_with(|| FilterBucket {
            publisher_account_id: row.publisher_account_id.clone(),
            publisher: row.publisher_display.clone(),
            instrume,
            filter_raw,
            canonical: Some(row.filter_canonical.clone()),
            frame_uuids: Vec::new(),
        });
        if b.canonical.as_deref() != Some(row.filter_canonical.as_str()) {
            b.canonical = None;
        }
        b.frame_uuids.push(row.frame_uuid);
    }
    let running = registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key(ctx, project_id)?)
        .map(|s| s.progress.lock().unwrap_or_else(|p| p.into_inner()).clone());
    Ok(FilterBucketSheet { project_id: project_id.to_string(), dictionary, buckets: by_key.into_values().collect(), running })
}

enum Stop {
    Forbidden,
    Refused(String),
}

/// One PATCH, a 429 waited out (`wait`, up to RATE_LIMIT_RETRIES times).
async fn patch_with_retry(client: &CollabClient, token: &str, pid: &str, uuid: &str, canonical: &str, wait: Duration) -> Result<(), AccountClientError> {
    let mut tries = 0;
    loop {
        match client.patch_frame_filter(token, pid, uuid, canonical).await {
            Err(AccountClientError::RateLimited) if tries < RATE_LIMIT_RETRIES => {
                tries += 1;
                tracing::debug!(project_id = pid, frame_uuid = uuid, tries, "remap: rate limited, waiting");
                tokio::time::sleep(wait).await;
            }
            other => return other,
        }
    }
}

pub async fn remap_collab_filter(
    ctx: Arc<ServiceContext>,
    project_id: String,
    frame_uuids: Vec<String>,
    canonical: String,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<(), ApiError> {
    if frame_uuids.is_empty() {
        return Err(ApiError::Invalid("no frames to remap".into()));
    }
    {
        let database = db(&ctx)?;
        let conn = database.conn();
        let project = crate::api::collab_exchange::live_project(&conn, &project_id)?;
        let dictionary = crate::api::collab::project_dictionary(&project)?;
        if !dictionary.iter().any(|d| d.canonical == canonical) {
            return Err(ApiError::Invalid(format!("{canonical:?} is not in the project's filter dictionary")));
        }
    }
    let Some((hub_url, token)) = crate::api::account::hub_credentials(&ctx)? else {
        return Err(ApiError::SignedOut("Sign in to remap filters.".into()));
    };
    let client = CollabClient::new(&hub_url).map_err(crate::api::collab::client_err)?;
    let k = key(&ctx, &project_id)?;
    let total = frame_uuids.len() as u32;
    let state = Arc::new(RemapState {
        cancel: AtomicBool::new(false),
        progress: Mutex::new(CollabRemapProgress { project_id: project_id.clone(), canonical: canonical.clone(), done: 0, failed: 0, total }),
    });
    {
        let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
        if reg.contains_key(&k) {
            tracing::warn!(project_id, "remap refused: one is already running");
            return Err(ApiError::Conflict(REMAP_BUSY_MSG.into()));
        }
        reg.insert(k.clone(), state.clone());
    }
    tracing::info!(project_id, canonical, count = total, "filter remap started");
    if let Some(em) = emitter.as_deref() {
        emit_event(em, COLLAB_REMAP_PROGRESS_EVENT, &*state.progress.lock().unwrap_or_else(|p| p.into_inner()));
    }
    tokio::spawn(async move {
        let started = Instant::now();
        let mut stop: Option<Stop> = None;
        let mut last_emit = Instant::now();
        let mut queue = frame_uuids.into_iter();
        let mut inflight = tokio::task::JoinSet::new();
        let mut next_send = Instant::now();
        loop {
            while stop.is_none() && !state.cancel.load(Ordering::SeqCst) && inflight.len() < IN_FLIGHT {
                let Some(uuid) = queue.next() else { break };
                tokio::time::sleep_until(next_send.into()).await;
                next_send = Instant::now() + MIN_SPACING;
                let (c, t, p, can) = (client.clone(), token.clone(), project_id.clone(), canonical.clone());
                inflight.spawn(async move {
                    let r = patch_with_retry(&c, &t, &p, &uuid, &can, RATE_LIMIT_WAIT).await;
                    (uuid, r)
                });
            }
            let Some(joined) = inflight.join_next().await else { break };
            let (uuid, res) = match joined {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(project_id, error = %e, "remap: a request task panicked");
                    state.progress.lock().unwrap_or_else(|p| p.into_inner()).failed += 1;
                    continue;
                }
            };
            {
                let mut p = state.progress.lock().unwrap_or_else(|p| p.into_inner());
                match res {
                    Ok(()) => p.done += 1,
                    Err(AccountClientError::Forbidden) => {
                        p.failed += 1;
                        stop.get_or_insert(Stop::Forbidden);
                    }
                    Err(AccountClientError::Http { status: 400, message }) => {
                        p.failed += 1;
                        stop.get_or_insert(Stop::Refused(message));
                    }
                    Err(e) => {
                        p.failed += 1;
                        tracing::warn!(project_id, frame_uuid = %uuid, error = %e, "remap: frame failed");
                    }
                }
            }
            if let Some(em) = emitter.as_deref() {
                if last_emit.elapsed() >= THROTTLE {
                    last_emit = Instant::now();
                    emit_event(em, COLLAB_REMAP_PROGRESS_EVENT, &*state.progress.lock().unwrap_or_else(|p| p.into_inner()));
                }
            }
        }
        let p = state.progress.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let cancelled = state.cancel.load(Ordering::SeqCst) && stop.is_none();
        let error = match stop {
            Some(Stop::Forbidden) => Some("This account lacks thresholds.edit on the project.".to_string()),
            Some(Stop::Refused(m)) => Some(format!("The hub refused the remap: {m}")),
            None => None,
        };
        registry().lock().unwrap_or_else(|p| p.into_inner()).remove(&k);
        let fin = CollabRemapFinished {
            project_id: p.project_id.clone(),
            canonical: p.canonical.clone(),
            done: p.done,
            failed: p.failed,
            total: p.total,
            cancelled,
            error: error.clone(),
        };
        match &error {
            Some(e) => tracing::error!(project_id = %p.project_id, done = p.done, failed = p.failed, error = %e, "filter remap stopped"),
            None => tracing::info!(project_id = %p.project_id, done = p.done, failed = p.failed, cancelled, duration_ms = started.elapsed().as_millis() as u64, "filter remap finished"),
        }
        if let Some(em) = emitter.as_deref() {
            emit_event(em, COLLAB_REMAP_FINISHED_EVENT, &fin);
        }
    });
    Ok(())
}

pub fn cancel_collab_remap(ctx: &ServiceContext, project_id: &str) -> Result<(), ApiError> {
    match registry().lock().unwrap_or_else(|p| p.into_inner()).get(&key(ctx, project_id)?) {
        Some(s) => {
            s.cancel.store(true, Ordering::SeqCst);
            tracing::info!(project_id, "filter remap cancel requested");
        }
        None => tracing::debug!(project_id, "remap cancel: none running"),
    }
    Ok(())
}
```
Things to check against the code before compiling:
- `CollabClient` must be `Clone`. If it is not, wrap it in an `Arc`.
- `client_err` is private in `api/collab.rs`: make it `pub(crate)`.
- `ApiError::SignedOut` takes a `String`, as `exclude_collab_frame` uses it.

- [ ] **Step 4: Wire both hosts**

Tauri, in `commands/collab.rs`:
```rust
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_collab_filter_buckets(state: State<'_, AppState>, project_id: String) -> Result<athenaeum_core::api::collab_remap::FilterBucketSheet, String> {
    athenaeum_core::api::collab_remap::get_collab_filter_buckets(&state.ctx, &project_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn remap_collab_filter(state: State<'_, AppState>, app: AppHandle, project_id: String, frame_uuids: Vec<String>, canonical: String) -> Result<(), String> {
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    athenaeum_core::api::collab_remap::remap_collab_filter(state.ctx.clone(), project_id, frame_uuids, canonical, Some(emitter)).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_collab_remap(state: State<'_, AppState>, project_id: String) -> Result<(), String> {
    athenaeum_core::api::collab_remap::cancel_collab_remap(&state.ctx, &project_id).map_err(|e| e.to_string())
}
```
Register all three in `invoke_handler![]` next to `commands::cancel_collab_publish`.

Web, in `routes/collab.rs`:
```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemapArgs {
    project_id: String,
    frame_uuids: Vec<String>,
    canonical: String,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_collab_filter_buckets(State(state): State<WebAppState>, Json(args): Json<ProjectIdArgs>) -> Result<Json<athenaeum_core::api::collab_remap::FilterBucketSheet>, (axum::http::StatusCode, String)> {
    athenaeum_core::api::collab_remap::get_collab_filter_buckets(&state.ctx, &args.project_id).map(Json).map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn remap_collab_filter(State(state): State<WebAppState>, Json(args): Json<RemapArgs>) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    athenaeum_core::api::collab_remap::remap_collab_filter(state.ctx.clone(), args.project_id, args.frame_uuids, args.canonical, Some(emitter)).await.map(Json).map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_collab_remap(State(state): State<WebAppState>, Json(args): Json<ProjectIdArgs>) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    athenaeum_core::api::collab_remap::cancel_collab_remap(&state.ctx, &args.project_id).map(Json).map_err(api_err)
}
```
Register `/api/get_collab_filter_buckets`, `/api/remap_collab_filter` and `/api/cancel_collab_remap` (all `post`) beside `/api/cancel_collab_publish`. Add a camelCase pin test for `RemapArgs`, as the file does for `SessionsArgs`:
```rust
#[test]
fn remap_args_are_camel_case() {
    let a: RemapArgs = serde_json::from_value(serde_json::json!({"projectId": "p", "frameUuids": ["u"], "canonical": "R"})).unwrap();
    assert_eq!((a.project_id.as_str(), a.frame_uuids.len(), a.canonical.as_str()), ("p", 1, "R"));
}
```

`ts_export.rs`: after `crate::api::collab::FilterMappingSheet,` add `crate::api::collab_remap::CollabRemapProgress, crate::api::collab_remap::CollabRemapFinished, crate::api::collab_remap::FilterBucket, crate::api::collab_remap::FilterBucketSheet,`.

- [ ] **Step 5: Run**

```bash
TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib api::collab_remap -- --test-threads=2
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web routes::collab -- --test-threads=4
CARGO_BUILD_JOBS=4 cargo check --workspace
grep -rcE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | awk -F: '{s+=$2} END {print s}'
```
Expected: tests PASS; the command count prints `297`.

- [ ] **Step 6: Commit** — `feat(collab): coordinator filter remap — buckets, paced cancellable PATCH task, three commands on both hosts` (same trailers).

---

### Task 10: `Remapped` for own frames, and the card capabilities

**Files:**
- Modify:
  - `crates/athenaeum-core/src/api/collab_exchange.rs` (`FramesChangeKind`, `as_str`, `classify_frame_change`, test)
  - `crates/athenaeum-core/src/api/collab.rs` (`ProjectCard`, `card_from_row`, `has_cap`)
  - generated `src/types/models.ts`
  - TS test builders of `ProjectCard` (`src/pages/ProjectDetail.test.tsx`, `src/hooks/useCollabNotifications.test.tsx`, `src/components/collab/project/MyFramesTab.test.tsx`, `src/components/collab/project/testFixtures.ts` — whichever build a `ProjectCard`)

**Interfaces:**
- Produces:
  - `FramesChangeKind::Remapped` (`"remapped"`), emitted when `v.own` and the stored row's `filter_canonical` differs.
  - `ProjectCard.can_remap: bool` (`thresholds.edit`) and `ProjectCard.can_edit_grid: bool` (`project.edit`). The coordinator has both.

- [ ] **Step 1: Failing tests**

In `classify_frame_change_rules`, after the existing asserts (the `view` closure builds `filter_canonical: "L"`; `own_row` gives the stored row; set its `filter_canonical`):
```rust
let mut p = prev(own, "published", true, 1);
p.filter_canonical = "Lum".into();
assert_eq!(classify_frame_change(Some(&p), &view(true, "published", true, 1)), vec![K::Remapped]);
let mut r = prev(rep, "published", true, 1);
r.filter_canonical = "Lum".into();
assert!(!classify_frame_change(Some(&r), &view(false, "published", true, 1)).contains(&K::Remapped), "others' remaps notify nothing (P12)");
```
In `api/collab.rs` tests:
```rust
#[test]
fn card_caps_follow_the_cached_governance_caps() {
    assert!(has_cap(true, "[]", "thresholds.edit"), "the coordinator holds every cap");
    assert!(has_cap(false, r#"["thresholds.edit"]"#, "thresholds.edit"));
    assert!(!has_cap(false, r#"["data.moderate"]"#, "project.edit"));
    assert!(!has_cap(false, "not json", "data.moderate"), "a malformed cache row is no caps");
}
```

- [ ] **Step 2: Run, see them fail.**

- [ ] **Step 3: Implement**
  - `FramesChangeKind`: add `Remapped` as the LAST variant, with the doc `/// My frames' canonical filter was changed by a coordinator remap (P12).`; `as_str` → `"remapped"`.
  - `classify_frame_change`: after the `Excluded` push,
    ```rust
    if v.own && prev.is_some_and(|p| p.filter_canonical != v.filter_canonical) {
        out.push(K::Remapped);
    }
    ```
  - `api/collab.rs`: generalize `can_moderate` into
    ```rust
    /// True for the coordinator (every cap is implicit), or when the cached
    /// `gov_caps_json` contains `cap`. A malformed cache row is `warn!`-logged
    /// and read as no caps.
    fn has_cap(is_coordinator: bool, gov_caps_json: &str, cap: &str) -> bool { /* the old can_moderate body, comparing with `cap` */ }
    fn can_moderate(is_coordinator: bool, gov_caps_json: &str) -> bool { has_cap(is_coordinator, gov_caps_json, "data.moderate") }
    ```
    Add `pub can_remap: bool` and `pub can_edit_grid: bool` to `ProjectCard` after `can_moderate`, with docs naming `thresholds.edit` / `project.edit`. Fill them in `card_from_row`. Grep `ProjectCard {` for any other constructor.

- [ ] **Step 4: Regenerate TS, fix the TS builders, run**

```bash
TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib -- --test-threads=4 classify_frame_change card_caps
npx tsc --noEmit -p .
```
Add `canRemap: false, canEditGrid: false` to every TS `ProjectCard` builder that `tsc` reports.

- [ ] **Step 5: Commit** — `feat(collab): Remapped change kind for own frames; card caps canRemap/canEditGrid` (same trailers).

---

### Task 11: Feed coalescing of pull-needing project events

**Files:**
- Modify:
  - `crates/athenaeum-core/src/api/collab_live/workers.rs` (merge function, carry queue, unit tests)
  - `crates/athenaeum-core/src/api/collab_live/live_tests.rs` (e2e)

**Interfaces:**
- Produces: `fn needs_pull(ev: &ProjectEvent) -> bool`, `fn absorb(into: &mut ProjectEvent, next: &ProjectEvent) -> bool` (pure, both `pub(crate)` for tests), and `FeedWorker.carry: VecDeque<FeedWork>`.

- [ ] **Step 1: Failing unit tests** (in `workers.rs` `mod tests`)

```rust
use crate::collab::live::wire::{ChangeKind, LiveEvent, ProjectEvent};

fn pev(pid: &str, prev: i64, version: i64, kinds: &[ChangeKind], more: bool) -> ProjectEvent {
    ProjectEvent { project_id: pid.into(), prev, version, kinds: kinds.to_vec(), frames: None, more }
}

#[test]
fn a_contiguous_frames_only_run_is_absorbed_into_one_pull() {
    let mut first = pev("p", 1, 2, &[ChangeKind::Frames], true);
    assert!(needs_pull(&first));
    assert!(absorb(&mut first, &pev("p", 2, 3, &[ChangeKind::Frames], true)));
    assert!(absorb(&mut first, &pev("p", 3, 4, &[ChangeKind::Frames], false)), "an inline one is covered by the pull too");
    assert_eq!((first.prev, first.version, first.more, first.frames.is_none()), (1, 4, true, true));
}

#[test]
fn absorb_refuses_another_project_a_gap_or_other_kinds() {
    let mut first = pev("p", 1, 2, &[ChangeKind::Frames], true);
    assert!(!absorb(&mut first, &pev("q", 2, 3, &[ChangeKind::Frames], true)));
    assert!(!absorb(&mut first, &pev("p", 5, 6, &[ChangeKind::Frames], true)), "a gap is the applier's catch-up");
    assert!(!absorb(&mut first, &pev("p", 2, 3, &[ChangeKind::Frames, ChangeKind::Members], true)));
    assert_eq!(first.version, 2);
}

#[test]
fn an_inline_event_does_not_start_a_merge() {
    let mut inline = pev("p", 1, 2, &[ChangeKind::Frames], false);
    inline.frames = Some(Vec::new());
    assert!(!needs_pull(&inline));
}
```
Plus an ordering test of the drain (it pins Review Focus 5). Factor the drain into a pure function over a `VecDeque`:
```rust
/// Take `first` and, from the front of `queue`, every work item `absorb`
/// accepts; stop at the first one it refuses (left in place, in order).
/// Returns the merged event and how many queued events it absorbed.
pub(crate) fn coalesce(first: ProjectEvent, queue: &mut VecDeque<FeedWork>) -> (ProjectEvent, usize)
```
```rust
#[test]
fn coalesce_keeps_every_refused_item_in_order() {
    let mut q: VecDeque<FeedWork> = VecDeque::new();
    q.push_back(FeedWork::Event(LiveEvent::Project(pev("p", 2, 3, &[ChangeKind::Frames], true))));
    q.push_back(FeedWork::Event(LiveEvent::Project(pev("q", 7, 8, &[ChangeKind::Frames], true))));
    q.push_back(FeedWork::Event(LiveEvent::Project(pev("p", 3, 4, &[ChangeKind::Frames], true))));
    let (merged, n) = coalesce(pev("p", 1, 2, &[ChangeKind::Frames], true), &mut q);
    assert_eq!((merged.version, n), (3, 1), "stops at the other project's event");
    assert_eq!(q.len(), 2, "q's event and p's later event stay, in order");
    assert!(matches!(&q[0], FeedWork::Event(LiveEvent::Project(e)) if e.project_id == "q"));
}
```

- [ ] **Step 2: Run, see them fail** — `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib collab_live::workers -- --test-threads=2`.

- [ ] **Step 3: Implement in `workers.rs`**

```rust
/// Plan P7 (wave 6 §11.2): a `project` event that will pull the manifest
/// (frames changed and not inlined).
pub(crate) fn needs_pull(ev: &ProjectEvent) -> bool {
    ev.kinds.contains(&ChangeKind::Frames) && (ev.more || ev.frames.is_none())
}

/// Fold `next` into `into` when it is the same project, contiguous, and
/// changes frames only — the pull `into` forces reads from the stored cursor
/// to the head, so it covers `next`'s rows whether inlined or not.
pub(crate) fn absorb(into: &mut ProjectEvent, next: &ProjectEvent) -> bool {
    let frames_only = !next.kinds.is_empty() && next.kinds.iter().all(|k| *k == ChangeKind::Frames);
    if next.project_id != into.project_id || next.prev != into.version || !frames_only {
        return false;
    }
    into.version = next.version;
    into.more = true;
    into.frames = None;
    true
}

pub(crate) fn coalesce(mut first: ProjectEvent, queue: &mut VecDeque<FeedWork>) -> (ProjectEvent, usize) {
    let mut n = 0;
    while let Some(FeedWork::Event(LiveEvent::Project(next))) = queue.front() {
        if !absorb(&mut first, next) {
            break;
        }
        queue.pop_front();
        n += 1;
    }
    (first, n)
}
```
Worker changes:
1. Add a field `carry: VecDeque<FeedWork>` (initialized empty in `new`).
2. In `run`, before the `select!`, serve the carry first:
```rust
if let Some(w) = self.carry.pop_front() {
    self.handle(w).await;
    continue;
}
```
3. In the `select!` arm, pass the receiver to a dispatch step that drains the channel when a pull-needing project event arrives:
```rust
Some(FeedWork::Event(LiveEvent::Project(ev))) if needs_pull(&ev) => {
    while let Ok(w) = work.try_recv() {
        self.carry.push_back(w);
    }
    let (merged, absorbed) = coalesce(ev, &mut self.carry);
    if absorbed > 0 {
        tracing::debug!(project_id = %merged.project_id, merged = absorbed, version = merged.version, "feed: pull-needing events merged");
        self.pending.fetch_sub(absorbed, Ordering::SeqCst);
    }
    self.handle(FeedWork::Event(LiveEvent::Project(merged))).await;
}
Some(w) => self.handle(w).await,
```
Keep the `biased` order and the timer arm as they are. The `handle` arm for `Event` already decrements `pending` once for the merged event itself. `merged` is a new log field; Task 13 adds it to the logging dictionary.

- [ ] **Step 4: The 500-event e2e** (in `live_tests.rs`)

```rust
/// Plan P7: a burst of pull-needing project events (a coordinator's remap
/// of pending rows, say) costs a holder far fewer manifest pulls than events.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_of_pull_events_is_folded_into_few_pulls() {
    let w = ts::two_instances().await;
    let uuids: Vec<String> = (0..300).map(|i| format!("burst-{i:03}")).collect();
    let refs: Vec<&str> = uuids.iter().map(String::as_str).collect();
    w.hub.seed_frames(ts::PID, "acc-a", &refs, "pending"); // never inlined: every event below asks for a pull
    let target = w.hub.version(ts::PID);
    wait_feed_version(&w.b.ctx, target, Duration::from_secs(20)).await;
    let before = w.hub.requests_to("/manifest").await;
    for u in &uuids {
        w.hub.update_frame(ts::PID, u, |f| f.filter_canonical = "R".into());
    }
    let target = w.hub.version(ts::PID);
    wait_feed_version(&w.b.ctx, target, Duration::from_secs(60)).await;
    let pulls = w.hub.requests_to("/manifest").await - before;
    assert!(pulls > 0 && pulls < 100, "{pulls} manifest pulls for 300 events");
}

async fn wait_feed_version(ctx: &crate::services::ServiceContext, version: i64, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let db = crate::api::db(ctx).unwrap();
        let v = crate::db::collab::get_project(&db.conn(), ts::PID).unwrap().map(|p| p.hub_version).unwrap_or(0);
        if v >= version {
            return;
        }
        assert!(Instant::now() < deadline, "feed version {v} < {version} after {within:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
```
Read before running:
- `FakeHub::requests_to(path)` matches a path SUFFIX or substring. Use the form its other callers use for the manifest route.
- `ts::PID` and A's account id (`"acc-a"`) are as `two_instances_with` sets them.

- [ ] **Step 5: Run**

```bash
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib collab_live::workers -- --test-threads=2
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib collab_live::live_tests -- --test-threads=2
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib collab_live -- --test-threads=2
```
Expected: PASS. The whole `collab_live` module must stay green: the feed is load-bearing for every live test. Root-cause any new failure (the live tests have exposed real bugs before); never re-run until green.

- [ ] **Step 6: Commit** — `perf(collab-live): fold a burst of pull-needing project events into one manifest pull` (same trailers).

---

### Task 12: Frontend — the Filters section and the notifications

**Files:**
- Create: `src/components/collab/project/FilterRemapSection.tsx`, `src/components/collab/project/FilterRemapSection.test.tsx`
- Modify:
  - `src/components/collab/project/ModerationTab.tsx` (+ test)
  - `src/pages/ProjectDetail.tsx` (+ test)
  - `src/hooks/useCollabNotifications.ts` (+ test)

**Interfaces:**
- Consumes: Task 9 commands and types (`FilterBucketSheet`, `FilterBucket`, `CollabRemapProgress`, `CollabRemapFinished` from `src/types/models.ts`); Task 10 `ProjectCard.canRemap`; the `remapped` kind.
- Produces:
  - `<FilterRemapSection projectId syncToken />`;
  - `ModerationTab` gains props `canModerate: boolean` and `canRemap: boolean`.

- [ ] **Step 1: Failing component tests** (`FilterRemapSection.test.tsx`; mock `api` exactly as `ModerationTab.test.tsx` does)

Cases:
1. It renders one row per bucket: publisher, camera, the raw filter (`(no FILTER)` when empty), the canonical (`mixed` when `canonical === null`) and the frame count.
2. `Apply` is disabled until the select differs from the bucket's canonical; choosing `R` and pressing Apply opens a dialog titled "Remap filter". Confirming calls `remap_collab_filter` with `{ projectId, frameUuids: bucket.frameUuids, canonical: 'R' }`.
3. A `collab-remap-progress` event for this project shows `Remapping… 3 of 12` and a Cancel button. Cancel calls `cancel_collab_remap { projectId }`.
4. A `collab-remap-finished` event shows the result line and reloads the sheet (a second `get_collab_filter_buckets` call). Events of another project are ignored.
5. A failed `get_collab_filter_buckets` shows the error text with `text-error` and logs with `console.error`.

- [ ] **Step 2: Run, see them fail** — `npx vitest run src/components/collab/project/FilterRemapSection.test.tsx`.

- [ ] **Step 3: Implement `FilterRemapSection.tsx`**

```tsx
import { useCallback, useEffect, useState } from 'react';
import { Loader2 } from 'lucide-react';
import { api } from '../../../api';
import { Button, DialogShell, Select } from '../../ui';
import type { CollabRemapFinished, CollabRemapProgress, FilterBucket, FilterBucketSheet } from '../../../types/models';

const HEADER = 'mb-1.5 mt-5 text-[13px] font-semibold text-content first:mt-0';
const bucketKey = (b: FilterBucket) => `${b.publisherAccountId}|${b.instrume}|${b.filterRaw}`;

function finishedText(f: CollabRemapFinished): string {
  if (f.error) return `Remap stopped after ${f.done} of ${f.total}: ${f.error}`;
  if (f.cancelled) return `Remap cancelled — ${f.done} of ${f.total} frames remapped to ${f.canonical}.`;
  const failed = f.failed > 0 ? ` (${f.failed} failed)` : '';
  return `${f.done} of ${f.total} frames remapped to ${f.canonical}${failed}.`;
}

export default function FilterRemapSection({ projectId, syncToken }: { projectId: string; syncToken?: number }): JSX.Element {
  const [sheet, setSheet] = useState<FilterBucketSheet | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [choice, setChoice] = useState<Record<string, string>>({});
  const [confirm, setConfirm] = useState<{ bucket: FilterBucket; canonical: string } | null>(null);
  const [running, setRunning] = useState<CollabRemapProgress | null>(null);
  const [result, setResult] = useState<string | null>(null);
  const [startError, setStartError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const s = await api.invoke<FilterBucketSheet>('get_collab_filter_buckets', { projectId });
      setSheet(s);
      setRunning(s.running);
      setLoadError(null);
    } catch (err) {
      console.error('[FilterRemapSection] get_collab_filter_buckets failed:', err);
      setLoadError(String(err));
    }
  }, [projectId]);

  useEffect(() => {
    void load();
  }, [load, syncToken]);

  useEffect(() => {
    let cancelled = false;
    let unProgress: (() => void) | undefined;
    let unFinished: (() => void) | undefined;
    api.listen<CollabRemapProgress>('collab-remap-progress', (p) => {
      if (cancelled || p.projectId !== projectId) return;
      setRunning(p);
    })
      .then((fn) => { if (cancelled) fn(); else unProgress = fn; })
      .catch((err) => console.error('[FilterRemapSection] listen progress failed:', err));
    api.listen<CollabRemapFinished>('collab-remap-finished', (f) => {
      if (cancelled || f.projectId !== projectId) return;
      setRunning(null);
      setResult(finishedText(f));
      void load();
    })
      .then((fn) => { if (cancelled) fn(); else unFinished = fn; })
      .catch((err) => console.error('[FilterRemapSection] listen finished failed:', err));
    return () => {
      cancelled = true;
      unProgress?.();
      unFinished?.();
    };
  }, [projectId, load]);

  const start = useCallback(async () => {
    if (!confirm) return;
    const { bucket, canonical } = confirm;
    setConfirm(null);
    setResult(null);
    setStartError(null);
    try {
      await api.invoke('remap_collab_filter', { projectId, frameUuids: bucket.frameUuids, canonical });
    } catch (err) {
      console.error('[FilterRemapSection] remap_collab_filter failed:', err);
      setStartError(String(err));
    }
  }, [confirm, projectId]);

  const cancel = useCallback(async () => {
    try {
      await api.invoke('cancel_collab_remap', { projectId });
    } catch (err) {
      console.error('[FilterRemapSection] cancel_collab_remap failed:', err);
    }
  }, [projectId]);

  return (
    <>
      <h3 className={HEADER}>Filters</h3>
      <p className="mb-2 text-[12px] text-content-faint">
        Remap a publisher's raw filter name to a canonical filter. Stacking groups and exports follow on the next plan; no file changes.
      </p>
      {loadError && <p className="text-[12.5px] text-error">{loadError}</p>}
      {running && (
        <div className="mb-2 flex items-center gap-2 text-[12px] text-content">
          <Loader2 size={12} className="animate-spin" />
          Remapping… {running.done + running.failed} of {running.total}
          <Button onClick={() => void cancel()}>Cancel</Button>
        </div>
      )}
      {result && <p className="mb-2 text-[12px] text-content">{result}</p>}
      {startError && <p className="mb-2 text-[12.5px] text-error">{startError}</p>}
      {sheet && sheet.buckets.length === 0 && <p className="text-[12px] text-content-faint">No frames in this project yet.</p>}
      {sheet && sheet.buckets.length > 0 && (
        <table className="w-full text-[12px]">
          <thead className="text-left text-content-faint">
            <tr>
              <th className="py-1 font-medium">Publisher</th>
              <th className="py-1 font-medium">Camera</th>
              <th className="py-1 font-medium">Raw filter</th>
              <th className="py-1 font-medium">Canonical</th>
              <th className="py-1 text-right font-medium">Frames</th>
              <th className="py-1" />
            </tr>
          </thead>
          <tbody>
            {sheet.buckets.map((b) => {
              const k = bucketKey(b);
              const picked = choice[k] ?? b.canonical ?? '';
              const unchanged = picked === '' || picked === b.canonical;
              return (
                <tr key={k} className="border-t border-border">
                  <td className="py-1 text-content">{b.publisher}</td>
                  <td className="py-1 text-content">{b.instrume || 'Unknown camera'}</td>
                  <td className="py-1 text-content">{b.filterRaw || '(no FILTER)'}</td>
                  <td className="py-1 text-content">{b.canonical ?? 'mixed'}</td>
                  <td className="py-1 text-right tabular-nums text-content">{b.frameUuids.length}</td>
                  <td className="py-1 text-right">
                    <span className="inline-flex items-center gap-1.5">
                      <Select aria-label={`Canonical filter for ${b.filterRaw || 'no FILTER'}`} value={picked} onChange={(e) => setChoice((c) => ({ ...c, [k]: e.target.value }))}>
                        {b.canonical === null && <option value="">Choose…</option>}
                        {sheet.dictionary.map((d) => (
                          <option key={d.canonical} value={d.canonical}>{d.canonical}</option>
                        ))}
                      </Select>
                      <Button disabled={unchanged || running !== null} onClick={() => setConfirm({ bucket: b, canonical: picked })}>Apply</Button>
                    </span>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
      {confirm && (
        <DialogShell
          title="Remap filter"
          onClose={() => setConfirm(null)}
          footer={
            <>
              <Button onClick={() => setConfirm(null)}>Cancel</Button>
              <Button variant="primary" data-autofocus onClick={() => void start()}>Remap</Button>
            </>
          }
        >
          <p className="text-[12.5px] text-content">
            Remap {confirm.bucket.frameUuids.length} frames from '{confirm.bucket.filterRaw || '(no FILTER)'}' to {confirm.canonical}?
            Stacking groups and exports follow on the next plan; no file changes.
          </p>
        </DialogShell>
      )}
    </>
  );
}
```
If `Button` has no `variant="primary"`, use the variant `ProjectExportDialog` uses for its primary action.

- [ ] **Step 4: ModerationTab, ProjectDetail, notifications**

`ModerationTab.tsx`:
- Add props `canModerate: boolean; canRemap: boolean;`.
- Wrap the existing two sections (and their dialogs) in `{canModerate && (<>…</>)}`.
- Append `{canRemap && <FilterRemapSection projectId={projectId} syncToken={syncToken} />}`.
- Update `ModerationTab.test.tsx` renders to pass `canModerate` (and add a case: `canRemap` only → only the Filters header renders).

`ProjectDetail.tsx`:
- The `moderation` tab is visible when `c.canModerate || c.canRemap`.
- Pass `canModerate={c.canModerate} canRemap={c.canRemap}`.
- The pending badge keeps showing only with `canModerate`.
- Add a test in `ProjectDetail.test.tsx`: a card with `canModerate: false, canRemap: true` shows the Moderation tab.

`useCollabNotifications.ts`:
- In `notifyFrameChange`, add:
  ```ts
  case 'remapped':
    notify({ title: `${n} of your ${plural} remapped in ${title}`, detail: 'A coordinator changed their canonical filter. Files are unchanged.', kind: 'project', link, toast: false });
    break;
  ```
- Add a StrictMode-safe listener for `collab-remap-finished`. For an error it notifies `{ title: 'Filter remap stopped', detail: f.error, kind: 'project', tone: 'warning', hasErrors: true, link }`. Otherwise it notifies `{ title: \`Remapped ${f.done} frames to ${f.canonical}\`, detail: title (the project title) + (f.failed ? \` · ${f.failed} failed\` : ''), kind: 'project', tone: f.failed ? 'warning' : 'success', link }`, plus a cancelled variant titled 'Filter remap cancelled'.
- Tests in `useCollabNotifications.test.tsx`: `remapped` produces a history-only entry (`toast: false`); a finished event with an error notifies with `hasErrors`.

- [ ] **Step 5: Run**

`npx tsc --noEmit -p . && npx vitest run src/components/collab src/pages/ProjectDetail.test.tsx src/hooks`
Expected: clean, PASS.

- [ ] **Step 6: Harness check (optional, recommended)**

`npm run ui:harness`, open the project page, Moderation tab, and confirm the Filters section uses the same row height and typography as the review table. Note any deviation in the ledger; do not restyle shared primitives in this plan.

- [ ] **Step 7: Commit** — `feat(collab-ui): Moderation → Filters remap section; remap notifications` (same trailers).

---

### Task 13: Docs, logging dictionary, open-items, final gates

**Files:**
- Modify:
  - `docs/export/README.md` (the project export section)
  - `docs/export/calibrated-lights.md` (line ~24 stale claim; the R3 pass-through)
  - `docs/transfers/README.md` (the collab section: remap, coalescing)
  - `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (dictionary extension)
  - `docs/superpowers/open-items.md` (owner smokes)
  - `CLAUDE.md`

- [ ] **Step 1: `docs/export/README.md` — project export**

Rewrite the project-export section:
- The layout is `<title>/<publisher>/camera_<instrume>/<canonical filter>/<file>`.
- The FILTER rule, with the exact comparison and the `/ was '<raw>'` comment.
- What happens to ATH_FILT, CHECKSUM and the XISF `Instrument:Filter:Name` property.
- Symlink mode: unchanged → link, rewrite → copy.
- Re-export: keep / replace / never write through a link.
- `.athenaeum-export.json` and the stale warning that never deletes.
- One project export at a time.
- The four counts.
- The patchers (`fits_writer::header_patch`, `fits_writer::xisf_patch`) and their limits (distributed XISF refused; the source is never opened for writing).

- [ ] **Step 2: `docs/export/calibrated-lights.md`**

Replace the "Collab publish — deferred (decision C …) 9 collab tests are `#[ignore]`d" paragraph with the current truth:
- Decision C was retired in collab v3 wave 2 (plan P7: the per-frame calibrated verdict is the one gate).
- Publish calibrates at publish time, and since 2026-10-01 goes through Calibrate → Review → Publish.

Add a paragraph on the R3 pass-through (`ExportReadiness.attested`, `check_mode_ready`'s first `CalibratedLights` arm, the original file names, the send path's `generate = false`).

- [ ] **Step 3: `docs/transfers/README.md` — collab**

Add a bullet on:
- the coordinator remap: buckets, the per-frame PATCH with pacing and limits, refusal of a second remap, the 403 and 400 stops, the 429 retry, and that no local write happens;
- the `Remapped` kind for own frames only;
- feed coalescing (P7): the rule, and that `pending` is decremented per absorbed event.

- [ ] **Step 4: Logging dictionary**

Append a "Dictionary extension (collab v3 wave 6a, 2026-10-02)" entry defining:
- `linked`, `copied`, `rewritten` (i32 counts on `"project export organized"` / `"project export finished"`);
- `failed` (the same sense, failed frames — reuse it if already defined);
- `canonical` (string; the canonical filter on `"filter remap started"`);
- `merged` (usize; events absorbed into one pull on `"feed: pull-needing events merged"`);
- `tries` (u32; 429 retries on `"remap: rate limited, waiting"`).

Reuse `project_id`, `frame_uuid`, `count`, `duration_ms`, `error`, `path`.

- [ ] **Step 5: `docs/superpowers/open-items.md`**

Add a section "Collab wave 6a — owner smoke (2026-10-02)":
1. Export a project with a remapped filter in copy mode. WBPP groups the frames by the canonical FILTER.
2. The same export in symlink mode: only the rewritten frames are files.
3. Remap a filter, then re-export into the same folder. The old copy is named in the warning and left on disk.
4. Export an XISF attested frame with a different raw FILTER. The copy opens, and its FILTER is canonical.
5. Remap in Moderation → Filters as the coordinator, and as a member who holds only `thresholds.edit`.
6. Cancel a remap mid-way.
7. An attested set in the export tab's Calibrated lights mode: no masters asked for, original names.
8. Windows: the export without symlinks; a remap.

Delete the stale ledger entries the inventory named, when this plan made them moot:
- the `fetch_collection_multi` "if a collab package" note;
- the "no `sync/project_ingest.rs` test" note.

- [ ] **Step 6: `CLAUDE.md`**

- The command count line: 294 → 297. List the three new commands in the collab parenthetical, in the line's existing style.
- The Calibrated-lights export paragraph: replace "Collab publish of a device's own lights is deferred (`LightCalStatus::NotCalibrated`)." with "An attested set (`frames_set.calibrated_externally`) passes through as is (R3)."
- The Transfers/collab bullets: one line for the remap (Moderation → Filters, per-frame PATCH, one per project) and one for the project export (canonical filter folders, FILTER-patched copies, never writing a replica).

Keep the file a map: details go to the references above.

- [ ] **Step 7: Final gates** (one at a time)

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
CARGO_BUILD_JOBS=4 cargo check --workspace
CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --no-default-features
TS_RS_WRITE=1 CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --test ts_contract && git diff --exit-code src/types
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --all-targets -- --test-threads=4
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-web -- --test-threads=4
npx tsc --noEmit -p .
npx vitest run
```
Expected: all green.
- The `ts_contract` regeneration leaves no diff.
- The two known load-sensitive collab live wall-clock tests may flake under the full parallel suite: re-run the `collab_live` module alone with `--test-threads=2` and record both runs.
- Any other red test is a defect to root-cause before the plan is called done.

- [ ] **Step 8: Commit** — `docs(collab): wave 6a — project export, remap, R3 pass-through; logging dictionary; open-items smokes` (same trailers).

---

## Self-review (done while writing)

**Spec coverage (6a scope):**

| Spec section | Task |
| ---- | ---- |
| §10.1 layout, selection, layout mode (P1), one at a time | 4, 5, 6 |
| §10.2 FILTER rule | 3 |
| §10.3 FITS patcher | 1 |
| §10.4 XISF patcher | 2 |
| §10.5 symlinks, re-export, record, counts | 5, 6 |
| §11.1 UI, caps | 10, 12 |
| §11.2 core, pacing, feed coalescing, `Remapped` | 9, 10, 11 |
| §13 R3 export half | 7 |
| §9.1 hub client `patch_frame_filter` | 8 |
| §14 logging fields | 13 |
| §18 6a docs | 13 |

The 6b/6c items (grid, `put_grid`, the stacking subject) are out of this plan by design.

**Type consistency:**
- `ProjectExportFrame` fields are used identically in Tasks 4–6.
- `PlaceCounts` maps 1:1 onto `ExportResult`'s four fields.
- `CollabRemapProgress` and `CollabRemapFinished` are the same names in Rust (Task 9), the generated TS and the frontend (Task 12).
- `needs_pull`, `absorb` and `coalesce` are the same in Task 11's tests and code.

**Placeholders:**
- None. Where a name must be confirmed against the code (the quick-xml 0.41 reader call, `Button` variants, `requests_to`'s matching form, `lights_by_id`'s shape, the fake hub's default dictionary), the step says which file to read and what to do either way.
