# Settings page — how a setting is added

The contract for the next setting: where it lives, how it is registered, which
component renders it and how it is written. The design is
`docs/superpowers/specs/2026-09-18-settings-redesign-design.md` (§12 is the
six-rule summary this file expands); the plan that built it is
`docs/superpowers/plans/2026-09-18-settings-redesign.md`. Everything below is
read from the code as it stands at `5707a317`.

The page has **no Save button and no "saved" banner**. Every change persists on
its own through one of two hooks, the registry makes it searchable and
resettable, and the defaults come from one Rust command.

## 1. Decide: KV or typed config

| Kind | What it is | Rust side | Frontend hook |
| ---- | ---- | ---- | ---- |
| **KV** | One scalar with a default, stored as a string under one key in `settings` | `settings/mod.rs` — a `keys::` constant, a `defaults::` constant, **and a pair in `defaults::all()`**; read in Rust through `SettingsManager` | `useSettingField(section, field, key, codec)` |
| **Typed config** | A group of values that travels together as one JSON document | Its own struct with `Default`, plus `get_*` / `set_*` / `reset_*` on BOTH hosts (`AnalysisConfig`, `PlateSolveConfig`, `CalibrationMatchingConfig`, `LoggingConfig`, `StackingConfig`) | `useAutosaveDocument<D>({ load, save, resetAll?, defaults })` |

A KV key with its own command (`set_blink_threads`, `set_integration_band_budget`,
`set_sync_upload_limit`) stays KV — pass `write` / `read` overrides to the hook.
A key that carries **no** default (`account.*`, `sync.*_dir`, `stacking.presets`,
`calibration.library_dir`) is deliberately absent from `defaults::all()` and gets
no reset affordance.

## 2. Register it — `src/settings/registry.ts`

The registry is METADATA only (titles, labels, help, keywords, order) — never a
value, a default or render code. Two consumers read it: the search index
(`useSettingsSearch`) and `SettingsSection` (title/description, `?section=`).

```ts
{ id: 'general.monitoring', tab: 'general', title: 'Monitoring', description?: '…',
  fields: [
    { id: 'enabledGlobal', label: 'Enable background monitoring',
      help: 'Master switch. When off, no scan roots are polled …',
      keywords: ['monitoring.enabled_global'] },   // put the KV key here
  ] }
```

- `fieldMeta(section, field)` and `sectionById(id)` **throw** on a miss — a typo
  is a crash in every environment, never a silently unlabeled field.
- KV field components take their label/help from the registry, never from a
  prop; hand-written panels reference the same entry
  (`fieldMeta('analysis.batch', 'concurrentFrames').label`).
- A new section must also be listed in its tab file's `*_SECTIONS` array
  (`src/components/settings/tabs/<Tab>Tab.tsx`, `{ sectionId, element }`);
  `tabs/index.ts` derives `sectionComponent(id)` from those lists for search.
  A panel that renders several sections lists the SAME element object against
  each id so it mounts once.

**Tabs, in order** (`SETTINGS_TABS`): `general` · `blink` · `analysis` ·
`plate_solving` · `calibration` · `stacking` · `transfers`.

| Tab | Section ids (`SETTINGS_SECTIONS`) |
| ---- | ---- |
| General | `general.updates` `general.grouping` `general.sessions` `general.monitoring` `general.autoMerge` `general.contentIndex` `general.archive` `general.dataLocations` `general.logging` |
| Blink | `blink.viewer` `blink.flatContour` `blink.annotations` |
| Analysis | `analysis.detection` `analysis.measurement` `analysis.psf` `analysis.batch` `analysis.rejection` |
| Plate Solving | `plateSolving.catalog` `plateSolving.solver` `plateSolving.inputGate` |
| Calibration | `calibration.matching` `calibration.memory` `calibration.masterFormat` |
| Stacking | `stacking.pipeline` `stacking.folders` |
| Transfers | `transfers.account` `transfers.sync` `transfers.folders` `transfers.upload` `transfers.receiving` `transfers.storage` |

## 3. Render it — the field components and the autosave rule

All in `src/components/settings/`, always inside a `<SettingsSection id="…">`
(the card: title/description from the registry, the section-level "Reset all",
the `?section=` scroll target, the search highlight).

| Component | Kind | Commit rule |
| ---- | ---- | ---- |
| `SettingToggle` | KV boolean (`boolCodec`) via the house `Checkbox` | discrete |
| `SettingSelect` | KV enum (`enumCodec([...])`) | discrete |
| `SettingNumber` `variant="slider"` | KV number as `<input type="range">` | discrete |
| `SettingNumber` (default `variant="input"`) | KV number (`intCodec(min,max)` / `floatCodec(min,max)`) | text |
| `SettingText` | KV string (`stringCodec(maxLen?)`) | text |
| `Checkbox` | the ONE checkbox (`accent-accent`, `size` `sm`/`md`, `role` `checkbox`/`switch`) — bind it yourself inside a typed panel | — |
| `LevelSelect` | the logging level picker (+ `inherit: { base }` for a module row) | — |

