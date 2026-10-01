# Collab project page — pixel-accurate UI (wave 5.5)

**Status:** approved in dialogue 2026-09-30, written for review.
**Wave:** 5.5 — after wave 5 (project UI), before wave 6 (stacking a project).
**Reference (the ONE visual source):** `docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html`,
rendered in **standards mode** (see §2.1).
**Builds on:** `2026-09-29-collab-project-observability-design.md` (the six-tab page, its amendments §13–§16).
This spec changes presentation only: no core, command, event or model change.

## 1. Goal and success criteria

The project page as built does not look like the approved mockup. The table columns shift while
scrolling. Header labels sit left of right-aligned numbers. Text is one to two sizes larger than the
mockup, so every table wraps: dates become "2026-08- / 31" and durations "18h / 42m". The member and
filter colours differ from the mockup, the frame drawer covers the page and has its own look, and the
dialogs each look different.

Wave 5.5 is done when:

1. Every region of the six tabs, the frame side panel, the member side panel and the collab dialogs is
   within **1 px** of the reference, measured on the same data at the same viewport (§17). The only
   exceptions are the deliberate deviations listed in §19.
2. No table cell wraps. A cell that does not fit truncates with an ellipsis, and a table that does not
   fit scrolls horizontally inside its own box.
3. Column positions do not depend on which rows are rendered. Scrolling a 3,719-row table never moves
   a column.
4. Every modal of the collab page (nine, §7), and `ConfirmDialog` / `AlertDialog` app-wide, share one shell.
5. The page no longer jumps sideways when a tab switch shows or hides the page scrollbar.
6. The final check has been made in the real Tauri window (§17.3).

## 2. Owner rulings (2026-09-30)

| # | Ruling |
| ---- | ---- |
| U1 | **Approach A.** A compact shared layer that mirrors the mockup's CSS: tokens, typography roles and `src/components/ui/` primitives. Every tab is then rebuilt on it. Neither a class-by-class patch nor a second global stylesheet. |
| U2 | **Scope = the project page plus shared primitives.** App-wide changes are limited to the tokens (§3.1), the font family (§3.2), the scrollbar gutter (§3.3), the Nord filter palette (§4.3), `Checkbox`, and `ConfirmDialog` / `AlertDialog` moved onto `DialogShell`. Other pages keep their own font sizes. |
| U3 | **Frame card and member card open as a docked side panel.** The table narrows and stays visible, and the header and tabs are never covered. Clicking another row switches the card. |
| U4 | **Dialog migration order.** Wave 5.5 covers `DialogShell`, `ConfirmDialog`, `AlertDialog`, every collab dialog (§7) and the Columns popover. The other ~22 app dialogs follow in a short wave 5.6, shell only. |
| U5 | **Page header follows the app's own pattern, not the mockup's "← Projects".** `HistoryNav` as on every page, and the title styled like Objects / Equipment / File Manager / Transfers (`text-2xl font-bold` plus a muted inline subtitle). The Projects list page is aligned to the same pattern. |
| U6 | **The reference is the mockup in standards mode.** The mockup file has no `<!doctype>`, so it renders in quirks mode. There, table cells do not inherit the 13 px body size and render at 16 px: `document.compatMode === "BackCompat"`, `td` 16 px, `body` 13 px, measured 2026-09-30. The wave adds `<!doctype html>` to the file so that the committed reference IS the intended design. |
| U7 | **Presentation only.** Anything the model does not provide is not shown (§16). No core, command or event is added. |

### 2.1 Reference rendering

The reference is the mockup file with `<!doctype html>` as its first line (U6). It is compared at a
**1440 × 900** window. The mockup's "Design notes" are switched off, and the MOCKUP bar and its fake
left rail are not part of the design. On the app side the sidebar is collapsed, so both content
columns start at the same x and have the same width.

## 3. Foundation

### 3.1 Tokens (`tailwind.config.js`, app-wide)

Existing tokens already equal the mockup's variables (`surface` = `--bg`, `surface-elevated` =
`--elev`, `surface-hover` = `--hover`, `border` = `--border`, `content` = `--text`,
`content-secondary` = `--text2`, `content-muted` = `--muted`, `accent*`, `success`, `warning`,
`error`, `info`, `orange`, `purple`). Added:

| Token | Value | Mockup source | Used for |
| ---- | ---- | ---- | ---- |
| `content-faint` | `rgba(216,222,233,.62)` | `--faint` | labels, column headers, meta lines |
| `content-ghost` | `rgba(216,222,233,.38)` | `--ghost` | empty "—" cells |
| `line` | `rgba(76,86,106,.55)` | `--line` | card borders, separators |
| `line-soft` | `rgba(76,86,106,.28)` | `table.ft td` border | frame-table row separators |
| `line-plain` | `rgba(76,86,106,.30)` | `table.plain td` border | plain-table row separators |
| `table-head` | `#333a47` | `table.ft th` | sticky header background |
| `table-group` | `#323946` | `tr.g td` | group row, level 0 |
| `table-group-l1` | `#353c4a` | `tr.g.l1 td` | group row, level 1 |
| `table-group-hover` | `#3a4251` | `tr.g:hover td` | group row hover |
| `table-row-hover` | `rgba(67,76,94,.55)` | `table.ft tr:hover td` | frame-table row hover (added in execution, §21 R35) |
| `table-plain-hover` | `rgba(67,76,94,.45)` | `table.plain tr.x:hover td` | clickable plain-table row hover (R35) |
| `table-peer-hover` | `rgba(67,76,94,.30)` | `.peer:hover` | Exchange peer-flow row hover (R35) |
| `teal` | `#8fbcbb` | `--teal` | member palette slot 7 |

