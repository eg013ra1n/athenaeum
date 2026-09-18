# Settings redesign — design

Owner request 2026-09-18 (`docs/backlog-v0.6.5.md` item 4), decisions taken
the same day in dialogue: no Save buttons anywhere; search across every tab;
seven tabs in a fixed order with Account and Sync under Transfers; defaults
from Rust through one command; a registry so search, reset and autosave are
built once; and a document that says where a future setting goes and how it
is written.

## 1. Goals and non-goals

**Goals.**

1. Every change on the Settings page persists on its own — the page has no
   Save button and no "settings saved" banner.
2. Seven tabs, in this order: General · Blink · Analysis · Plate Solving ·
   Calibration · Stacking · Transfers. Account and Sync live under Transfers.
   A new Blink tab holds the Blink viewer, flat contour plot and star
   annotation sections. Frame-set grouping comes before session detection and
   explains itself.
3. One checkbox look everywhere on the page (the house `accent-accent`
   control), including the stacking, analysis, plate-solve and matching panels
   the page renders.
4. A search field that filters sections across every tab.
5. A per-field reset to the Rust default beside every field whose value
   differs from it, plus a per-section reset; the defaults come from one
   backend command.
6. The Logging card stops repeating itself (one level picker, one inherit
   rule, one override banner).
7. `docs/settings/README.md` tells the next contributor where a setting goes,
   which component renders it and how it is registered.

**Non-goals.** No new settings; no change to what any setting means or to
its key; no change to the typed configs' Rust shapes; no redesign of the
per-set Stacking tab (it shares the panels and inherits the checkbox, nothing
else); no migration of stored values; no localStorage preferences moved to
the backend (`lightCalPrefs`, `stackingPrefs` stay where they are).

## 2. Information architecture

| Tab | Sections, in order |
| ---- | ---- |
| General | Updates · Frame set grouping · Session detection · Monitoring · Auto-merge · Content index · Archive · Data file locations · Logging |
| Blink | Blink viewer · Flat contour plot · Star annotation display |
| Analysis | Star detection · Measurement method · PSF fitting · Batch processing · Default rejection thresholds |
| Plate Solving | Star catalog · Solver parameters · Input gate |
| Calibration | Matching (the six collapsible groups as today) · Master build memory · Master file format |
| Stacking | Pipeline defaults (stage list + inspector) · Default folders |
| Transfers | Account · Sync · Folders · Upload speed limit · Simultaneous incoming transfers · Transfer storage |

**Frame set grouping** replaces both today's "Clustering Parameters" block
and the prose card at the bottom of General. Its help text says what the
threshold does — seed-and-grow, single-link on LIGHT RA/Dec, great-circle
distance, only frames not yet in a set — and what a change does not do:
existing sets are not regrouped; "Find new images" and the monitor use the
new value from then on.

**Deep links.** `?tab=<id>` stays; `&section=<id>` is new — the page opens
the tab, scrolls the section into view and flashes its header once. Every
section id is stable and listed in the registry (`general.updates`,
`blink.viewer`, …). Existing links (`?tab=plate_solving`, the `→ Coverage`
style links elsewhere) keep working.

## 3. The registry

`src/settings/registry.ts` is the one description of the page:

```ts
export interface SettingsFieldMeta {
  id: string;            // unique within the section
  label: string;         // the visible label
  help?: string;         // the visible help text
  keywords?: string[];   // extra search terms, e.g. the setting key
}
export interface SettingsSectionMeta {
  id: string;            // 'general.updates' — stable, used by ?section= and search
  tab: SettingsTabId;
  title: string;
  description?: string;
  fields: SettingsFieldMeta[];
}
export const SETTINGS_TABS: { id: SettingsTabId; label: string; icon: LucideIcon }[];
export const SETTINGS_SECTIONS: SettingsSectionMeta[];
```

The registry carries METADATA only — titles, labels, help, keywords, order.
It never holds values, defaults or render code. Two consumers read it: the
search index and `SettingsSection`, which looks its own id up to render the
title and to register itself for `?section=`. A section that renders without
a registry entry throws in development (`console.error` in production), and a
test asserts every registry entry is rendered by exactly one component and
every rendered section has an entry — that is what keeps search honest.

Field labels in the registry and in JSX must not drift: KV fields take their
label from the registry entry (`useSettingField` returns it), and the
hand-written panels reference the same constants (`FIELDS.analysis.maxStars.label`).

## 4. Components

All in `src/components/settings/`, the page itself in `src/pages/Settings.tsx`
reduced to the tab bar, the search field and the tab switch.

