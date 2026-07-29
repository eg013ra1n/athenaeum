# Folders Screen Redesign — Design

- **Date:** 2026-07-29
- **Status:** Approved (brainstorm with owner, visual-companion session; mockups in `.superpowers/brainstorm/88248-1785348825/content/`)
- **Scope:** The File Manager's "Monitored Directories" tab only. Browse Files / Duplicates / Missing Metadata tabs are untouched.

## 1. Problem

The Monitored Directories tab grew by accretion into five stacked sections: the scan-root list, then designator sections for Calibration Folder, Sync Incoming, Collaboration, and Archive Folders. Resulting UX debt:

- **Duplicated information.** A standalone special folder (calibration library, sync incoming, collaboration) appears TWICE: once in the main scan-root list — rendered identically to a normal root, with all three checkboxes and a Delete button that dead-ends on the backend deletion guard — and once in its own designator section.
- **Meaningless controls.** Checkbox labels ("Unique camera", "Monitor") explain themselves only via hover tooltips; irrelevant controls are shown on special roots.
- **Invisible rules.** The placement rules (special roots must live outside every monitored directory; the calibration folder may alternatively live inside one; archive destinations are never scanned and may live anywhere) surface only as post-hoc Conflict errors.
- **Inconsistent interactions.** `window.confirm`/`alert` in some sections vs `ConfirmDialog`/`AlertDialog` in others; archive-folder add is desktop-only (`alert` in web mode).

## 2. Decisions (all approved by owner)