The `*-muted` backgrounds of `success`, `warning`, `error` and `info` change from `.25` to `.22`
alpha, as in the mockup. The difference is not visible on other pages. The app has one theme (dark
Nord), so there is no light-theme mapping to maintain.

### 3.2 Typography

- **Font family, app-wide.** The `:root` font changes from `Inter, system-ui, …` to
  `system-ui, -apple-system, "Segoe UI", Roboto, sans-serif`. Inter is not bundled, so today the
  app's font depends on whether the machine happens to have Inter installed. Monospace becomes
  `ui-monospace, "SF Mono", Menlo, Consolas, monospace` (a `font-mono` override in the Tailwind config).
- **The project page root** (`ProjectDetail` content column) is `13px / 1.4` with
  `font-variant-numeric: tabular-nums`. Other pages keep their current sizes (U2).
- **Roles.** Each size is used only for its role:

| Size | Weight | Role |
| ---- | ---- | ---- |
| 10.5 px | 500 | tab count pill |
| 11 px | 400 | chip, group count, gate header, `Button size="sm"` |
| 11.5 px | 400/500 | column header (500), pill, legend, toolbar label, attention detail |
| 12 px | 400 | button, KV, toolbar, totals bar, meta line, card subtitle, file name (mono) |
| 12.5 px | 400 | plain tables, attention title, empty state, side-panel file name (mono) |
| 13 px | 400/600 | base text; card and side-panel section titles (600) |
| 18 px | 600 | My-frames segment number |
| 20 px | 600 | Overview "My contribution" number |
| 24 px | 700 | page title (U5) |

### 3.3 No sideways jump

The scroll container in `Layout.tsx` (`<div ref={contentRef} className="flex-1 overflow-auto">`)
gets `scrollbar-gutter: stable`. This applies app-wide.

## 4. Shared primitives — `src/components/ui/` (new)

Each primitive reproduces one mockup rule. The rule is named in its doc comment, and exact values
live in the primitive, never repeated at call sites.

### 4.1 Components

| Primitive | Mockup rule | Props / variants |
| ---- | ---- | ---- |
| `Button` | `.btn`, `.btn.pri`, `.btn.sm`, `.btn[disabled]`, `.linkbtn` | `variant: default \| primary \| danger \| link`, `size: md \| sm`. Danger = error border + text; primary danger = error background. |
| `Pill` | `.pill` | `children`, optional leading dot |
| `Chip` | `.chip` + `.c-ok/.c-warn/.c-err/.c-info/.c-mute` | `tone` |
| `Seg` | `.seg` | options, value, onChange |
| `SegmentTiles` | `.segs button` | tiles `{ n, label, tone }`, active |
| `Card` | `.card`, `.card h2`, `.sub` | `title`, `subtitle`, `action` (right side) |
| `KV` | `.kv` (`auto 1fr`, gap 3/14, 12 px) | `[label, value][]` |
| `FilterDot` | `.fdot` (8 px, radius 2) | filter name → §4.3 colour |
| `MemberDot` | `.dot` (7 px round) | colour from §4.4 |
| `StatusDot` | `.dot` + `.live-dot` | `online \| offline \| live` |
| `Bar` | `.bar` (8 px) | segments `{ value, color }` |
| `ProgressBar` | `.pbar` (4 px) | percent |
| `Sparkline` | `.spark` | number series, colour |
| `FilterChip` | `.fchip`, `.k`, `.on`, `.zero` | label, count, on |
| `Popover` | `.pop` | anchor, children (Columns menu) |
| `EmptyState` | `.empty` | text |
| `SidePanel` | §6 | open, onClose, children |
| `DialogShell` | §7 | title, size, footer, onClose |

### 4.2 Existing shared components

- `settings/Checkbox.tsx` — the app's one checkbox — is restyled to `.cb` / `.cb.on` / `.cb.mid`:
  13 px, radius 3, accent fill, indeterminate bar. App-wide.
- `ConfirmDialog.tsx` and `AlertDialog.tsx` render through `DialogShell`. Their props do not change,
  so their 15+ callers are untouched.

### 4.3 Filter palette (`utils/filterColors.ts`, app-wide, the one source)

L `#e5e9f0` · R `#bf616a` · G `#a3be8c` · B `#5e81ac` · Ha `#d08770` · OIII `#88c0d0` ·
SII `#b48ead` · OSC `#ebcb8b`.

The existing alias matching (`h-alpha`, `o3`, `s2`, `lum`, …) stays. Unknown filters keep a stable
colour from the unknown palette. The analysis charts and the export cards pick the new colours up
automatically.

### 4.4 Member palette (`collab/project/memberColors.ts`)