- `tabs/{General,Blink,Analysis,PlateSolving,Calibration,Stacking,Transfers}Tab.tsx`
  — one file per tab, each a list of sections. General's sections each get a
  file under `sections/` (`UpdatesSection.tsx`, `FrameSetGroupingSection.tsx`,
  `SessionDetectionSection.tsx`, `MonitoringSection.tsx`, `AutoMergeSection.tsx`,
  `ContentIndexSection.tsx`, `ArchiveSection.tsx`, `DataLocationsSection.tsx`)
  and the Blink tab's three likewise. `AccountSection`, `SyncSection`,
  `TransfersSection`, `StackingSection`, `LoggingSettings`,
  `AnalysisSettingsPanel`, `PlateSolveSettingsPanel`,
  `CalibrationMatchingConfig` keep their files and move under the new tabs.
- `SettingsSection` — `{ id, children, onResetAll? }`: card, title and
  description from the registry, the section-level "Reset all" (visible only
  when `onResetAll` is given), the scroll/flash target for `?section=`, the
  search registration.
- `Checkbox` — the one checkbox: native `<input type="checkbox">` with
  `accent-accent`, sizes `sm` (`w-3.5 h-3.5`, the stacking rows) and `md`
  (`w-4 h-4`), `label`, optional `description`, `disabled`. `SwitchRow`
  (Folders) becomes a thin wrapper over it so there is one control in the
  codebase.
- `SettingToggle`, `SettingSelect`, `SettingNumber`, `SettingText` — a KV
  field each: label/help from the registry, value and commit from
  `useSettingField`, the reset affordance, inline validation and error.
- `ResetButton` — the `↺` icon button with title `Reset to default (X)`;
  rendered only when `value !== default`.
- `LevelSelect` — the logging level picker, used once for the base level and
  once per module row with the extra `Inherit (<base>)` option.
- `SettingsSearch` — the input above the tabs, plus `SearchResults`, which
  renders the matching sections in place of the tab body.

## 5. Autosave

One rule, stated in the README and enforced by the hooks:

- **Discrete controls** (checkbox, radio, select, slider) commit on change,
  debounced 300 ms so a slider drag is one write.
- **Text and number inputs** commit on blur and on Enter; Escape restores the
  last committed value. While the draft is invalid (out of range, not a
  number, empty where a value is required) the field shows the error inline
  and nothing is written.
- **Feedback.** A successful write is quiet — a `saved` tick fades in beside
  the field for 1.5 s. A failed write shows the error under the field, keeps
  the draft, and raises `notify({ kind: 'generic', tone: 'warning', … })`
  once per failure so the history has it. No page-level banner.

Two hooks carry it:

```ts
// KV settings — one key, string on the wire, typed in the hook.
useSettingField<T>(key: string, codec: Codec<T>): {
  value: T; draft: string; setDraft(s): void; commit(): Promise<void>;
  setValue(v: T): Promise<void>;   // discrete controls
  error: string | null; saving: boolean; savedAt: number | null;
  defaultValue: T; isDefault: boolean; reset(): Promise<void>;
  meta: SettingsFieldMeta;
}
// Typed configs — one document, saved whole.
useAutosaveDocument<D>(opts: {
  load(): Promise<D>; save(d: D): Promise<void>; defaults: D | null;
  debounceMs?: number;             // 500 by default, the StackingSection value
}): { doc: D | null; patch(p: Partial<D> | (d: D) => D): void;
      error: string | null; saving: boolean; savedAt: number | null;
      isDefault(path: string): boolean; resetField(path: string): void;
      resetAll(): Promise<void>; }
```

`useAutosaveDocument` is `StackingSection`'s debounce generalized: a
`dirtyRef` so a load never writes, a pending-document ref, an unmount flush,
and one in-flight write at a time (a patch during a write queues the next
write; the last document wins). `StackingSection` switches to it; Analysis,
Plate Solving, Calibration Matching and Logging adopt it and drop their Save
buttons, `saved` timers and success banners. `CalibrationMatchingConfig`'s
"Refresh All Calibration Sets" stays a button — it is an action, not a
setting. So do "Build index now", "Clean up …" and the folder pickers.

The device name and the hub URL (Account) follow the text rule: commit on
blur/Enter, no button. A device rename is a network-visible change, so its
field help says so.

**Values that need a restart** (the transfer working folder) keep their
"Restart Athenaeum to apply" badge exactly as today; autosave does not change
when a value takes effect.

## 6. Defaults and reset

One command on both hosts:

```
get_settings_defaults → SettingsDefaults {
  kv: Record<string, string>,        // every key in settings::defaults, as stored
  analysis: AnalysisConfig,
  plateSolve: PlateSolveConfig,
  calibrationMatching: CalibrationMatchingConfig,
  logging: LoggingConfig,
  stacking: StackingConfig,
}
```