**Discrete** controls (checkbox, select, slider) commit on change through
`setValue`, debounced **300 ms**, so a slider drag is one write. **Text/number**
inputs edit `draft` freely and commit on **blur or Enter** (`commit()`);
**Escape** restores the last committed value (`escape()`). `commit()` runs
`codec.parse` then `codec.validate?` — while the draft is invalid the error
shows inline and **nothing is written**. A typed document
(`useAutosaveDocument`) debounces **500 ms** by default; a load never writes,
two patches inside the window write once with the last document, and an
unmount flushes a pending write.

**Feedback.** Success is quiet: `SavedTick` (a `Check`) fades in beside the
field for 1.5 s. Failure keeps the draft, shows the error under the field and
raises exactly one `notify({ kind: 'generic', tone: 'warning', title:
'Setting not saved', detail: '<label>: <error>', dedupeKey: key })` (typed
documents: `'<label> not saved'`). The unmount flush never notifies.

## 4. Defaults and reset

`get_settings_defaults` (both hosts → `athenaeum_core::api::settings`) returns
`SettingsDefaults { kv, analysis, plateSolve, calibrationMatching, logging,
stacking }` built from `settings::defaults::all()` and the same `T::default()`
constructors the `reset_*` commands write. `SettingsDefaultsProvider` loads it
once per Settings mount; `useSettingsDefaults()` exposes `{ defaults, error }`.
If the call fails the page still renders and edits; reset affordances are
hidden and one line under the search field says so.

- **A KV default is automatic** once the key is in `defaults::all()`; the Rust
  test `every_key_with_a_default_is_listed` (`settings/mod.rs`) fails when a
  key is added without its pair. A typed default is its `Default` impl
  (`typed_defaults_equal_what_reset_writes` in `api/settings.rs` pins it).
- **The one TypeScript-held exception**: `blink.annotation_config` stores `""`
  meaning "the built-in object", and that object is
  `DEFAULT_ANNOTATION_SETTINGS` (`src/types/helpers.ts`) — the same one
  `BlinkViewer` falls back to, aliased as `BUILT_IN_ANNOTATION_CONFIG` in
  `StarAnnotationSection.tsx`. Do not add a second such exception.
- **Field reset** — `ResetButton` (`↺`, title `Reset to default (X)`) renders
  only while `!isDefault`; `reset()` writes the default **through the field's
  own commit path** — a normal change, not a special command. Typed panels use
  `isDefault(path)` / `resetField(path)` on the document.
- **Section reset** — `SettingsSection` shows "Reset all" (confirm dialog
  `Reset <title>?`) when either an explicit `onResetAll` is given (a typed
  document's `resetAll()`: the `reset_*` command, then a reload; Logging has no
  such command and writes `defaults.logging`) or at least one `useSettingField`
  underneath registered itself in the section's `ResetAllContext` — a KV
  section needs no wiring; a field without a real default never registers.
- **`resetScopeLabel`** overrides the confirm text when the reset is bigger
  than the card — a whole document (`"This resets the whole Analysis
  configuration back to its default."`) or one with a caveat (Stacking: per-set
  overrides and folders are untouched).

## 5. Search and deep links

`useSettingsSearch(query)` builds the index once from the registry: per section
the title, description, tab label and every field's label, help and keywords,
lower-cased with diacritics folded. Terms are AND-ed per section; results keep
registry order. `SearchResults` renders each match by its REAL element (edits
commit in place) with a tab chip, and `matchedFieldIds` (`<section>/<field>`)
ring-highlights the matching field rows (a hand-written panel highlights at
card level). While a query is active the tab bar is inert; clearing returns to
the previous tab; `/` focuses the field, Escape clears it. **A section is
searchable only if it is in the registry** — put the KV key and synonyms in
`keywords`.

Deep links: `?tab=<tab id>` selects the tab; `&section=<section id>` scrolls
that card into view and flashes it for 1.2 s, e.g.
`/settings?tab=calibration&section=calibration.masterFormat`. The DOM id is
`settings-<section id>`.

## 6. Style

