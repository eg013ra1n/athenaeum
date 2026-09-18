# Settings Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the Settings page per the spec — seven tabs in a fixed order, every change persisted on its own with no Save button, one checkbox, a registry that drives search and per-field reset, defaults from one Rust command, and a README that tells the next contributor where a setting goes.

**Architecture:** A metadata registry (`src/settings/registry.ts`) describes tabs → sections → fields; `SettingsSection` renders a registered section; KV fields go through `useSettingField` (one key, autosave, reset), typed configs through `useAutosaveDocument` (one document, debounced, `StackingSection`'s discipline generalized). `get_settings_defaults` (both hosts) is the single source of defaults. The 1636-line `Settings.tsx` becomes a tab bar + search + seven tab files.

**Tech Stack:** React 19 + TypeScript, Tailwind tokens, Vitest + @testing-library/react (new), Rust (`athenaeum-core` `api::settings`, Tauri command, Axum route), `ts_rs`.

**Spec:** `docs/superpowers/specs/2026-09-18-settings-redesign-design.md` — every rule below cites its section.

## Global Constraints

- Tab order and section membership exactly as spec §2. Section ids are the registry's.
- No Save button, no success banner anywhere on the page (spec §5). Actions (Build index now, Clean up, Refresh All Calibration Sets, folder pickers, Reset all) stay buttons.
- Discrete controls commit on change (300 ms debounce); text/number inputs commit on blur or Enter, Escape restores, invalid never writes (spec §5).
- One checkbox component with `accent-accent` (spec §4) — no other `type="checkbox"` markup remains under `src/components/settings/`, `src/pages/Settings.tsx`, `src/components/stacking/panels/`, `src/components/stacking/StageRow.tsx`, `src/components/analysis/`, `src/components/plate-solve/`, `src/components/calibration/`.
- Defaults never restated in TypeScript except the annotation config's built-in object (spec §6; the one exception, documented in the README).
- Two backends in sync for the one new command; `#[tracing::instrument(skip_all, err)]` per CLAUDE.md; TS regenerated with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`.
- Design tokens only; `notify()` for failures only; the StrictMode-safe listener pattern where listeners exist.
- Gates: Rust tasks `cargo check --workspace` + named tests; frontend tasks `npx tsc --noEmit` + `npm test`.
- Task classes (owner request): **A** infrastructure · **B** shared components · **C** tab restructuring · **D** panel migration · **E** search + reset · **F** docs + tests.

---

## File map

| File | Responsibility |
| ---- | ---- |
| `crates/athenaeum-core/src/settings/mod.rs` | `keys`/`defaults` grow the page's frontend-only keys; `defaults::all()` |
| `crates/athenaeum-core/src/api/settings.rs` (new) | `SettingsDefaults`, `get_settings_defaults` |
| `crates/athenaeum-tauri/src/commands/settings.rs`, `crates/athenaeum-web/src/routes/settings.rs`, `lib.rs`, `routes/mod.rs`, `ts_export.rs` | the command on both hosts |
| `src/settings/registry.ts` (new) | tabs, sections, fields — metadata only |
| `src/settings/SettingsDefaultsContext.tsx` (new) | loads `get_settings_defaults` once per mount |
| `src/settings/codecs.ts` (new) | `Codec<T>` helpers: `boolCodec`, `intCodec(min,max)`, `floatCodec(min,max)`, `stringCodec`, `enumCodec(values)` |
| `src/hooks/useSettingField.ts`, `src/hooks/useAutosaveDocument.ts`, `src/hooks/useSettingsSearch.ts` (new) | the three hooks |
| `src/components/settings/{Checkbox,SettingsSection,ResetButton,SettingToggle,SettingSelect,SettingNumber,SettingText,LevelSelect,SettingsSearch,SearchResults}.tsx` (new) | shared components |
| `src/components/settings/tabs/*Tab.tsx`, `src/components/settings/sections/*Section.tsx` (new) | the seven tabs and General/Blink sections |
| `src/pages/Settings.tsx` | tab bar + search + switch only |
| `src/components/folders/SwitchRow.tsx` | wraps `Checkbox` |
| existing panels (`AnalysisSettingsPanel`, `PlateSolveSettingsPanel`, `CalibrationMatchingConfig`, `LoggingSettings`, `StackingSection`, `AccountSection`, `SyncSection`, `TransfersSection`, stacking panels) | migrated in D |
| `vitest.config.ts`, `src/test/setup.ts`, `package.json` | the test runner |
| `docs/settings/README.md`, `CLAUDE.md`, `docs/superpowers/open-items.md`, `docs/backlog-v0.6.5.md` | F |

---

### Task A1 (class A): `get_settings_defaults` on both hosts

**Files:**
- Modify: `crates/athenaeum-core/src/settings/mod.rs` (`keys`, `defaults`)
- Create: `crates/athenaeum-core/src/api/settings.rs`; register in `crates/athenaeum-core/src/api/mod.rs`
- Modify: `crates/athenaeum-tauri/src/commands/settings.rs`, `crates/athenaeum-tauri/src/lib.rs` (`invoke_handler`), `crates/athenaeum-web/src/routes/settings.rs`, `crates/athenaeum-web/src/routes/mod.rs`, `crates/athenaeum-core/src/ts_export.rs`
- Test: `api/settings.rs` `mod tests`, `settings/mod.rs` tests

**Interfaces:**
- Produces: command `get_settings_defaults` → `SettingsDefaults { kv: BTreeMap<String,String>, analysis: AnalysisConfig, plate_solve: PlateSolveConfig, calibration_matching: CalibrationMatchingConfig, logging: LoggingConfig, stacking: StackingConfig }` (`#[serde(rename_all = "camelCase")]`, `ts_rs::TS`); `settings::defaults::all() -> &'static [(&'static str, &'static str)]`.

- [ ] **Step 1: Frontend-only keys become Rust keys.** `src/pages/Settings.tsx` reads these without a Rust constant: `blink.resolution`, `rustafits.quality.thumbnail`, `rustafits.quality.preview`, `rustafits.quality.full`, `flat_contour.resolution_pct` (`50`), `flat_contour.sigma_px` (`1.0`), `flat_contour.contours` (`15`), `flat_contour.gradient_pct` (`50`), `updates.auto_check` (`true`), `updates.check_beta` (`false`), `blink.annotation_config` (`""` = built-in), `ui.tree_view_mode`, `ui.blink_sidebar_px` (`320`). Read the current frontend defaults for the first four from `loadSettings` (`Settings.tsx` lines ~198-257, the `defaultValue:` of each `get_setting`) and add each pair to `keys` and `defaults` with a one-line doc comment. Then:

```rust
pub mod defaults {
    // … existing constants …
    /// Every key with a default, for `api::settings::get_settings_defaults`.
    /// Adding a key to `keys` without listing it here fails
    /// `every_key_with_a_default_is_listed`.
    pub fn all() -> &'static [(&'static str, &'static str)] {
        &[
            (super::keys::GROUPING_THRESHOLD_VALUE, GROUPING_THRESHOLD_VALUE),
            (super::keys::GROUPING_THRESHOLD_UNIT, GROUPING_THRESHOLD_UNIT),
            (super::keys::SESSION_GAP_THRESHOLD_HOURS, SESSION_GAP_THRESHOLD_HOURS),
            (super::keys::DUPLICATES_USE_CONTENT_HASH, DUPLICATES_USE_CONTENT_HASH),
            (super::keys::BLINK_THREADS, BLINK_THREADS),
            (super::keys::BLINK_MEMORY_CACHE_SIZE, BLINK_MEMORY_CACHE_SIZE),
            (super::keys::BLINK_MEMORY_CACHE_MAX_MB, BLINK_MEMORY_CACHE_MAX_MB),
            (super::keys::BLINK_MEMORY_RETENTION_MINUTES, BLINK_MEMORY_RETENTION_MINUTES),
            (super::keys::MONITORING_INTERVAL_MINUTES, MONITORING_INTERVAL_MINUTES),
            (super::keys::MONITORING_ENABLED_GLOBAL, MONITORING_ENABLED_GLOBAL),
            (super::keys::AUTO_MERGE_ON_BUTTON_CLICK, AUTO_MERGE_ON_BUTTON_CLICK),
            (super::keys::AUTO_MERGE_ON_MONITOR_DETECT, AUTO_MERGE_ON_MONITOR_DETECT),
            (super::keys::ARCHIVE_COMPRESSION, ARCHIVE_COMPRESSION),
            (super::keys::CALIBRATION_MASTER_FORMAT, CALIBRATION_MASTER_FORMAT),
            (super::keys::COMPUTE_MAX_CONCURRENT, COMPUTE_MAX_CONCURRENT),
            (super::keys::INTEGRATION_BAND_BUDGET_MB, INTEGRATION_BAND_BUDGET_MB),
            (super::keys::INTEGRATION_READ_CONCURRENCY, INTEGRATION_READ_CONCURRENCY),
            (super::keys::SYNC_MAX_UPLOAD_BYTES_PER_SEC, SYNC_MAX_UPLOAD_BYTES_PER_SEC),
            (super::keys::SYNC_MAX_CONCURRENT_RECEIVES, SYNC_MAX_CONCURRENT_RECEIVES),
            (super::keys::BLINK_RESOLUTION, BLINK_RESOLUTION),
            (super::keys::RUSTAFITS_QUALITY_THUMBNAIL, RUSTAFITS_QUALITY_THUMBNAIL),
            (super::keys::RUSTAFITS_QUALITY_PREVIEW, RUSTAFITS_QUALITY_PREVIEW),
            (super::keys::RUSTAFITS_QUALITY_FULL, RUSTAFITS_QUALITY_FULL),
            (super::keys::FLAT_CONTOUR_RESOLUTION_PCT, FLAT_CONTOUR_RESOLUTION_PCT),
            (super::keys::FLAT_CONTOUR_SIGMA_PX, FLAT_CONTOUR_SIGMA_PX),
            (super::keys::FLAT_CONTOUR_CONTOURS, FLAT_CONTOUR_CONTOURS),
            (super::keys::FLAT_CONTOUR_GRADIENT_PCT, FLAT_CONTOUR_GRADIENT_PCT),
            (super::keys::UPDATES_AUTO_CHECK, UPDATES_AUTO_CHECK),
            (super::keys::UPDATES_CHECK_BETA, UPDATES_CHECK_BETA),
            (super::keys::BLINK_ANNOTATION_CONFIG, BLINK_ANNOTATION_CONFIG),
            (super::keys::UI_TREE_VIEW_MODE, UI_TREE_VIEW_MODE),
            (super::keys::UI_BLINK_SIDEBAR_PX, UI_BLINK_SIDEBAR_PX),
        ]
    }
}
```

(Names that do not yet exist in `keys`/`defaults` are added in this step with the values above; keys that carry no default — `account.*`, `sync.cached_*`, `sync.*_dir`, `stacking.defaults`, `stacking.presets`, `archive.root_path`, `calibration.library_dir` — are deliberately absent.) Test in `settings/mod.rs`:

```rust
#[test]
fn every_key_with_a_default_is_listed() {
    let listed: std::collections::BTreeSet<&str> = defaults::all().iter().map(|(k, _)| *k).collect();
    for key in [keys::GROUPING_THRESHOLD_VALUE, keys::SESSION_GAP_THRESHOLD_HOURS, keys::BLINK_THREADS,
                keys::MONITORING_INTERVAL_MINUTES, keys::ARCHIVE_COMPRESSION, keys::CALIBRATION_MASTER_FORMAT,
                keys::COMPUTE_MAX_CONCURRENT, keys::INTEGRATION_BAND_BUDGET_MB, keys::SYNC_MAX_CONCURRENT_RECEIVES,
                keys::UPDATES_AUTO_CHECK, keys::FLAT_CONTOUR_CONTOURS, keys::BLINK_RESOLUTION] {
        assert!(listed.contains(key), "{key} has a default but is not in defaults::all()");
    }
    assert_eq!(listed.len(), defaults::all().len(), "duplicate key in defaults::all()");
}
```

- [ ] **Step 2: The command in core** — `crates/athenaeum-core/src/api/settings.rs`:

```rust
//! The Settings page's one source of defaults (spec 2026-09-18 §6): the same
//! constructors the `reset_*` commands write, plus every KV default.
use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use crate::analysis::config::AnalysisConfig;
use crate::calibration::config::CalibrationMatchingConfig;
use crate::logging::config::LoggingConfig;
use crate::plate_solve::config::PlateSolveConfig;
use crate::stacking::config::StackingConfig;
use crate::api::ApiError;
use crate::services::ServiceContext;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDefaults {
    pub kv: BTreeMap<String, String>,
    pub analysis: AnalysisConfig,
    pub plate_solve: PlateSolveConfig,
    pub calibration_matching: CalibrationMatchingConfig,
    pub logging: LoggingConfig,
    pub stacking: StackingConfig,
}

pub fn get_settings_defaults(_ctx: &ServiceContext) -> Result<SettingsDefaults, ApiError> {
    Ok(SettingsDefaults {
        kv: crate::settings::defaults::all().iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        analysis: AnalysisConfig::default(),
        plate_solve: PlateSolveConfig::default(),
        calibration_matching: CalibrationMatchingConfig::default(),
        logging: LoggingConfig::default(),
        stacking: StackingConfig::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_defaults_equal_what_reset_writes() {
        // `reset_*` each construct `T::default()` (api/analysis.rs:119,
        // api/calibration.rs:271, api/stacking.rs:656) — pin the equivalence
        // through serde so a future reset that seeds differently fails here.
        let (_tmp, ctx) = test_ctx();   // copy of api/frame_sets.rs tests' helper
        let d = get_settings_defaults(&ctx).unwrap();
        assert_eq!(serde_json::to_value(&d.analysis).unwrap(), serde_json::to_value(AnalysisConfig::default()).unwrap());
        assert_eq!(serde_json::to_value(&d.stacking).unwrap(), serde_json::to_value(StackingConfig::default()).unwrap());
        assert_eq!(d.kv.get("calibration.master_format").map(String::as_str), Some("fits"));
    }
}
```

(`test_ctx()` is `api/frame_sets.rs:450` — copy it. `PlateSolveConfig` is `crate::plate_solve::config::PlateSolveConfig`; `AnalysisConfig` is `crate::analysis::config::AnalysisConfig`. Check whether `plate_solve` is feature-gated in `lib.rs` — if it is, gate the `plate_solve` field and the whole command the same way the plate-solve commands are, and the TS type still exports.) Add `pub mod settings;` to `api/mod.rs`.

- [ ] **Step 3: Both hosts.** Tauri `commands/settings.rs`:

```rust
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_settings_defaults(state: State<'_, AppState>) -> Result<athenaeum_core::api::settings::SettingsDefaults, String> {
    athenaeum_core::api::settings::get_settings_defaults(&state.ctx).map_err(|e| e.to_string())
}
```

register in `lib.rs` `invoke_handler![]` beside `commands::get_setting`. Axum `routes/settings.rs`:

```rust
/// POST /api/get_settings_defaults
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_settings_defaults(State(state): State<WebAppState>, _body: Json<serde_json::Value>)
    -> Result<Json<athenaeum_core::api::settings::SettingsDefaults>, (StatusCode, String)> {
    athenaeum_core::api::settings::get_settings_defaults(&state.ctx).map(Json).map_err(api_err)
}
```

(`api_err` is what the sibling handlers in that file use — copy their exact mapper name.) Register `.route("/api/get_settings_defaults", post(settings::get_settings_defaults))` beside `/api/get_setting`. `ts_export.rs`: add `crate::api::settings::SettingsDefaults` to the registry list (the `models.ts` group), regenerate, confirm `src/types/models.ts` gains `SettingsDefaults` and that `LoggingConfig`/`PlateSolveConfig`/`CalibrationMatchingConfig` are already exported there or in their own files (if one is not, register it in the same list).

- [ ] **Step 4: Gates and commit**

Run: `cargo test -p athenaeum-core --lib -- settings:: api::settings` · `cargo check --workspace` · `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` · `npx tsc --noEmit`

```bash
git add crates/athenaeum-core/src/settings/mod.rs crates/athenaeum-core/src/api/settings.rs crates/athenaeum-core/src/api/mod.rs crates/athenaeum-core/src/ts_export.rs crates/athenaeum-tauri crates/athenaeum-web src/types
git commit -m "settings: get_settings_defaults — every KV default and every typed default from one command"
```

---

### Task A2 (class A): Vitest, the registry, the defaults context, the codecs

**Files:**
- Create: `vitest.config.ts`, `src/test/setup.ts`; modify `package.json` (devDependencies `vitest`, `@testing-library/react`, `@testing-library/jest-dom`, `jsdom`; script `"test": "vitest run"`)
- Create: `src/settings/registry.ts`, `src/settings/codecs.ts`, `src/settings/SettingsDefaultsContext.tsx`
- Test: `src/settings/registry.test.ts`, `src/settings/codecs.test.ts`

**Interfaces:**
- Produces: `SettingsTabId = 'general' | 'blink' | 'analysis' | 'plate_solving' | 'calibration' | 'stacking' | 'transfers'`; `SETTINGS_TABS`, `SETTINGS_SECTIONS`, `sectionById(id)`, `FIELDS` (typed access `FIELDS.general.updates.autoCheck` → `SettingsFieldMeta`); `Codec<T> = { parse(s: string): T | Error; format(v: T): string; validate?(v: T): string | null }`; `useSettingsDefaults(): { defaults: SettingsDefaults | null; error: string | null }`.

- [ ] **Step 1: Test runner.** `npm i -D vitest @testing-library/react @testing-library/jest-dom jsdom` (pin the versions npm resolves; Vite is `^8.1.5`, so Vitest 4.x). `vitest.config.ts`:

```ts
import { defineConfig } from 'vitest/config';
export default defineConfig({
  test: { environment: 'jsdom', setupFiles: ['src/test/setup.ts'], include: ['src/**/*.test.{ts,tsx}'] },
});
```

`src/test/setup.ts`: `import '@testing-library/jest-dom/vitest';`. A smoke test `src/test/smoke.test.ts` (`expect(1 + 1).toBe(2)`) proves `npm test` runs.

- [ ] **Step 2: The registry** — `src/settings/registry.ts`, metadata for EVERY section of spec §2. Write it in full; the shape:

```ts
import type { LucideIcon } from 'lucide-react';
import { Settings as SettingsIcon, Eye, BarChart3, ScanSearch, Crosshair, SquareStack, ArrowLeftRight } from 'lucide-react';