| # | Decision |
| ---- | ---- |
| D1 | Structure: **master–detail workspace** — left rail (~300px) + inspector pane. The five sections are replaced; each folder exists exactly once, in the rail. |
| D2 | Rail rows carry a **rescan `↻` control next to the folder name**, **always visible**; spinner + percent while scanning; disabled when offline. |
| D3 | Trouble surfaces as **badges in the rail only** ("3 missing", "offline") — no extra summary strip. |
| D4 | Add affordance: **global "＋ Add Folder" button** at the top of the rail opening a teaching type-picker, **plus** placeholder rows with a "Set up…" button for unassigned roles. No group-header ＋ buttons. |
| D5 | Icons: **lucide, role-tinted** with Nord hues via design tokens (blue folder, purple library, cyan inbox, green users, yellow archive). No emoji, no monochrome-only. |
| D6 | Tab renamed **"Monitored Directories" → "Folders"**. |
| D7 | Add Folder dialog is a **teaching dialog**: placement rule shown *before* picking a directory; the picked path is validated inline with a human-readable explanation instead of a raw backend Conflict. |
| D8 | Calibration-library folder switch becomes **one step** (small backend change) with an honest confirmation (old dedicated root's master catalog rows are removed; files on disk untouched). |
| D9 | Offline folders: red badge in the rail + a warning banner with **Relink** inside the inspector; the rest of the inspector is read-only while offline. Empty state (zero folders): centered "No folders yet" panel with a single "＋ Add Folder" button. |

## 3. Screen architecture

`FileManager.tsx` keeps its four tabs; the first tab is renamed **Folders** and its content becomes a two-pane workspace:

```
┌─ Folders ─ Browse Files ─ Duplicates ─ Missing Metadata ──────────────┐
│ ┌───────────────┐ ┌───────────────────────────────────────────────┐  │
│ │ ＋ Add Folder  │ │  <Inspector for the selected folder>          │  │
│ │ MONITORED      │ │                                               │  │
│ │  ▸ rows…       │ │                                               │  │
│ │ SPECIAL ROLES  │ │                                               │  │
│ │  ▸ rows…       │ │                                               │  │
│ │ ARCHIVE DEST.  │ │                                               │  │
│ │  ▸ rows…       │ │                                               │  │
│ └───────────────┘ └───────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────────────────────┘
```

- Selection state is local to the tab; default selection = first monitored folder (or nothing + empty-state hint when the rail is empty).
- The existing deep-link from Transfers (`navigate('/files', { state: { focusSyncIncoming: true } })`) now selects the Sync Incoming rail row and opens its inspector (scroll-into-view of the old designator section is dropped).
- The `reveal` deep-link to the Browse Files tab is untouched.

## 4. Rail

Three fixed groups, in order: **Monitored**, **Special roles**, **Archive destinations**. Group headers are plain labels (no ＋).

Row anatomy (all folder kinds):

- Role-tinted lucide icon (`Folder`, `Library`, `Inbox`, `Users`, `Archive` — tint via design tokens).
- **Name** = folder basename, bold. Sub-line = parent path; for role rows the sub-line leads with the role label ("Calibration Library · /Volumes/Astro"); for archive rows it appends "N sets · X GB".
- Badges after the name where applicable: `3 missing` (warning tone), `offline` (error tone).
- **`↻` rescan button** (scanned kinds only), always visible, right-aligned; spinner + "scanning… NN%" in the sub-line while a scan runs; disabled at reduced opacity when offline.
- Archive rows: `★` marker on the default destination; no `↻`.

Special-role placeholder rows (role not assigned): dashed border, dimmed role name, one-line purpose ("for received project files"), and an explicit **Set up…** button that opens the Add Folder dialog pre-selected on that role.

Sort: monitored folders alphabetically; special roles in fixed order (Calibration Library, Sync Incoming, Collaboration — assigned or placeholder); archive destinations with default first, then alphabetically.

## 5. Inspector states

### 5.1 Monitored folder

- Header: name + `Monitored` chip; full path with **reveal** (desktop only); primary **↻ Scan now**; **Relink…** secondary.
- Stat chips: files cataloged, bytes on disk, last scan (+ "N new" from last result), watch interval ("every 30 min" when watching / "manual only").
- **Behavior** section — three switches, each with title + full description (no tooltips):
  - *Watch for new files* → `monitor_enabled`. "Re-scan this folder periodically in the background. The interval is global — Settings → Scanning."
  - *Include in duplicate detection* → `find_duplicates`. "Files here are content-hashed and compared against every other folder with this enabled."
  - *Treat camera as unique to this folder* → `unique_camera`. "Two rigs with the same camera model? Keeps their calibration frames apart. Takes effect after the next scan."
- **Needs attention** section (rendered only when non-empty): missing-files row (count + `Review ▸` expanding the existing `MissingFilesPanel`), parse-errors row (count + expandable log, from `last_scan_errors`/last result).
- **Remove** section: explanation ("Forgets the folder and its catalog entries. Files on disk are never touched.") + danger button → existing `ConfirmDialog` → `delete_scan_root`.

### 5.2 Special-role folder (assigned)

- Header: name + role chip (role color).
- Role explainer card in place (what the folder does, key facts — e.g. masters count for the library).
- Stat chips: placement ("standalone · own scanned folder" or, for the calibration library covered by a monitored root, "inside <root>"), last scan.
- **Role** section: **Change folder…** and **Release role** buttons with inline explanation ("Releasing keeps the folder monitored and never touches files.").
- **Behavior** section — only applicable switches are shown (hidden, not disabled):

| Switch | Monitored | Calibration Library | Sync Incoming | Collaboration |
| ---- | ---- | ---- | ---- | ---- |
| Watch for new files | ✓ | ✓ | ✓ | ✓ |
| Include in duplicate detection | ✓ | — | ✓ | ✓ |
| Unique camera | ✓ | — | — | — |

- No Remove here: a role folder is released first (it then appears under Monitored and gains the normal Remove). This removes today's dead-end Delete-then-guard-error path. The backend deletion guard stays as a safety net.
- Special case — calibration library covered by a monitored root (settings-key-only, no dedicated root): the rail row still appears under Special roles (single source of truth), sub-line "inside <covering root>"; scan controls and Behavior section are hidden (the covering root owns scanning); Role section works the same.

### 5.3 Archive destination

- Header: name + `★ Default destination` chip (or a "Make default" action when not default).
- Explainer: ""Move and ZIP" writes finished frame sets here. Never scanned — it may live anywhere, even inside a monitored folder."
- Stat chips: archived sets count, total size, default status.
- **Contents** section: archived frame sets with per-set zips (moved here from the old expandable rows; same lazy `listArchiveZips` loading; reveal per zip on desktop; "missing" marker preserved).
- **Remove** section: "Removes it from this list only — zips on disk stay." → `deleteArchiveRoot`.

### 5.4 Offline folder (any scanned kind)

- Rail: `offline` badge, dimmed icon, disabled `↻`.
- Inspector: red banner — "Folder not reachable — drive unmounted, renamed or moved. The catalog still remembers all N files." + **Relink — point to new location…** button (existing `relink_scan_root`; result panel with matched / new / orphaned counts as today). Everything else read-only.

### 5.5 Empty state

Shown only when there are zero folders of ANY kind (no scan roots, no archive roots). The rail is hidden entirely; a centered panel fills the tab: folder icon, "No folders yet", one line ("Add a folder with your FITS/XISF files to start cataloging. Roles and archive destinations can come later."), single **＋ Add Folder** button. As soon as one folder exists, the normal rail + inspector layout renders (placeholder role rows included).

## 6. Add Folder dialog

One dialog, two steps, reached from: the global rail button, placeholder "Set up…" rows (pre-selected role), and the empty state.

**Step 1 — type picker.** Five entries, each icon + name + one-line description:

- Monitored folder — "Watch a folder of FITS/XISF files and catalog everything in it."
- Archive destination — "Where "Move and ZIP" stores finished sets. Not scanned."
- Calibration Library / Sync Incoming / Collaboration — role descriptions; an already-assigned role renders with ✓ + its current path, disabled.

**Step 2 — pick + validate.** Before the directory picker opens, the placement rule for the chosen type is displayed (e.g. "A monitored folder can't sit inside another monitored folder — pick a separate directory."; roles: "…must be its own folder, outside your monitored folders."; calibration library: "…may be inside a monitored folder, or standalone — a standalone folder is also scanned."; archive: no rule beyond exists). After picking (native picker on desktop, `FolderBrowserModal` on web — including archive destinations, fixing the web-mode gap), the path is validated via the new dry-run command and any conflict is shown inline in the dialog with the human-readable explanation; the confirm button stays disabled until the path validates.

## 7. Flows

- **Add monitored / archive** → existing `add_scan_root` / `addArchiveRoot` after inline validation passes.
- **Set up role** → existing `set_sync_incoming_dir` / `set_collaboration_dir` / `set_calibration_library_dir`.
- **Change role folder (sync / collaboration)** → UI runs `clear_*` then `set_*` sequentially (no backend change); on `set` failure the UI reports honestly that the role is now unassigned and the old folder remains monitored.
- **Change calibration library folder** → new one-step `switch_calibration_library_dir` (see §8), behind a `ConfirmDialog` spelling out the consequences.
- **Release role** → existing `clear_*` commands (demote to normal / clear setting), confirm first; the folder then appears under Monitored.
- **Remove monitored** → existing `delete_scan_root` with the current confirm.
- **Relink** → existing `relink_scan_root`.
- **Scan** → existing `startRescanWithProgress`; per-root progress drives the rail spinner and the inspector; the `ScanSummaryModal` remains the post-scan report.

## 8. Backend changes (small; both backends in the same change, per the two-backend rule)

All logic in `athenaeum-core` (`api::scan_roots` or a new `api::folders` module), thin Tauri/Axum wrappers, `#[tracing::instrument(skip_all, err)]`, new model types registered in `ts_export.rs`.

1. **`switch_calibration_library_dir(path)`** — one transaction-shaped flow: validate the new path (same checks as `set_calibration_library_dir`); if an old dedicated `calibration_library` root exists, delete it (same catalog-purge semantics as `delete_scan_root`, bypassing the special-root deletion guard for this internal step); then run the existing set logic (covered → settings key only; standalone → new dedicated root + settings key). Returns the normalized effective path. Frontend always shows a confirmation that names the purge ("catalog entries of masters under the old folder are removed; files on disk are kept; a rescan re-imports them if the folder stays monitored — it won't").
2. **`validate_folder_candidate(kind, path)`** — dry-run of the `add_scan_root` checks (exists, is-dir, canonicalize, sandbox, overlap in both directions, per-kind uniqueness; archive kind: exists/is-dir only). Returns a typed verdict the dialog can render: `{ ok: true }` or `{ ok: false, reason: "insideExisting" | "containsExisting" | "alreadyMonitored" | "roleTaken" | "notFound" | "notADirectory", detail: { conflictingPath?, rolePath? } }`. Never writes.
3. **`get_folder_overview()`** — per-root stats for rail + inspector in one call: for scanned roots `{ rootId, fileCount, totalBytes }` (SQL aggregate over `files` by path prefix); for archive roots `{ archiveRootId, setCount, totalZipBytes }` (aggregate over archive operation records). One command so the rail renders without N+1 calls.

Existing commands are otherwise reused unchanged: `get_scan_roots` (+availability), toggles, `delete_scan_root` (guard intact), the three get/set/clear triples, archive root CRUD + listings, `relink_scan_root`, `get_missing_files*`.

## 9. Frontend structure

- `src/components/folders/` (new): `FoldersTab.tsx` (layout + selection state + deep-link handling), `FolderRail.tsx`, `FolderInspector.tsx` dispatching to `MonitoredInspector` / `RoleInspector` / `ArchiveInspector` / `OfflineBanner`, `AddFolderDialog.tsx`, `roleMeta.ts` (per-kind icon, tint token, labels, descriptions, switch-visibility matrix — single source of truth shared by rail, inspector, dialog).
- `FileManager.tsx`: the `directories` tab body is replaced by `<FoldersTab/>`; scan/relink/missing-files state and handlers move into it (or a `useFoldersTab` hook). Old section components (`CalibrationFolderSection`, `SpecialFolderSection`, `ArchiveFoldersSection`) are deleted.
- All dialogs use `ConfirmDialog`/`AlertDialog` — no `window.confirm`/`alert` anywhere on the tab (removes the current inconsistency).
- Design tokens only (`bg-surface-elevated`, `text-content-muted`, role tints via the Nord token set); icons from `lucide-react`.
- Notifications: unchanged policy — `notify()` on discrete outcomes via existing handlers.

## 10. Error handling

- Every backend error is logged to console and surfaced via `AlertDialog`/inline dialog text + `notify()` where the section did so before; raw backend text is never hidden (shown as detail when remapped).
- The dialog's inline validation covers the known conflicts; an unexpected error from the actual add/set call still surfaces verbatim (validation is advisory, the backend stays authoritative — TOCTOU between validate and add is acceptable and reported honestly by the add call).
- Offline detection stays as today (`useScanRootsWithAvailability`).

## 11. Testing

- **Core:** unit tests for `switch_calibration_library_dir` (standalone→standalone, standalone→covered, covered→standalone, no-previous), `validate_folder_candidate` (each verdict), `get_folder_overview` aggregates. Existing scan-root tests untouched.
- **Gates:** `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`.
- **Manual smoke (desktop + web):** add monitored; add archive (web included); set up each role from placeholder row and from global button; role change (sync via clear+set, calibration via one-step); release role; remove monitored; offline badge + relink; rail rescan + spinner; missing-files review; archive contents browse; empty state; Transfers deep-link lands on Incoming row.

## 12. Out of scope / follow-ups

- Other File Manager tabs; Settings page; any storage-model unification of archive roots vs scan roots.
- Docs-site guide refresh (artfrom-space) after the feature ships — UI flows change materially.
- Localized UI (labels stay English).