The palette is `accent #88c0d0`, `success #a3be8c`, `purple #b48ead`, `warning #ebcb8b`,
`orange #d08770`, `info #81a1c1`, `teal #8fbcbb`, `error #bf616a`. **This account always gets
`accent`** (the mockup's "You"). The other members take the remaining seven in their existing
deterministic order (displayName, then accountId), cycling. Assignment is by that rule, so on the
harness data the palette values match the mockup exactly, while which member gets which colour
follows the rule rather than the mockup's hand-picked order.

## 5. Frame tables (`ProjectFrameTable` — My frames, Library, Moderation)

### 5.1 Geometry

- `table-layout: fixed`, `width: 100%`, `border-collapse: separate`, with a `<colgroup>` that gives
  every column its mockup width. Widths come only from the column definition, never from content,
  so the windowed rendering can never move a column.

| Column | Width | Align | Column | Width | Align |
| ---- | ---- | ---- | ---- | ---- | ---- |
| checkbox | 32 | — | Why held back | 230 | left |
| Frame | rest (min 220) | left | Ver | 44 | right |
| Publisher | 108 | left | Status | 112 | left |
| Night | 98 | left | Holders | 88 | right |
| Filter | 70 | left | On disk | 84 | left |
| Camera | 122 | left | Published | 142 | left |
| Exp / Σ | 82 | right | On this device | 168 | left |
| FWHM″ | 66 | right | Size | 78 | right |
| Ecc | 60 | right | SNR | 58 | right |
| Stars | 64 | right | | | |

- **Header.** Sticky, 30 px, `table-head`, 11.5 px weight 500 `content-faint`, padding `0 8px`,
  bottom border `border`. Numeric headers are right-aligned. A sortable header turns `content` on
  hover; the sorted header is `accent` with ` ↑` / ` ↓`.
- **Data row.** Cell height 28 px including its 1 px `line-soft` separator (border-box, 28 px pitch —
  measured in the mockup and the app; the windowing constant `ROW_H` is 28, §21 R17), 13 px
  `content-secondary`, padding `0 8px`, no wrap, ellipsis. File name: mono 12 px.
- **Row states.** Hover `rgba(67,76,94,.55)`. Selected (checkbox) `rgba(136,192,208,.08)`. Active
  (its side panel is open) `rgba(136,192,208,.16)`.
- **Group rows.** `table-group` (level 0) and `table-group-l1` (level 1), weight 500 `content`,
  hover `table-group-hover`. Caret: 14 px wide, `content-faint`. The group count sits after the
  label, 11 px `content-faint` with a 6 px left margin ("374 fr"). Numeric group cells are
  `content-muted` weight 400: `x̃ 2.42`, `Σ 18h 42m`, sizes as sums.

### 5.2 Surroundings (top to bottom)

1. **Filter row** (`.toolbar`, gap 6/10, padding 8 0):
   - "Filter" label (11.5 px `content-faint`);
   - one `FilterChip` per filter with its dot and count;
   - a 1 × 18 px `line` separator;
   - selects (26 px high, 12 px) for camera, nights, and — per table — publisher and state;
   - the search input.
2. **Group row:** "Group by" `[select] ▸ [select]`, then `Expand all · Collapse all` as link
   buttons. On the right, "Columns ⚙" opens a `Popover` of checkboxes. Library also has
   "Export for WBPP" (§10).
3. **Totals bar:** `elev` background, 1 px `line` border with no bottom border, radius 6 at the top,
   padding 7 × 10, 12 px `content-muted`. Content: "Showing **2,446** of 2,446 frames",
   "Σ **163h 54m**", "**256.9 GB**", "FWHM x̃ **2.40″**". The table's actions sit on the right.
4. **Table box:** 1 px `line` border, radius 6 at the bottom, `surface` background. Its **height
   fills the window down to its bottom edge, minimum 360 px** (deviation D2; the mockup's fixed
   560 px was a prototype convenience).

### 5.3 Per-table columns (from the mockup's `TABLES`, ZP removed)

| Table | Default grouping | Visible by default | Also available |
| ---- | ---- | ---- | ---- |
| My frames · Ready | Night ▸ Filter | Frame, Filter, Camera, Exp, FWHM, Ecc, Stars, Size | Night, SNR |
| My frames · Held back | Why held back ▸ Night | Frame, Why held back, Night, Filter, Exp, FWHM, Ecc, Stars | Camera, SNR, Size |
| My frames · Published | Night ▸ Filter | Frame, Filter, Exp, Ver, Status, Holders, On disk, Published, Size | Night, Camera, FWHM, Ecc |
| Library | Publisher ▸ Filter | Frame, Publisher, Night, Filter, Camera, Exp, FWHM, Ecc, Holders, On this device, Size | Stars, SNR |
| Moderation | Publisher ▸ Night | Frame, Publisher, Night, Filter, Camera, Exp, FWHM, Ecc, Stars, SNR | Size |

**Cell formats.**

| Column | Frame row | Group row |
| ---- | ---- | ---- |
| Exp | "180 s" | Σ duration |
| FWHM | 2 decimals | "x̃ 2.42" |
| Ecc | 2 decimals | "x̃ 0.41" |
| Stars | en-US grouping | "x̃ 1,107" |
| SNR | 1 decimal | "x̃ 45.4" |
| Size | `formatSize`: "122 MB" / "4.9 GB" | sum |
| Holders | "2 on / 3", or chip "1 copy" (warn) | chip "4 single", else "min 2" |
| Status | status chip | chips for the non-published counts |
| On disk | "yes" (faint), chip "missing" (err), chip "changed" (warn) | — |
| Why held back | chip with the first reason (quality = err, else warn), then "+N" | — |

### 5.4 Plain tables (Members, Exchange sessions, drawer Gate)

`table.plain`: 12.5 px, `border-collapse: collapse`. Header: 11.5 px weight 500 `content-faint`,
padding 6 × 8, bottom border `border`. Cells: padding 6 × 8, bottom border `line-plain`, no wrap.
Numeric columns right-aligned. A clickable row has hover `rgba(67,76,94,.45)`. Too wide → the table
scrolls horizontally inside its wrapper, never wraps.

## 6. Side panel (frame card, member card)

### 6.1 Layout

- While a card is open, the tab body becomes a two-column grid: `minmax(0,1fr) 400px`, gap 14 px.
  The panel sits under the tabs and never covers the page header or tabs (U3).
- The panel is `position: sticky` at the top of the scroll area, with height = the visible area
  below the tabs and its own scroll. Style: `elev` background, 1 px `line` border, radius 8,
  padding 16 × 18 (bottom 30).
- The table narrows. With fixed widths (§5.1), the Frame column absorbs it down to its 220 px
  minimum; below that the table scrolls horizontally. Nothing wraps.
- Clicking another row switches the card, and that row takes the active state. The card closes on
  Esc, on ×, on a second click of the active row, and on a tab change.

### 6.2 Frame card content (the mockup's `drawerHtml`)

1. **Title block.** The file name (mono 12.5 px `content`, `word-break: break-all`), then a `Button
   size="sm"` × on the right. Under it, the status chips: own frame → `ready` (info), `held back`
   (warn), or the publication status; other frame → publication status plus the device state from §5.3.
2. **Frame** (13 px 600 section title, margin 16 0 6), as `KV`:
   - Publisher (member dot + name);
   - Night ("2026-08-31 · Mon");
   - Filter (filter dot + name);
   - Camera;
   - Exposure ("180 s");
   - Size;
   - Version ("v2 · v1 superseded" when > 1);
   - Published (timestamp).
3. **Metrics**: FWHM "2.41″", Eccentricity "0.50", Stars "1107", SNR 1 decimal ("45.4"; today it
   prints `23.054622650146484`). No "Zero point" row: ZP is not computed (§16).
4. **Gate** with subtitle "thresholds vN" — own frames only. A grid `1fr auto auto auto`, gap 3/12,
   12 px; header row (Rule, Value, Needs) 11 px `content-faint`. Each row: rule, value, needs
   (faint), then ✓ (success) or ✕ (error). A rule that was not evaluated shows "—". It lists the
   `rules` verdicts plus the preconditions the frame fails (Plate-solved, Analyzed, Filter mapped).
5. **Who holds it**, with subtitle "N online of M". Row (12.5 px, padding 2 0): status dot, member
   dot, member name, `publisher` chip (mute) on the publisher, device name right-aligned
   (`content-faint`).
6. **On this device**. Own frame: the path (mono 11.5 px `content-faint`, `break-all`) with a copy
   button. Received frame: "Received 2026-09-01 12:49 from Andrei".
7. **Actions** (moderator): `Exclude…` (danger) or `Restore`, at the end of the card.

### 6.3 Member card content

- Title: the member dot, the name (13 px 600), role chip(s).
- **Cameras**: one block per camera — the camera name in bold, then per filter "filter dot, filter,
  N fr · x̃ FWHM 2.41″ · x̃ ecc 0.44" (from `qualityByCamera`). "Nothing published yet." when empty.
- **Devices**: each device with its status dot, name and "online"/"offline".
- **Holds**: "2,849 fr · 308.9 GB · 94 % of the project".

## 7. Dialogs — `DialogShell`

- **Scrim** `rgba(46,52,64,.6)`. **Window** centred, width `sm` 440 / `md` 560 px (max 92 vw),
  `elev` background, 1 px `border`, radius 8, shadow `0 8px 24px rgba(0,0,0,.35)`, padding 14 × 16.
- **Header**: title 13 px 600, `Button size="sm"` × on the right. **Body**: 12.5 px `content-muted`,
  with inputs and selects as in the mockup (26 px high, 12 px, `elev` background, `border`,
  radius 4). **Footer**: right-aligned, gap 8. Cancel is `Button`. Confirm is
  `Button variant="primary"`, or `danger` for a destructive action.
- **Behaviour**: Esc and a scrim click close it, except while an action is running. Focus is
  trapped inside and returns to the opener on close. `role="dialog"`, `aria-modal`, labelled by
  the title.
- **Migrated in 5.5**:
  - `ConfirmDialog` and `AlertDialog` (app-wide);
  - `ProjectExportDialog`, `ExcludeDialog`, `RepublishGuardDialog`, `LinkObjectDialog`,
    `FilterMappingDialog`, `DeviceReplaceDialog`;
  - the two inline dialogs: the publish confirm "Publish to …" in `ProjectDetail.tsx` and the
    reject-reason dialog in `ModerationTab.tsx`;
  - the attention confirm in `CollabAttention` (already a `ConfirmDialog`);
  - the Columns menu as `Popover`.
- `LinkObjectDialog` also takes over the "Linked objects" list from the My-frames header (§9):
  linked objects with unlink, then suggestions.
- **Wave 5.6** (not this wave): the remaining ~22 app dialogs, shell only.

## 8. Page header and tabs

- **Row 1** (U5):
  - `HistoryNav fallback="/projects"` exactly as on other pages;
  - title `text-2xl font-bold` (24 px);
  - inline subtitle `text-sm font-normal text-content-muted ml-3`: "◎ M31 · r 1.5° · 8 members";
  - the role chip (`coordinator`, info);
  - on the right: `Pill` "● Live · synced 4 s ago" (live dot) and the link button
    "Manage on portal ↗".
  - "synced N s ago" ticks every second from the card's `fetchedAt`.
  - Clicking the pill runs Sync now; it reads "Syncing…" meanwhile. There is no separate button.
    Other live states reuse the pill: "Connecting…", "Reconnecting in 12 s", "Offline" (error
    dot), "Signed out".
- **Row 2**, meta line 12 px `content-faint`: "▣ Publishing from this device · Auto-publish on ·
  Auto-replicate on".
  - "Auto-publish on/off" and "Auto-replicate on/off" are toggles styled as text: accent on hover,
    the word flips on click. Their explanations move into tooltips.
  - Publishing elsewhere → "Publishing from kostya-obs · Publish from here" (link button).
  - The `AutoReplicateBar` card and "66.59 GB published" are removed.
- **Projects list page**: its header is aligned to the same pattern (`HistoryNav`, `text-2xl
  font-bold` "Projects"); its body is unchanged.
- **Tabs**:
  - bar: margin-top 14, bottom border `border`;
  - tab: padding 8 × 14, `content-faint`; active: `content`, weight 600, 2 px `accent` underline;
  - counts are 10.5 px pills (`surface-hover` background, `content-muted`, radius 999, padding 0 5):
    My frames "136 ready", Library "278 to go", Moderation "153" (warning tone);
  - the tab body starts 14 px below.

## 9. Overview

A grid `minmax(0,1.5fr) minmax(0,1fr)`, gap 14, top-aligned. Every block is a `Card`.

- **Integration toward goal** — subtitle "published, accepted frames · by member".
  - One row per filter: grid `54px 1fr 150px`, gap 10, margin 9 0.
  - Label: filter dot + name.
  - Bar: 14 px, radius 3, `surface-hover` background, one segment per member in their colour.
  - Goal marker: 2 px `content` line, 3 px above and below the bar.
  - Right text, 12 px `content-muted`: "**8h 16m** of 40h · 31h 44m to go" (warning), or "… ·
    goal met" (success), or "**8h 16m**" with no goal.
  - Legend under the rows: 11.5 px `content-faint`, member dot + name.
- **My contribution** — three tiles (`grid 3 × 1fr`, gap 8, 1 px `line`, radius 6, padding 8 × 10,
  hover border `accent`). Number 20 px 600: ready = accent, published = success, held back =
  warning. Label 11.5 px `content-faint`: "ready to publish", "published", "held back". Clicking a
  tile opens that My-frames segment.
- **Needs attention** — rows `.att`:
  - Row style: 12.5 px, padding 7 0, a `line` top border between rows. A count chip in its tone,
    then a title and an 11.5 px `content-faint` detail, then a `Button size="sm"` on the right.
  - Rows are derived from data the page already loads; each row appears only when its count is
    greater than 0:
    - **Not plate-solved** (own held back, failure `solve`): "N frames from 2026-09-26 are not
      plate-solved" (or "N frames are not plate-solved" across nights). Detail "They cannot be
      published until solved." Button **Review** → My frames · Held back, reason filter.
    - **Not analyzed** (`analyze`): "N frames are not analyzed" · **Analyze** (runs the existing
      batch action).
    - **Unmapped filter** (`filterMapped = false`): "N frames with an unmapped filter “S2 6nm”".
      Detail "Map it once; the gate re-checks them." Button **Map** → `FilterMappingDialog`.
    - **Fail quality thresholds** (`threshold`): "N frames fail the quality thresholds" ·
      **Review** → My frames · Held back.
    - **One copy only** (own published, `holdersTotal = 0`): "N published frames exist in one copy
      only · 3.9 GB". Detail "If that device is lost, the project loses them." Button **Show** →
      My frames · Published, state "Only one copy".
    - **Not on disk / changed** (own published): "N published frames are missing or changed on
      this device" · **Show**.
    - **Missing, holders offline** (Library `wanted` with no online holder): "N frames missing here
      because their holders are offline". Detail names the offline holders and how long they have
      been offline, from the member summary: "Irina offline 1 day, Olga offline 3 days". Button
      **Show** → Library, state "Missing".
    - **Wait for approval** (`canModerate`, pending): "N frames wait for your approval". Detail
      "First publications by Irina and Pavel." Button **Moderate**.
  - Chip tones: solve / analyze / map / threshold / wait for approval = warn; one copy / missing /
    not on disk = err.
  - The old "N library frames still to come" row is dropped; the Library tab count carries it.
- **Exchange now** — `Card` action "Open Exchange →" (link button). Rows: "↓ **42.0 MB/s** from
  Kostya, Masha" and "↑ **23.3 MB/s** to Kostya, Andrei". EmptyState "Nothing is moving." when idle.
- **Quality thresholds** — subtitle "vN · set by the coordinator". Lines: "FWHM ≤ 3.00″",
  "Eccentricity ≤ 0.55", "Stars ≥ 150", "Reject trailed frames". An unknown key renders as
  `key op value`.

## 10. My frames, Library, Moderation

- **My frames**:
  - `SegmentTiles`: "**136** Ready to publish" (accent), "**594** Published" (success), "**390**
    Held back" (warning). Min width 150, number 18 px, label 12 px; the active tile has an `accent`
    border and a `rgba(136,192,208,.08)` fill. On the right of the same row: "+ Link an object" and
    "Recalibrate and republish all" (`Button`).
  - The "Linked objects" strip moves into `LinkObjectDialog` (§7). The auto-publish switch moves to
    the header (§8).
  - Totals actions:
    - Ready: "Publish all N" (primary; on a selection: "Publish N");
    - Held back: "Solve", "Analyze";
    - Published: "Republish" (the guard dialog stays).
- **Library**:
  - No "Project frames" heading. "Export for WBPP" sits on the group row's right, next to "Columns".
  - The attention lists (Changed files, Waiting for your choice, Not kept, Other files) render as
    `.att` rows in one `Card` above the filter row, only when any list is non-empty. "Other files"
    stays one collapsed row with a count.
  - On this device:
    - "● have" (success dot);
    - "○ queued" (faint);
    - downloading: `ProgressBar` 60 px + "42%";
    - `Chip` "missing" (err) + the reason in 11 px faint ("publisher offline", "holder offline",
      "gone from disk");
    - "not kept" (faint).
  - Group aggregate: a stacked `Bar` 80 px (have success / downloading accent / queued faint /
    missing error / not kept ghost), then "N missing" in 11 px error.
  - Actions: "Keep again" and, for a moderator, "Exclude".
- **Moderation**:
  - The frame table is grouped Publisher ▸ Night.
  - Totals bar: "Approve all N" (primary), "Reject all N", then `Checkbox` "Trust these publishers".
  - Below it, section title "Excluded frames" (13 px 600) and the same table with a "Restore" action.
  - "This project publishes without review." is a meta line when approval is off.

## 11. Members

A plain table (§5.4). Columns in the mockup's order:

1. **Member** — member dot + **name** (600).
2. **Role** — the role; for the coordinator, "Coordinator (Processor data)" with the parenthesis in
   `content-faint`.
3. **Devices** — one 7 px dot per device (success online / `border` offline, 2 px apart); the device
   name is the tooltip.
4. **Published ↓**.
5. **One column per filter**, in the order L R G B Ha OIII SII OSC, then others, then "No filter".
   Only filters with data or a goal get a column. The header shows the `FilterDot` and the name.
   Cells show "3h 58m", or "—" in `content-ghost`.
6. **Σ** — bold.
7. **FWHM x̃** — "2.30″".
8. **Holds** — "2,849 fr · 308.9 GB " followed by "94%" in `content-faint`.
9. **Last seen** — "now" in success, otherwise "3 days ago" in `content-faint`.

Every column sorts. A row click opens the member card (§6.3). Durations everywhere on the page use
`formatDurationPadded`: "3h 02m", "45h 57m", "18m".

## 12. Exchange

- **Receiving** `Card`:
  - Subtitle: "43.4 MB/s from 2 members · 157 frames to go · waiting for publisher: 12".
  - One row per flow (`.peer`): grid `28px minmax(140px,1.2fr) minmax(160px,2fr) 90px 128px 70px`,
    gap 10, padding 9 × 4, a `line` top border between rows, hover `rgba(67,76,94,.3)`.
  - Row content:
    - avatar (26 px circle, member colour, initial, 11 px 700, `surface` text);
    - "**↓ from Kostya**" with "kostya-obs · 2 in flight" (11.5 px faint);
    - a `Bar` with the caption "37 landed this session · 4.8 GB" (11 px faint);
    - rate (600, right);
    - `Sparkline`;
    - "ETA 9m" (12 px faint, right).
  - A row click expands the in-flight list (padding-left 42, rows `1fr 160px 80px`: mono name,
    `ProgressBar`, "42% · 122 MB").
- **Sending** `Card`: the same, "↑ to Kostya", "21 served this session · 2.7 GB".
- **Received** `Card`:
  - Subtitle: "sessions · a session ends after 5 min without a landing".
  - Plain table columns: Started (timestamp), From (member dot + name, comma-separated), Frames,
    Size, Duration, Avg rate, Outcome.
  - Outcome is `Chip` "landed" (ok), or "partial · N failed" (warn) when `failed > 0`.
- EmptyStates: "Nothing is being received." / "Nothing is being sent." / "No sessions yet."

## 13. Empty, loading and error states

- Empty: `EmptyState` (12.5 px `content-faint`, padding 10 0).
- Loading: the same line, reading "Loading…".
- Load error: 12.5 px `error`, "Could not load … — see console". This does not replace
  `notify()`, which still reports action failures.

## 14. Accessibility and interaction

- Tabs: `role="tablist"`, `aria-selected`. Tables keep their existing roles and labels. `Checkbox`
  stays a real input.
- Every icon-only control keeps an `aria-label`.
- Focus-visible outline everywhere: 2 px `accent`, offset 1 px (mockup rule).
- `prefers-reduced-motion`: no transitions. Essential animation — the busy spinners — keeps running
  (a frozen spinner reads as a stuck icon; §21 R35).
- Every overlay that handles Escape is on the overlay stack, including the always-mounted
  notification and transfers panels, the update dialog and the Held-back group menus, so one
  Escape closes exactly the top layer (§21 R35).

## 15. Frontend files

- **New:** `src/components/ui/*` (§4.1) with tests. In `src/components/collab/format.ts`, the
  mockup's formatters: `formatSize` (the mockup's `fmtB`, decimal units: "122 MB", "4.9 GB",
  "1.25 TB") and `formatDurationPadded` (`fmtDur`: "3h 02m", "18m"). The project page uses only these.
  The existing binary `formatBytes` ("116.3 MB") and `formatDuration` keep their current non-project
  callers (the Transfers page is out of scope), until that page gets its own pass.
