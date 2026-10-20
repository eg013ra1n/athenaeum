# Frontend conventions


## Detail moved from CLAUDE.md (2026-10-20) - Frontend conventions and Settings page

### Frontend Conventions

- Backend access via the `api` object in `src/api/` only. Desktop-specific bits in `src/api/desktop.ts`.
- Tailwind + design tokens (above). Icons from `lucide-react`. Charts from `recharts`.
- Custom hooks prefixed `use…`; pages mostly presentational, logic in hooks.
- TS interfaces in `src/types/models.ts` mirror Rust models; `src/types/calibration-config.ts` mirrors the calibration config.
- Shared compact UI primitives live in `src/components/ui/` (they mirror the collab mockup's CSS: `Button`, `Chip`, `Card`, `KV`, `DialogShell`, `SidePanel`, …); a new or reworked modal uses `DialogShell` (the collab dialogs and `ConfirmDialog`/`AlertDialog` do; the rest move in wave 5.6). `npm run ui:harness` renders the project page on the mockup's data for side-by-side checks.

**Settings page** (`docs/settings/README.md` is the contract — read it before adding a setting).
Seven tabs in a fixed order — General · Blink · Analysis · Plate Solving · Calibration · Stacking ·
Transfers — each a list of registered sections. `src/settings/registry.ts` is the ONE description
of the page (tab → section → field metadata: ids, labels, help, keywords; never values or render
code); it drives search (`useSettingsSearch`, `?tab=…&section=…` deep links) and per-field /
per-section reset. A KV key renders through `useSettingField` + `SettingToggle`/`SettingSelect`/
`SettingNumber`/`SettingText` inside a `SettingsSection`; a typed config (Analysis, Plate Solving,
Calibration Matching, Logging, Stacking) patches through `useAutosaveDocument`. Discrete controls
commit on change (300 ms debounce), text/number on blur/Enter, Escape restores, an invalid draft
never writes. Defaults come from `get_settings_defaults` (both hosts, `api::settings`) built from
`settings::defaults::all()` + each config's `Default`; the annotation config is the one TS-held
exception. **Never a Save button, never a banner** — `notify()` on failure only; `Checkbox` is the one checkbox.