export type SettingsTabId = 'general' | 'blink' | 'analysis' | 'plate_solving' | 'calibration' | 'stacking' | 'transfers';

export interface SettingsFieldMeta { id: string; label: string; help?: string; keywords?: string[] }
export interface SettingsSectionMeta { id: string; tab: SettingsTabId; title: string; description?: string; fields: SettingsFieldMeta[] }

export const SETTINGS_TABS: { id: SettingsTabId; label: string; icon: LucideIcon }[] = [
  { id: 'general', label: 'General', icon: SettingsIcon },
  { id: 'blink', label: 'Blink', icon: Eye },
  { id: 'analysis', label: 'Analysis', icon: BarChart3 },
  { id: 'plate_solving', label: 'Plate Solving', icon: ScanSearch },
  { id: 'calibration', label: 'Calibration', icon: Crosshair },
  { id: 'stacking', label: 'Stacking', icon: SquareStack },
  { id: 'transfers', label: 'Transfers', icon: ArrowLeftRight },
];

export const SETTINGS_SECTIONS: SettingsSectionMeta[] = [
  { id: 'general.updates', tab: 'general', title: 'Updates', fields: [
    { id: 'autoCheck', label: 'Automatically check for updates on startup', keywords: ['updates.auto_check'] },
    { id: 'checkBeta', label: 'Check for beta updates', keywords: ['updates.check_beta'] },
  ]},
  { id: 'general.grouping', tab: 'general', title: 'Frame set grouping',
    description: 'How lights are grouped into frame sets by sky position.', fields: [
    { id: 'threshold', label: 'Grouping threshold', help: 'Lights whose centres lie within this distance of a set’s centre join that set. Seed-and-grow, single link, great-circle distance; only lights not yet in any set take part. Changing it does not regroup existing sets — Find new images and the monitor use the new value from now on.', keywords: ['grouping.threshold.value', 'cluster', 'radius'] },
    { id: 'thresholdUnit', label: 'Threshold unit', keywords: ['grouping.threshold.unit', 'deg', 'arcmin', 'arcsec'] },
  ]},
  { id: 'general.sessions', tab: 'general', title: 'Session detection', fields: [
    { id: 'gapHours', label: 'Session gap threshold (hours)', help: 'A gap longer than this between two lights starts a new night.', keywords: ['session_gap_threshold_hours', 'night'] },
  ]},
  // … monitoring, autoMerge, contentIndex, archive, dataLocations, logging;
  // blink.viewer, blink.flatContour, blink.annotations;
  // analysis.detection, analysis.measurement, analysis.psf, analysis.batch, analysis.rejection;
  // plateSolving.catalog, plateSolving.solver, plateSolving.inputGate;
  // calibration.matching, calibration.memory, calibration.masterFormat;
  // stacking.pipeline, stacking.folders;
  // transfers.account, transfers.sync, transfers.folders, transfers.upload, transfers.receiving, transfers.storage
];
```

Every field label must be the string the JSX shows today (copy them from `Settings.tsx` and the panels — the search index reads these). `FIELDS` is derived: `export const FIELDS = Object.fromEntries(SETTINGS_SECTIONS.map(s => [s.id, Object.fromEntries(s.fields.map(f => [f.id, f]))]))` typed as `Record<string, Record<string, SettingsFieldMeta>>` plus `export function fieldMeta(section: string, field: string): SettingsFieldMeta` that throws on a miss (a dev-time typo is a crash, never a silent unlabeled field). Test `registry.test.ts`: ids unique, every section's tab is in `SETTINGS_TABS`, every field id unique within its section, no empty labels.

- [ ] **Step 3: Codecs** — `src/settings/codecs.ts` with `boolCodec` (`'true'|'false'`), `intCodec(min, max)`, `floatCodec(min, max, step?)`, `stringCodec(maxLen?)`, `enumCodec<T extends string>(values)`. `parse` returns the value or an `Error` with the message the field shows (`"Must be a whole number between 1 and 32"`). Test each with one valid and one invalid input.

- [ ] **Step 4: Defaults context** — `src/settings/SettingsDefaultsContext.tsx`: a provider mounted by `Settings.tsx` that calls `api.invoke<SettingsDefaults>('get_settings_defaults')` once (cancelled-flag effect), exposes `{ defaults, error }`; `useSettingsDefaults()` throws outside the provider. On error: `console.error('[Settings] get_settings_defaults failed:', err)` and `error` set — the page renders without reset affordances (spec §9).

- [ ] **Step 5: Gates and commit**

Run: `npm test` (smoke + registry + codecs pass) · `npx tsc --noEmit`

```bash
git add package.json package-lock.json vitest.config.ts src/test src/settings
git commit -m "settings: the registry, the codecs, the defaults context, and a test runner"
```

---

### Task A3 (class A): `useSettingField`, `useAutosaveDocument`, `useSettingsSearch`

**Files:**
- Create: `src/hooks/useSettingField.ts`, `src/hooks/useAutosaveDocument.ts`, `src/hooks/useSettingsSearch.ts`
- Test: `src/hooks/useSettingField.test.tsx`, `src/hooks/useAutosaveDocument.test.tsx`, `src/hooks/useSettingsSearch.test.ts` (mock `api.invoke` with `vi.mock('../api', …)`)

**Interfaces:** exactly spec §5's signatures; `useSettingsSearch(query: string): { results: SettingsSectionMeta[]; matchedFieldIds: Set<string> }`.

- [ ] **Step 1: Failing tests for `useSettingField`** (`renderHook` + fake timers):
  - a `setValue(true)` on a `boolCodec` field invokes `set_setting` once with `{ key, value: 'true' }` after 300 ms; two `setValue`s inside the window write once, the last value;
  - `setDraft('12')` on an `intCodec(1, 32)` field writes nothing until `commit()`; `commit()` writes `'12'`; `setDraft('99')` then `commit()` writes nothing and `error` is the codec's message; `escape()` restores the draft to the committed value;
  - `reset()` writes the default from the context (`'true'`) through `set_setting` and `isDefault` becomes true;
  - a rejected `set_setting` sets `error`, keeps the draft, and calls `notify` once.

- [ ] **Step 2: Implement `useSettingField`** — reads the initial value with `get_setting { key, defaultValue: <default from context, or '' if the context has none> }` on mount (cancelled flag), holds `committed` and `draft`, the 300 ms debounce for `setValue`, `commit()` for the draft, `reset()` = `setValue(defaultValue)`, `savedAt` set on success, `error` on failure + `notify({ kind: 'generic', tone: 'warning', title: 'Setting not saved', detail: <label>: <error>, dedupeKey: key })`. `meta` = `fieldMeta(section, field)` — the hook signature is `useSettingField<T>(section: string, field: string, key: string, codec: Codec<T>)`.

- [ ] **Step 3: Failing tests for `useAutosaveDocument`**: a `load()` never triggers `save`; `patch` twice within 500 ms saves once with the last document; unmount with a pending patch saves; `save` rejection exposes `error` and keeps `doc`; `resetField('a.b')` patches the default's value at that path; `resetAll()` calls `opts.resetAll()` and reloads.

- [ ] **Step 4: Implement `useAutosaveDocument`** — port `StackingSection.tsx:113-204` verbatim in spirit (`dirtyRef`, `pendingRef`, debounce, unmount flush, `notifyOnFailure` only for the debounced path, never on the unmount flush), generalized over `opts.load/save/resetAll/defaults`; `isDefault(path)` and `resetField(path)` use a tiny `getPath`/`setPath` over dotted paths (no lodash).

- [ ] **Step 5: `useSettingsSearch`** — index built once with `useMemo` from `SETTINGS_SECTIONS`: for each section a normalized haystack (`title + description + tab label + every field label/help/keywords`, lower-cased, diacritics folded with `normalize('NFD').replace(/\p{M}/gu, '')`); query split on whitespace, every term must be a substring; results keep registry order; `matchedFieldIds` = `${section.id}/${field.id}` for fields whose own text matches any term. Tests: title match, help match, keyword match (`'session_gap'`), AND of two terms, empty query → no results.

- [ ] **Step 6: Gates and commit**

Run: `npm test` · `npx tsc --noEmit`

```bash
git add src/hooks/useSettingField.ts src/hooks/useAutosaveDocument.ts src/hooks/useSettingsSearch.ts src/hooks/*.test.ts*
git commit -m "settings: useSettingField, useAutosaveDocument and useSettingsSearch, with their tests"
```

---

### Task B1 (class B): the shared components

**Files:**
- Create: `src/components/settings/{Checkbox,SettingsSection,ResetButton,SettingToggle,SettingSelect,SettingNumber,SettingText,LevelSelect}.tsx`
- Modify: `src/components/folders/SwitchRow.tsx` (wraps `Checkbox`)
- Test: `src/components/settings/SettingNumber.test.tsx`, `Checkbox.test.tsx`

**Interfaces:**
- `Checkbox { checked, onChange(bool), label?: ReactNode, description?: string, disabled?, size?: 'sm'|'md', role?: 'checkbox'|'switch' }`
- `SettingsSection { id: string; children; onResetAll?: () => Promise<void>; actions?: ReactNode }` — reads `sectionById(id)` for title/description; renders `<section id={`settings-${id}`} data-settings-section={id}>`; the "Reset all" button (with `ConfirmDialog`) when `onResetAll` is given; registers itself in a module-level `Set` (`registerRenderedSection`/`unregisterRenderedSection`) that the registry test reads.
- `ResetButton { visible: boolean; defaultLabel: string; onReset(): void }` — `↺` (`RotateCcw` 14px), `title="Reset to default (…)"`, hidden when `!visible`.
- `SettingToggle { section, field, settingKey }` → `Checkbox` bound through `useSettingField(…, boolCodec)`, label/description from meta, `ResetButton` on the right.
- `SettingSelect<T> { section, field, settingKey, options: {value:T,label:string}[], codec }`, `SettingNumber { section, field, settingKey, codec, unit?, step?, placeholder? }` (draft/blur/Enter/Escape, inline error, `saved` tick), `SettingText` (same for strings).
- `LevelSelect { value: string; onChange(v): void; inherit?: { base: string } ; disabled? }` — the four levels plus the `Inherit (<base>)` option when `inherit` is given.

- [ ] **Step 1: `Checkbox`** — the house control:

```tsx
export function Checkbox({ checked, onChange, label, description, disabled, size = 'md', role = 'checkbox' }: CheckboxProps) {
  const box = size === 'sm' ? 'w-3.5 h-3.5' : 'w-4 h-4';
  return (
    <label className={`flex items-start gap-2 ${disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer'}`}>
      <input type="checkbox" role={role} checked={checked} disabled={disabled}
             onChange={(e) => onChange(e.target.checked)} className={`mt-0.5 shrink-0 ${box} accent-accent`} />
      {(label || description) && (
        <span className="flex-1 min-w-0">
          {label && <span className="block text-sm text-content-secondary">{label}</span>}
          {description && <span className="block text-xs text-content-muted leading-relaxed">{description}</span>}
        </span>
      )}
    </label>
  );
}
```

`SwitchRow` becomes `<Checkbox role="switch" label={<span className="font-medium text-content">{title}</span>} description={description} …/>` inside its existing hover wrapper — its props unchanged.

- [ ] **Step 2: The field components** follow one layout: label row (label, unit, `ResetButton`), control, help line, error line (`text-error text-xs`), a `saved` tick (`Check` 12px, `text-success`, fades via `transition-opacity`) shown while `Date.now() - savedAt < 1500`. `SettingNumber` keeps a string draft (the `NumericField.tsx` discipline from `src/components/stacking/`), commits on blur/Enter, Escape restores.

- [ ] **Step 3: Tests** — `Checkbox` renders `accent-accent` and toggles; `SettingNumber` with a mocked hook: typing does not call `commit`, blur does, Escape restores, an error renders under the field.

- [ ] **Step 4: Gates and commit**

Run: `npm test` · `npx tsc --noEmit`

```bash
git add src/components/settings src/components/folders/SwitchRow.tsx
git commit -m "settings: one Checkbox, SettingsSection, ResetButton and the four KV field components"
```

---

### Task C1 (class C): the page shell, seven tabs, General and Blink sections

**Files:**
- Modify: `src/pages/Settings.tsx` (reduce to shell)
- Create: `src/components/settings/tabs/{General,Blink,Analysis,PlateSolving,Calibration,Stacking,Transfers}Tab.tsx`; `src/components/settings/sections/{Updates,FrameSetGrouping,SessionDetection,Monitoring,AutoMerge,ContentIndex,Archive,DataLocations,BlinkViewer,FlatContour,StarAnnotation,MasterBuildMemory,MasterFileFormat}Section.tsx`
- Test: `src/components/settings/tabs/registry-coverage.test.tsx`

**Interfaces:**
- Consumes: everything from A2/A3/B1. `Settings.tsx` exports nothing new; tabs are `export function GeneralTab()` etc.

- [ ] **Step 1: Shell.** `Settings.tsx` keeps: `useSearchParams` for `?tab=` (the `validTabs` list becomes `SETTINGS_TABS.map(t => t.id)`), the new `?section=` (on mount and on change: `document.getElementById(`settings-${section}`)?.scrollIntoView({ block: 'start' })` + a `flash` class for 1.2 s), `SettingsDefaultsProvider`, `SettingsSearch` above the tab bar (Task E1 wires results; here it renders the input only), the tab bar built from `SETTINGS_TABS`, and `{activeTab === 'general' && <GeneralTab/>}` … Delete `loadSettings`, `handleSave`, the 21 `useState`s the Save bar fed, the success banner, `handleArchiveCompressionChange`, `loadIntegrationBudget`/`handleSaveIntegrationBudget`, the master-format state — each moves into its section (next steps).

- [ ] **Step 2: General sections**, one file each, all `SettingsSection` + field components, keys and codecs:

| Section id | Fields (component, key, codec) |
| ---- | ---- |
| `general.updates` | `SettingToggle updates.auto_check`, `SettingToggle updates.check_beta` |
| `general.grouping` | `SettingNumber grouping.threshold.value floatCodec(0.001, 180)`, `SettingSelect grouping.threshold.unit enumCodec(['deg','arcmin','arcsec'])` — plus the explanatory paragraph from the registry `description` (spec §2) |
| `general.sessions` | `SettingNumber session_gap_threshold_hours floatCodec(0.5, 48)` |
| `general.monitoring` | `SettingToggle monitoring.enabled_global`, `SettingNumber monitoring.interval_minutes intCodec(1, 1440)` |
| `general.autoMerge` | two `SettingToggle`s |
| `general.contentIndex` | `SettingToggle duplicates.use_content_hash` + the status text and the **Build index now** button (`useContentIndex`) as today |
| `general.archive` | `SettingSelect archive.compression enumCodec(['store','deflate'])` — through `useSettingField` (`set_archive_compression` lives in the host command layers, not in core — read `crates/athenaeum-tauri/src/commands/archive.rs`; if it only writes the KV key, the page writes the key directly, otherwise keep calling it through the hook's `write` override) |
| `general.dataLocations` | unchanged read-only block (database path, log dir, reveal buttons), desktop only |
| `general.logging` | `<LoggingSettings/>` (migrated in D2) |

`FrameSetGroupingSection` replaces BOTH today's "Clustering Parameters" block and the bottom "About Frame Set Grouping" card; the card's prose is folded into the registry `description` and one `<p>` under the fields.

- [ ] **Step 3: Blink sections** — `blink.viewer` (`SettingSelect blink.resolution`, the three JPEG-quality sliders as `SettingNumber` bound to `rustafits.quality.*` with `intCodec(10, 100)` — a range input whose `onChange` calls `setValue` (discrete rule), `SettingNumber blink.threads intCodec(0, max)` where `max` comes from `get_blink_threads_max` and the write goes through `set_blink_threads` (custom commit: the hook takes an optional `write?: (v: T) => Promise<void>` override — add it to `useSettingField`'s options in this task, with a test), `SettingNumber blink.memory_cache_size`, `blink.memory_cache_max_mb`, `blink.memory_retention_minutes`); `blink.flatContour` (four `SettingNumber`s); `blink.annotations` (the color scheme select, line width, the direction-tick `SettingToggle`, the seven numerics — all inside ONE JSON key `blink.annotation_config`: bind them through `useAutosaveDocument` with `load = get_setting + JSON.parse ?? BUILT_IN`, `save = set_setting(JSON.stringify)`, `defaults = BUILT_IN` (the one TS-held default, spec §6), 300 ms).

- [ ] **Step 4: Calibration tab** — `CalibrationMatchingConfig` (as is until D3), `MasterBuildMemorySection` (`SettingNumber integration.band_budget_mb intCodec(0, 16384)` with a custom `write` calling `set_integration_band_budget { mb }` and the "Applied: X MB / auto Y" readout from `get_integration_band_budget` refreshed after each write), `MasterFileFormatSection` (`SettingSelect calibration.master_format`). Analysis, Plate Solving, Stacking, Transfers tabs render their existing components (Account + Sync move into `TransfersTab` above `TransfersSection`).

- [ ] **Step 5: Registry coverage test** — render each tab with `api.invoke` mocked to resolve defaults, collect `registerRenderedSection` calls, assert the set equals `SETTINGS_SECTIONS.map(s => s.id)` exactly (until D-tasks wrap the panels, the panels' sections are registered by wrapping them in `SettingsSection` inside their tab files — e.g. `AnalysisTab` wraps `AnalysisSettingsPanel` in `<SettingsSection id="analysis.detection">`? No: a panel spans several sections. For this task register a panel's sections through a `<RegisteredSections ids={[…]}>` shim that calls `registerRenderedSection` for each id; D-tasks replace the shim with real `SettingsSection`s).

- [ ] **Step 6: Gates and commit**

Run: `npm test` · `npx tsc --noEmit` · a desktop or `npm run dev:web` look at every tab.

```bash
git add src/pages/Settings.tsx src/components/settings/tabs src/components/settings/sections src/hooks/useSettingField.ts
git commit -m "settings: seven tabs from the registry; General and Blink as registered, self-saving sections"
```

---

### Task D1 (class D): Analysis and Plate Solving panels on `useAutosaveDocument` + `Checkbox`

**Files:**
- Modify: `src/components/analysis/AnalysisSettingsPanel.tsx`, `src/components/plate-solve/PlateSolveSettingsPanel.tsx`, `src/components/calibration/RejectionThresholdBar.tsx` (the Trailed checkbox), `src/components/settings/tabs/{Analysis,PlateSolving}Tab.tsx`

- [ ] **Step 1: Analysis.** Replace the `config` state + `handleSave`/`handleReset`/`saved` timer with `useAutosaveDocument<AnalysisConfig>({ load: () => api.invoke('get_analysis_config'), save: (c) => api.invoke('set_analysis_config', { config: c }), resetAll: () => api.invoke('reset_analysis_config').then(c => api.invoke('set_analysis_config', { config: { ...c, batch_concurrency: 0 } })), defaults: defaults?.analysis ?? null })`; each numeric input becomes a draft/blur commit (`patch({ max_stars: n })` on blur/Enter, the codec's range as today's help says); the "Auto" checkbox → `Checkbox`; `analysis.rejection_defaults` and `analysis.fwhm_default_unit` → two `useSettingField`s (JSON string and enum). Split the panel into five `SettingsSection`s (`analysis.detection`, `analysis.measurement`, `analysis.psf`, `analysis.batch`, `analysis.rejection`) with `onResetAll` on the first calling the document's `resetAll` (one reset for the whole config — say so in its confirm text). Delete the Save/Reset buttons.
- [ ] **Step 2: Plate Solving.** Same treatment: `useAutosaveDocument<PlateSolveConfig>` over `get/set/reset_plate_solve_config`; sections `plateSolving.catalog` (download UI unchanged — actions), `plateSolving.solver`, `plateSolving.inputGate`; the "Refuse trailed frames" checkbox → `Checkbox`.
- [ ] **Step 3: Tests** — extend the registry coverage test (the shim for these two panels is removed); a render test that typing in Max Stars and blurring calls `set_analysis_config` once with the new value.
- [ ] **Step 4: Gates and commit** — `npm test` · `npx tsc --noEmit`

```bash
git add src/components/analysis src/components/plate-solve src/components/calibration/RejectionThresholdBar.tsx src/components/settings/tabs
git commit -m "settings: Analysis and Plate Solving save on change, no Save buttons"
```

---

### Task D2 (class D): Logging without duplication, Stacking on the shared hook

**Files:**
- Modify: `src/components/settings/LoggingSettings.tsx`, `src/components/settings/StackingSection.tsx`, `src/components/stacking/panels/*.tsx`, `src/components/stacking/StageRow.tsx`, `src/components/stacking/panels/OutputPanel.tsx` (radios keep native `radio`, only checkboxes change)

- [ ] **Step 1: Logging.** `useAutosaveDocument<LoggingConfig>({ load: () => api.invoke<LoggingConfigResponse>('get_logging_config').then(r => r.config), save: (c) => api.invoke('set_logging_config', { config: c }), defaults: defaults?.logging })`; keep `envOverrideActive` from the response for the one banner (the section `description` states the override rule; the banner appears only when active); `LevelSelect` for the base level and each of the five module rows (`inherit: { base: doc.level }`); `toModuleValue(modules, key) = modules[key] ?? INHERIT` / `fromModuleValue` deletes the key on `INHERIT` — the two functions, used by every row. Delete the Save button and both notify payloads (the hook notifies on failure).
- [ ] **Step 2: Stacking.** `StackingSection` replaces its `dirtyRef`/`pendingConfigRef`/two effects with `useAutosaveDocument<StackingConfig>({ load: get_stacking_defaults, save: set_stacking_defaults, resetAll: reset_stacking_defaults, defaults: defaults?.stacking })`; its Reset button becomes the `SettingsSection` "Reset all" (`stacking.pipeline`); `stacking.folders` wraps the two `FolderCard`s. Behaviour pinned by the hook tests stays identical (500 ms, load never writes, unmount flush).
- [ ] **Step 3: Checkbox sweep** — every `<input type="checkbox"` in `src/components/stacking/panels/*.tsx`, `StageRow.tsx` (`size="sm"`), `NormalizePanel`, `DrizzlePanel`, `CalibratePanel`, `MeasurePanel`, `ReferencePanel`, `RegisterPanel`, `IntegratePanel` → `<Checkbox>`; `grep -rn 'type="checkbox"' src/components/stacking src/components/settings src/pages/Settings.tsx` must return only `Checkbox.tsx`.
- [ ] **Step 4: Gates and commit** — `npm test` · `npx tsc --noEmit`

```bash
git add src/components/settings/LoggingSettings.tsx src/components/settings/StackingSection.tsx src/components/stacking
git commit -m "settings: Logging with one level picker and no Save; Stacking on the shared autosave hook; one Checkbox in every panel"
```

---

### Task D3 (class D): Calibration Matching, Account, Sync, Transfers

**Files:**
- Modify: `src/components/calibration/CalibrationMatchingConfig.tsx`, `src/components/calibration/ClusteringParametersPanel.tsx` (and the other matching sub-panels it renders), `src/components/settings/{AccountSection,SyncSection,TransfersSection}.tsx`

- [ ] **Step 1: Matching.** `useAutosaveDocument<CalibrationMatchingConfig>` over `get/set/reset_calibration_matching_config`; the six collapsible groups stay, wrapped as ONE `SettingsSection id="calibration.matching"` (`onResetAll` = the document's `resetAll`); sliders commit on change (discrete rule), the date-threshold and clustering numbers on blur/Enter; "Refresh All Calibration Sets" stays a button; delete Save/Reset.
- [ ] **Step 2: Account.** Device name and hub URL become blur/Enter commits (`SettingText`-style: draft, commit calls the existing rename / `set_setting account.hub_url` path); the field help says a device rename is visible to the other nodes; delete both Save buttons. Hub selector clicks already persist.
- [ ] **Step 3: Transfers.** Upload limit and simultaneous receives: `SettingNumber` with custom `write` (`set_sync_upload_limit`, `set_sync_max_concurrent_receives`) — delete the two Save buttons; folder cards, cleanup buttons, the restart badge unchanged. Sections `transfers.account`, `transfers.sync`, `transfers.folders`, `transfers.upload`, `transfers.receiving`, `transfers.storage`.
- [ ] **Step 4: Gates and commit** — `npm test` (coverage test now has no shim left) · `npx tsc --noEmit` · `grep -rn '>Save' src/components/settings src/pages/Settings.tsx src/components/analysis src/components/plate-solve src/components/calibration` returns nothing.

```bash
git add src/components/calibration src/components/settings
git commit -m "settings: Calibration Matching, Account, Sync and Transfers save on change; the last Save buttons go"
```

---

### Task E1 (class E): search results and per-field reset wired through

**Files:**
- Create: `src/components/settings/SettingsSearch.tsx`, `src/components/settings/SearchResults.tsx`
- Modify: `src/pages/Settings.tsx`, `src/components/settings/SettingsSection.tsx` (highlight prop), the field components (highlight)

- [ ] **Step 1: `SettingsSearch`** — input with `Search` icon, clear `X`, `placeholder="Search settings…"`, `/` focuses it when no input is focused, Escape clears; controlled by `Settings.tsx` state.
- [ ] **Step 2: `SearchResults`** — for a non-empty query, `useSettingsSearch(query).results` rendered in registry order; each result is the section's REAL component (a `sectionComponent(id)` map in `tabs/index.ts` returns the element for a section id — tabs are lists of `{ id, element }` so the map is derived, not duplicated), wrapped with a tab chip (`SETTINGS_TABS` label) in the header; `matchedFieldIds` passed down via a `SearchHighlightContext` so `SettingsSection`/field components add `ring-1 ring-accent/60` to matched fields. "No settings match" empty state. The tab bar renders but is disabled while a query is active.
- [ ] **Step 3: Reset all on tabs** is NOT added (spec §6 keeps section-level). Verify every KV section passes `onResetAll` (writes each field's default through `useSettingField.reset`) and every typed section passes the document's `resetAll`.
- [ ] **Step 4: Tests** — `SearchResults` with query `'gap'` renders exactly the Session detection section; `'blink jpeg'` renders the viewer section with the quality fields highlighted.
- [ ] **Step 5: Gates and commit** — `npm test` · `npx tsc --noEmit`

```bash
git add src/components/settings/SettingsSearch.tsx src/components/settings/SearchResults.tsx src/components/settings/tabs/index.ts src/pages/Settings.tsx src/components/settings/SettingsSection.tsx
git commit -m "settings: search across every tab, results edited in place; reset all per section"
```

---

### Task F1 (class F): README, CLAUDE.md, the owed smokes

**Files:**
- Create: `docs/settings/README.md` (spec §12, all six rules, with one worked example of each kind: a KV toggle and a typed-config field)
- Modify: `CLAUDE.md` (a "Settings page" paragraph: registry, the two hooks, `get_settings_defaults`, "no Save buttons", the README), `docs/superpowers/open-items.md` (desktop click-through of the seven tabs, search, a field reset, a section reset, the restart badge, the Logging card), `docs/backlog-v0.6.5.md` (item 4 → SHIPPED with the commit range)

- [ ] **Step 1: README** per spec §12; include the annotation-config exception and the `?tab=…&section=…` deep-link form.
- [ ] **Step 2: CLAUDE.md** paragraph (≤ 12 lines) under "Frontend Conventions".
- [ ] **Step 3: Ledger + backlog.**
- [ ] **Step 4: Commit**

```bash
git add docs/settings/README.md CLAUDE.md docs/superpowers/open-items.md docs/backlog-v0.6.5.md
git commit -m "docs: how a setting is added — registry, hooks, defaults, no Save buttons"
```

---

## Self-review

- **Spec coverage.** §2 tabs → C1 (+ D-tasks for the panels' sections); §3 registry → A2; §4 components → B1; §5 autosave → A3 + D1–D3; §6 defaults/reset → A1 + A2 + E1; §7 search → A3 + E1; §8 logging → D2; §9 errors → A3 (hook contract) + A2 (defaults failure); §10 tests → every task; §11 compatibility → C1 (`?tab=`), B1 (`SwitchRow`); §12 docs → F1.
- **Types.** `useSettingField(section, field, key, codec, { write? })` is the shape B1, C1 and D-tasks use; `useAutosaveDocument({ load, save, resetAll?, defaults, debounceMs? })` the shape D1–D3 use; `SettingsDefaults` field names (`plateSolve`, `calibrationMatching`) match the Rust `camelCase` rename.
- **Order.** A1 → A2 → A3 → B1 → C1 → D1 → D2 → D3 → E1 → F1; D1–D3 are independent of each other once C1 lands and can run in parallel (different files).