- **Changed:**
  - `tailwind.config.js`, `src/index.css`, `src/components/Layout.tsx`;
  - `src/utils/filterColors.ts`;
  - `src/components/settings/Checkbox.tsx`, `ConfirmDialog.tsx`, `AlertDialog.tsx`;
  - `src/pages/ProjectDetail.tsx`, `src/pages/Projects.tsx` (header only);
  - `src/components/collab/project/**` (all tabs, `frames.tsx`, `table/ProjectFrameTable.tsx`,
    `FrameDrawer.tsx` → `FramePanel`, `memberColors.ts`);
  - `src/components/collab/{CollabAttention, CollabLiveStatus, AutoReplicateBar, AutoPublishSwitch,
    LinkObjectDialog, FilterMappingDialog, ProjectExportDialog, DeviceReplaceDialog}.tsx`;
  - the exchange presentation in `src/components/collab/project/ExchangeTab.tsx`.
- **Not changed:** any Rust crate, any command, any event, `src/types/models.ts`.

## 16. Not shown — the model does not provide it (U7)

| Mockup element | Why not | What renders instead |
| ---- | ---- | ---- |
| "Zero point" in the drawer, ZP column | no zero point is computed (v3 R9 unbuilt) | row and column omitted |
| "N queued" per peer flow | `FlowView` has no per-peer queue | "N in flight" only |
| "upload streams 3 of 4" in Sending | the upload cap is not in any view | omitted |
| per-flow landed-vs-queued bar | as above | `Bar` = byte progress of the in-flight items |
| rate history | not in the model | `Sparkline` over the existing 40-sample `state.rates` history per flow (`RATE_HISTORY_MAX`), filled by the 1 Hz `collab-exchange-progress` events; empty after a reload |