built in `athenaeum-core::api::settings::get_settings_defaults` from the SAME
constructors the `reset_*` commands use (`AnalysisConfig::default()`, …) and
`settings::defaults`, so a default can never differ between "reset" and
"show me the default". The frontend loads it once per Settings mount
(`useSettingsDefaults`, a context) and every field compares against it.

- **Field reset** writes the default through the field's own commit path —
  it is a normal change, not a special command.
- **Section reset** for KV sections writes every field's default; for a typed
  config it calls the existing `reset_*` command and reloads (the shape those
  commands already have). A confirm dialog, the same `ConfirmDialog`
  `StackingSection` uses today.
- The four existing `reset_*` commands stay; `get_settings_defaults` adds one
  read. A Rust test pins that each typed default in the response equals what
  the matching `reset_*` writes.

## 7. Search

`useSettingsSearch(query)` builds the index from the registry once: for each
section, the title, description, every field label, help and keyword, plus
the tab label. Matching is case-insensitive substring over the normalized
text (diacritics folded), terms AND-ed. With a non-empty query the tab bar
stays but is inert and the body shows `SearchResults`: each matching section
rendered by its normal component inside its card, with a tab chip in the
header and the matching field labels highlighted. Editing works in place —
the results are the real sections, not copies. Clearing the query returns to
the previous tab. `Escape` in the search field clears it; `/` focuses it when
no input is focused.

A section is searchable only if it is in the registry — the registry test
keeps that true.

## 8. Logging

`LoggingSettings` keeps its command surface (`get_logging_config`,
`set_logging_config`) and loses its duplication: the base level and the five
module rows render `LevelSelect`; the "absent means inherit" rule lives in
one `toModuleValue`/`fromModuleValue` pair; the `ATHENAEUM_LOG` override is
stated once, in the section's description, and the success toast goes with
the Save button. The log-directory and database paths move into "Data file
locations", where they already are, and the Logging section links to it.

## 9. Error handling and logging

- A failed `set_setting` / `set_*_config`: field error + `notify` warning,
  value stays as the user typed it, `console.error` with the key.
- `get_settings_defaults` failing: the page still renders and edits; reset
  affordances are hidden and a one-line note under the search field says
  defaults could not be loaded.
- The registry test failing is a build-time failure, never a runtime one.

## 10. Testing

Frontend tests run under Vitest with `@testing-library/react` (new dev
dependencies — the frontend has no test runner today; `npm test` is added).
Pinned:

- `useSettingField`: a discrete change writes once after the debounce; a
  text draft does not write until blur/Enter; an invalid draft never writes
  and shows the error; Escape restores; reset writes the default through the
  same path.
- `useAutosaveDocument`: a load never writes; two patches inside the window
  write once with the last document; an unmount flushes a pending write; a
  write failure keeps the draft and exposes the error.
- Registry: every `SETTINGS_SECTIONS` id is rendered by exactly one
  `SettingsSection` across the seven tabs, and every rendered section has an
  entry (a render of each tab with the hooks mocked).
- Search: title, label, help and keyword matches; AND of terms; the result
  order follows the registry.
- Rust: `get_settings_defaults` equals the `reset_*` outputs; the `kv` map
  covers every key in `settings::defaults`.
- Manual (open-items): the desktop click-through of each tab, the search,
  a field reset, a section reset, the restart badge.

## 11. Migration and compatibility

- No stored value changes. Keys, config shapes and the four `reset_*`
  commands are untouched; one command is added on both hosts.
- `?tab=` values stay valid; `blink` is the only new one.
- `SwitchRow` keeps its props (Folders page unchanged).
- The per-set Stacking tab renders the same panels and gets the `Checkbox`
  through them; its board-row toggle keeps its own `sm` size.

## 12. Documentation

`docs/settings/README.md` — the contract for the next setting:

1. Decide KV or typed. A single scalar with a default goes to
   `settings/mod.rs` (`keys` + `defaults`) and is read through
   `SettingsManager`; a group of related values that travels together is a
   typed config with `get/set/reset` on both hosts.
2. Register the field in `src/settings/registry.ts` (section, label, help,
   keywords) — this is what makes it searchable and resettable.
3. Render it with the matching field component inside its section; for a
   typed config, patch through `useAutosaveDocument`.
4. Add the default to `get_settings_defaults` (automatic for KV — the map is
   built from `settings::defaults`; a typed config's default is its `Default`).
5. Style: sentence-case labels, help in one sentence saying what the value
   does and what it does not, units in the label (`(hours)`, `(MB)`), the
   default not restated in the help (the reset tooltip shows it).
6. Never add a Save button; never build a banner; `notify()` on failure only.

CLAUDE.md gets a ten-line "Settings page" paragraph pointing at the README,
the registry and the two hooks.