- Labels in sentence case; units in the label — `Session gap threshold
  (hours)`, `Memory Cache Limit (MB)` (or `SettingNumber`'s `unit` prop).
- Help is one sentence saying what the value does and what it does **not** do.
- Do not restate the default in the help — the reset tooltip shows it. (Older
  copied labels still say `Default: 85`; do not add new ones.)
- Design tokens only (`text-content-muted`, `bg-surface-hover`, `text-error`).
- **Never a Save button, never a banner** — actions that are not settings
  (Build index now, Clean up, Refresh All Calibration Sets, folder pickers)
  stay buttons; `notify()` on **failure only**.

## 7. Worked example A — a KV toggle (`monitoring.enabled_global`)

Rust, `crates/athenaeum-core/src/settings/mod.rs`:

```rust
pub mod defaults {
    pub const MONITORING_ENABLED_GLOBAL: &str = "true";
    pub fn all() -> &'static [(&'static str, &'static str)] {
        &[ /* … */ (super::keys::MONITORING_ENABLED_GLOBAL, MONITORING_ENABLED_GLOBAL), /* … */ ]
    }
}
pub mod keys {
    pub const MONITORING_ENABLED_GLOBAL: &str = "monitoring.enabled_global";
}
```

Registry entry: the `general.monitoring` block shown in §2 (field
`enabledGlobal`, keyword `monitoring.enabled_global`). Section,
`src/components/settings/sections/MonitoringSection.tsx` — the toggle is
discrete, the interval is a text commit whose range is the codec's, and
`onValueChange` lets the sibling follow the toggle:

```tsx
export function MonitoringSection() {
  const [enabled, setEnabled] = useState(true);
  return (
    <SettingsSection id="general.monitoring">
      <div className="space-y-4">
        <SettingToggle
          section="general.monitoring"
          field="enabledGlobal"
          settingKey="monitoring.enabled_global"
          onValueChange={(v) => { if (typeof v === 'boolean') setEnabled(v); }}
        />
        <SettingNumber
          section="general.monitoring"
          field="intervalMinutes"
          settingKey="monitoring.interval_minutes"
          codec={intCodec(1, 1440)}
          disabled={!enabled}
        />
      </div>
    </SettingsSection>
  );
}
```

Listed in `tabs/GeneralTab.tsx` as `{ sectionId: 'general.monitoring', element:
<MonitoringSection /> }`. Nothing else: the default, the `↺`, the section's
"Reset all" and search all follow from the three pieces above.

## 8. Worked example B — a typed-config field (`LoggingSettings`)

`src/components/settings/LoggingSettings.tsx` binds one `LoggingConfig`
document; each control patches a path and the hook writes the whole document:

```tsx
const { doc, patch, error, resetAll } = useAutosaveDocument<LoggingConfig>({
  load: async () => {
    const resp = await api.invoke<LoggingConfigResponse>('get_logging_config');
    setEnvOverrideActive(resp.envOverrideActive);
    return resp.config;
  },
  save: (config) => api.invoke('set_logging_config', { config }),
  // No reset_logging_config command exists — the default document IS the reset.
  resetAll: async () => {
    if (!defaults?.logging) throw new Error('defaults not loaded yet');
    await api.invoke('set_logging_config', { config: defaults.logging });
  },
  defaults: defaults?.logging ?? null,
  label: 'Logging settings',
});
// …
<SettingsSection id="general.logging" onResetAll={resetAll}>
  <LevelSelect label="Base log level" value={doc.level} onChange={(level) => patch({ level })} />
  <LevelSelect
    label={m.label}
    value={toModuleValue(doc.modules, m.key)}
    onChange={(value) => patch((prev) => ({ ...prev, modules: fromModuleValue(prev.modules, m.key, value) }))}
    inherit={{ base: doc.level }}
  />
</SettingsSection>
```

A numeric field in a typed panel keeps a string draft and calls
`patch({ max_stars: n })` on blur/Enter (`AnalysisSettingsPanel.tsx`'s
`DocNumberField`); its `↺` is `isDefault('max_stars')` / `resetField('max_stars')`.
A whole-document reset passes `resetScopeLabel`.

## 9. Tests to extend

`npm test` (Vitest + Testing Library) and `npx tsc --noEmit`; Rust:
`cargo test -p athenaeum-core --lib -- settings:: api::settings`.

| Change | Test that must keep passing / grow |
| ---- | ---- |
| New KV key | `every_key_with_a_default_is_listed` (`settings/mod.rs`) — add the key to `defaults::all()` |
| New typed config | `typed_defaults_equal_what_reset_writes` (`api/settings.rs`) + a `SettingsDefaults` field; regenerate TS with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` |
| New registry entry | `src/settings/registry.test.ts` (unique ids, valid tab, non-empty labels) |
| New or moved section | `src/components/settings/tabs/registry-coverage.test.tsx` — every registry id rendered by exactly one tab, nothing rendered unregistered |
| A section's Reset all | the `AutoMergeSection.test.tsx` shape: "Reset all" calls every field's reset |
| Search terms you rely on | `src/hooks/useSettingsSearch.test.ts`, `SearchResults.test.tsx` (`'gap'`, `'blink jpeg'`) |
| Hook behaviour | `useSettingField.test.tsx`, `useAutosaveDocument.test.tsx` — only when the hooks change |

## 10. Checklist (spec §12)

1. KV: `keys::` + `defaults::` + `defaults::all()`; typed: struct `Default` + `get/set/reset_*` on both hosts + a `SettingsDefaults` field.
2. Registry entry (section id, label, help, keywords incl. the key); section listed in its tab's `*_SECTIONS`.
3. Field component inside `SettingsSection`; typed → `patch` through `useAutosaveDocument`.
4. Default: automatic for KV, `Default` for typed — never restated in TypeScript.
5. Sentence-case label, units in the label, one-sentence help, no default in the help.
6. No Save button, no banner, `notify()` on failure only.