## 17. Harness and verification

### 17.1 Harness (`scripts/ui-harness/`, dev-only, never bundled)

- `fixtures.mjs` — the mockup's deterministic generator (seed 20260929: 3,719 frames, 8 members),
  moved out of the mockup script and mapped to the app's API shapes: `ProjectCard`, `ProjectDetail`,
  `ProjectFrameView[]`, `OwnFrameRow[]`, `MemberSummary[]`, `ExchangeSnapshot`,
  `ModerationFrameView[]`, `FrameHolderView[]`, `CollabAttention`, `CollabLiveStatus`,
  `CollabStorageStatus`, `ReceiveSessionView[]`.
- `server.mjs` — a mock of the web API on `127.0.0.1:8790`:
  - `POST /api/<command>` for the project page and the app shell's startup commands;
  - an idle SSE `/api/events`;
  - an unknown command is logged once and answered with `[]` (`list_*`) or `null`;
  - CORS for the Vite origin.
- `npm run ui:harness` starts the server, then `VITE_TARGET=web VITE_API_BASE_URL=http://127.0.0.1:8790
  vite --port 1430 --strictPort`. The page is `http://localhost:1430/projects/p-m31`.
- The mockup gets `<!doctype html>` (U6).

### 17.2 Per-task check

1. Open two Chrome tabs at 1440 × 900: the reference, and the harness with the sidebar collapsed.
2. Compare screenshots region by region.
3. Measure with `getBoundingClientRect` / `getComputedStyle` on both pages: column left edges and
   widths, row pitch, header and cell font size / weight / colour, card paddings, the tab underline.
4. A difference above 1 px, or any colour or size difference not listed in §19, is a defect. The
   measurements go into the task's ledger entry.

### 17.3 Final check in Tauri

After the harness checks pass, check the real desktop window (WebKit) on the owner's data.
Screenshots need the terminal to hold the macOS Screen Recording permission (one-time, the owner's
action). Until then this step is reported as not done, never as passed.

## 18. Tests

- Existing vitest suites are updated where text or structure changes (tab counts, header, Members
  columns, attention rows, drawer → panel).
- New tests:
  - `DialogShell`: Esc closes, a scrim click closes, focus is trapped and returns to the opener,
    no close while busy.
  - `SidePanel`: a row click switches the card; a tab change closes it; Esc closes it.
  - `ProjectFrameTable`: the `<colgroup>` widths are the defined widths whatever rows are in the
    window; numeric headers carry the right-align class.
  - Formatting: `formatDurationPadded` ("3h 02m", "18m"), `formatSize` ("122 MB", "4.9 GB",
    "1.25 TB"), SNR to 1 decimal.
  - `memberColors`: this account is always `accent`; others follow the rule.
  - `filterColors`: the Nord values and aliases.
  - Needs-attention derivation: each row's count and tone from fixture rows.
- Rust is untouched. Before the push: the full vitest run, `tsc --noEmit`, and the full core suite
  (standing rule).

## 19. Deliberate deviations from the mockup

| # | Deviation | Reason |
| ---- | ---- | ---- |
| D1 | Header uses `HistoryNav` + a 24 px bold title, not "← Projects" + 17 px | U5 — consistency with every other page |
| D2 | Frame-table height fills the window (min 360 px), not a fixed 560 px | the fixed height was a prototype convenience |
| D3 | Frame and member cards are a docked side panel; the mockup overlays the frame card and expands member rows inline | U3 |
| D4 | Post-mockup controls placed in the mockup's language: auto-publish / auto-replicate toggles in the meta line, Sync on the Live pill, linked objects in the Link dialog, Export for WBPP on the group row, the Library attention card, Moderation's Reject all / Trust these publishers / Excluded frames | the mockup predates them |
| D5 | ZP, per-peer queued, upload streams not shown | §16 |
| D6 | Which member gets which colour follows a rule (this account = accent) rather than the mockup's hand-picked order | §4.4 |
| D7 | Table cells at 13 px, not the 16 px the quirks-mode mockup shows | U6 |

## 20. Review focus

The inputs most likely to break the layout; each has a test or a harness check in the plan.

1. **Long names** — a 60-character frame file name, a 30-character member or device name, a long
   project title. Expected: they truncate with an ellipsis (tables, header) or break by character
   (panel title); no row grows and no column moves.
2. **Many filters** — 12 distinct filters, including unmapped raw names. Expected: the Members table
   scrolls horizontally inside its box, the Overview rows stay one line each, and unknown filters
   keep a stable colour.
3. **Narrow window** — a 1100 px window with the side panel open. Expected: the Frame column
   shrinks to 220 px, then the table scrolls horizontally; the panel stays 400 px; the header wraps
   its right side under the title, never overlapping.
4. **Empty project** — no members' frames, no own frames, no flows. Expected: every card and table
   shows its EmptyState line, with no blank boxes or NaN.
5. **Live states** — connecting, reconnecting, offline, signed out, and publishing from another
   device. Expected: the pill and the meta line show each state in the same geometry; Sync is
   disabled while it cannot run.

## 21. Implementation notes (execution rulings, 2026-09-30 – 2026-10-01)

Decisions taken while the plan ran. Each one either sharpens this spec or records where the build
departs from it. The full record, with the cost of each if wrong, is in the plan's execution ledger.

| # | Ruling |
| ---- | ---- |
| R1 | Interim only: the old drawer got `aria-label="Frame details"` one task before `FramePanel` replaced it. |
| R2 | Harness threshold keys are the gate registry's (`fwhm_arcsec`, `eccentricity`, `stars_detected`, `not_trailed`); the Overview thresholds card keys on them. |
| R3 | The harness answers commands it does not model with `null`, or `[]` for `list_*`; an object-shaped `list_*` (`list_terminal_transfers`) needs its own fixture. |
| R4 | `formatDurationPadded` rounds total minutes first ("2h 00m", never the mockup's "1h 60m"). |
| R5 | `SegmentTiles` keeps a space between number and label so the accessible name reads "136 Ready to publish". |
| R6 | `Pill` accepts `disabled` and dims (opacity .45, no hover). |
| R7 | The live dot's glow is a token ring (`ring-success/[0.18]`), not a raw shadow. |
| R8–R9 | One module-level overlay stack (dialog, popover, panel; panels at the bottom). Only the top overlay handles Escape and calls `preventDefault`; only the top dialog traps Tab; `data-autofocus` wins initial focus; a scrim closes only when press and release both land on it; `DialogShell` portals to `document.body`. |
| R10 | The settings switch folds into `Checkbox`; `AlertDialog` closes with the shell's ×. |
| R11 | `Checkbox`'s label is `relative`, so the hidden input stays inside the scroller. |
| R12 | Initial focus: a field if the dialog has one; never a destructive button; with no field and a destructive action, Cancel. |
| R13 | The checkbox box sits 3.5 px down to centre on a 20 px line. |
| R14 | The member-colour context lives in `MemberColorsContext.tsx` (a case-only clash with `memberColors.ts` breaks imports on a case-insensitive disk). |
| R15, R19 | Where the plan's pixel values disagreed with the mockup, the mockup won: row indent 8 + 18·depth (+14 for a frame row), bars fill their cell, "Clear filters" 12 px, search 170 px, caret 13 px; the table's min-width sits on a wrapper `div`. |
| R16 | Checkbox column 34 px (§5.1's 32 was a transcription error). |
| R17 | `ROW_H` = 28 (§5.1 amended). |
| R18 | `useFillHeight` also refits from a `ResizeObserver` on its parent and on `document.body`. |
| R20 | Header row 1 never wraps: a long title truncates, and the pill and portal link stay on the row (§20.3 "never overlapping" is met by truncation). |
| R21 | Tabs have no `-mb-px` (it clipped the underline); header gap 14 px, meta-line margin 6 px. |
| R22 | A zone-less `fetchedAt` is parsed as UTC; the pill's age rolls s → m → h → d and resets after its own Sync; form controls inherit `font-variant-numeric` (base rule in `index.css`). |
| R23 | `ExcludeDialog` moved onto `DialogShell` together with `FramePanel`, which opens it. |
| R24 | The panel's Gate grid keeps every blocker's explanation, one row per cause. |
| R25 | Overview attention counts come from the same predicates the opened table filters on; the missing-frames row shows only when this member receives; Map opens the filter-mapping dialog; "not analyzed" navigates to Held back instead of running analysis from Overview. |
| R26 | My frames: the row buttons take the tile box (150 × 33); the Link dialog lists linked objects with Unlink, then the remaining suggestions; `AutoPublishSwitch` removed. |
| R27 | Export for WBPP lives on the Library group row, so an empty Library has none. |
| R28 | Each Library attention list shows its note as a visible faint line, not a tooltip only. |
| R29 | Moderation's "Excluded frames" keeps its 20 px top margin; `TextArea` joins the field primitives. |
| R30 | Members: FWHM x̃ and Devices sortable; filter columns = filters with data or a goal; a member with no data shows a ghost "—"; names truncate at 16 rem; the member card lists one filter per line under each camera (a deliberate deviation). The member-name cell is `align-top`: the truncating name made the baseline-aligned row 1.25 px taller than the mockup's 30.5. |
| R31 | Exchange: the Received table's Started shows seconds through an opt-in `formatTimestamp(iso, { seconds: true })` (every other caller keeps HH:MM); the From cell is plain inline dot + name. |
| R32 | `PeerFlowRow` sets its own 13 px / 1.4 so it renders the same on Transfers; the in-flight size column is a fixed 110 px on one line (the mockup's 80 px column wraps it). |
| R33 | Dialogs on the shell: the web folder browser renders inside the export dialog's shell, and Escape closes it before the dialog; a self-opening device prompt focuses "Not now", never the irreversible Replace; the republish guard shows decimal sizes like the rest of the page; Link's footer button is "Done" (the shell's × is "Close"). |
| R34 | The `link` button variant keeps the mockup's UA padding (1px 6px, 18.8 px tall); My frames' row buttons are left-aligned like the mockup's segment buttons. Scenario polish: the truncated project title shows in full on hover; a member holding nothing reads a ghost "—" in Holds; the reconnect countdown reads "Reconnecting…" at zero. |
| R35 | Final review: the notification and transfers panels, the update dialog and the Held-back group menus joined the overlay stack (one shared `useOverlayEscape`), so one Escape closes one layer; reduced motion keeps spinners; an unknown member gets no colour (neutral), never this account's; Escape typed in a field outside the side panel leaves it open; selects take the §14 focus outline; the Card heading is named by its title alone; a busy dialog keeps Tab inside; "Last seen" ticks every minute; the Overview's offline-holders line says when it is the live exchange that is off; three hover tokens replace raw colours; `Button size="tile"` replaces the My frames overrides. |
| R36 | Post-fix sweep of card insets: the Received card is the mockup's flush table card (`Card flush`: padding 12px 0 4px, header padded 16), the Exchange cards sit 12 px apart (the plan's 14 was off), and the Members table sits in a padding-0 card (radius 8, elevated background). |
