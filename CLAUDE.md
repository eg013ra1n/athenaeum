# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## General rules
When implementing plans, start with the minimal viable approach. Do NOT over-engineer or try to build/stub entire dependency trees (e.g., don't build all of Siril when only a few C files are needed). Ask for clarification if scope is unclear.
Never swallow errors silently. When implementing error handling in Rust commands or TypeScript handlers, always log errors to console/stderr before returning. Silent error swallowing has repeatedly caused hours of debugging.

### Testing and Debugging

When debugging, test with real data files (e.g., real FITS files) early rather than spending extended time on synthetic tests. If synthetic tests pass but real-world behavior differs, switch to real data immediately.
When there are any 

### Communication section

When the user describes a concept (e.g., 'calibration set ID', 'filter field'), ask for clarification if there's any ambiguity rather than assuming a technical interpretation. Do not substitute domain terms (e.g., 'equipment ID' ≠ 'calibration set ID', 'filter' ≠ 'sort').

### Editing Conventions

For multi-file edit sessions, consolidate changes into complete passes rather than making many incremental small edits. When editing large files, be especially careful to preserve file integrity—avoid partial writes or truncation.

## Project Overview

Athenaeum is a desktop application for astrophotographers to manage FITS/XISF image files. It builds a searchable metadata catalog with specialized astronomy features including:
- Automated frame set grouping by sky coordinates
- Directory browsing with file metadata display
- Calibration frame management
- Export templates with path resolution
- User-configurable settings system

The application uses Tauri 2.0 (Rust backend + React frontend) with SQLite for local catalog storage.

## Commands

### Development
```bash
npm run tauri dev          # Start development server with hot reload
npm run build             # Build frontend only
npm run tauri build       # Build complete desktop application
```

### Testing
```bash
# Rust backend tests
cd src-tauri && cargo test

# Frontend tests (when added)
npm test
```

### Database
The SQLite database is created in the user's app data directory by Tauri. Schema initialization happens in `src-tauri/src/db/schema.rs`.

## Architecture

This project uses a Tauri stack: Rust backend + React/TypeScript frontend. Be aware of serialization boundaries—Rust uses snake_case, TypeScript uses camelCase. Always verify serde attributes and IPC serialization when wiring backend to frontend.

### Frontend (React + TypeScript)

- **Routing**: React Router v7 with 6 main views:
  - `FileManager` - Directory browser with file metadata display
  - `Objects` - Frame set library grouped by sky coordinates
  - `ShootCalendar` - Calendar view of imaging sessions
  - `Equipment` - Equipment and setup management
  - `Export` - Export templates and file organization
  - `Settings` - Application settings and configuration
- **State Management**: React hooks + Tauri commands for backend communication
- **Styling**: Tailwind CSS with dark theme (bg-gray-800/900, text-gray-100)
- **Component Structure**:
  - `src/components/Layout.tsx` - Main app shell with sidebar navigation
  - `src/components/DirectoryTree.tsx` - Directory browser with file listing and metadata
  - `src/pages/*` - One page component per view
  - `src/hooks/*` - Custom hooks for Tauri command invocation (when added)
  - `src/types/models.ts` - TypeScript interfaces matching Rust models

### Backend (Rust)

- **Module Organization**:
  - `models.rs` - Serde-compatible data structures (File, Frame, FramesSet, etc.)
  - `db/` - SQLite operations (`schema.rs`, `operations.rs`)
  - `fits_parser/` - FITS/XISF metadata extraction
  - `scanner/` - Multi-threaded directory traversal with walkdir + rayon
  - `duplicates/` - xxHash XXH3_64 computation and duplicate detection
  - `calibration/` - Calibration frame matching algorithms (see Calibration Matching System below)
  - `clustering/` - Sky coordinate-based frame set clustering (seed-and-grow single-link)
  - `coordinates/` - Astronomical coordinate conversions (RA/Dec string parsing, decimal degrees)
  - `settings/` - Settings management with runtime and database persistence
  - `export/` - Path template resolution and file copying
  - `commands/` - **Modular Tauri commands** organized by domain (see below)
  - `commands_rustafits.rs` - Specialized FITS image rendering commands

- **Commands Module Structure** (Refactored 2025-11-17):
  The Tauri commands are organized into focused modules by domain. All 68 commands are in `src-tauri/src/commands/`:
  - `mod.rs` - Module exports, AppState definition, and re-exports
  - `core.rs` - 2 commands: App initialization (greet, initialize_database)
  - `scan_roots.rs` - 9 commands: Directory scanning and monitoring
  - `files.rs` - 7 commands: File operations and browsing
  - `settings.rs` - 5 commands: Application configuration
  - `frame_sets.rs` - 14 commands: Frame set management and operations
  - `calibration.rs` - 17 commands: Calibration frame matching and library management
  - `duplicates.rs` - 8 commands: Black hole & duplicate detection
  - `cache.rs` - 2 commands: Cache management
  - `spatial.rs` - 4 commands: Sky coordinate queries and spatial operations
  - `utils.rs` - Shared helper functions (calculate_fov, angular_distance, format_bytes)

  See `src-tauri/REFACTORING.md` for complete command migration map and details.

- **Tauri Commands**: Functions marked with `#[tauri::command]` in `commands/` modules are callable from React via `invoke()`. All commands are re-exported through `commands/mod.rs` for backward compatibility.

### Database Schema

See `src-tauri/src/db/schema.rs` for full schema. Key tables:

- `files` - Physical files (path, filename, size, format, modified_at)
- `frames` - Metadata extracted from FITS/XISF with astronomical coordinates
  - Basic: OBJECT, DATE-OBS, TELESCOP, INSTRUME, EXPTIME, FILTER, IMAGETYP
  - Camera: GAIN, OFFSET, XBINNING, YBINNING, CCD-TEMP, SET-TEMP
  - Optics: FOCALLEN, XPIXSZ, PIXSZ
  - Coordinates: RA, DEC, OBJCTRA, OBJCTDEC, SITELAT, SITELONG
- `scan_roots` - Monitored directory paths
- `calibration_set` + `calibration_set_frames` - Grouped calibration frames
- `projects` - Exists but currently not used (vestigial table; frame sets are global and not scoped to projects)
- `frames_set` - Top-level frame sets grouped by sky coordinates (NO project_id column - frame sets are global)
- `imaging_nights` - Imaging nights/sessions within a frame set (linked via `frames_set_id`)
- `sessions` - Groups frames by instrument within an imaging night
- `session_members` - Junction table linking frames to sessions (the actual many-to-many between frames and sessions)
- `tags` + `frame_tags` - User tagging system
- `export_templates` - Saved export path templates
- `fits_header` - Complete original FITS header storage
- `settings` - Application configuration (key-value pairs)

Indexes on: filename, date_obs, object, instrume, ra, dec, objctra, objctdec, exptime, filter

### Key Technical Decisions

**Frame Set Clustering**: Uses a seed-and-grow single-link clustering algorithm to group LIGHT frames by sky coordinates (RA/Dec). For each unassigned frame (in deterministic RA→Dec→DATE-OBS order), a new cluster is seeded; the cluster then iteratively absorbs any unassigned frame within `threshold_deg` of the cluster's current center. After each member is added, the center is recomputed as the spherical mean of all members so far — so the cluster center *moves* during growth. This is intentional: it lets dithered or mosaicked fields collapse into a single frame set, even when the dither/mosaic span exceeds the threshold. The trade-off is that long chains of frames each within `threshold_deg` of the running mean can also merge, even if the chain endpoints are farther apart than the threshold. Distance is great-circle (spherical law of cosines), so RA wraparound and high-declination compression are handled correctly. The algorithm is **not** DBSCAN — there is no min_pts, no core/border/noise distinction, and every frame ends up in some cluster (singletons allowed).

**Coordinate Parsing**: Supports multiple RA/Dec formats:

- Decimal degrees (e.g., `123.456`, `-45.678`)
- HMS/DMS strings (e.g., `12h34m56.7s`, `-45d40m30s`)
- Colon-separated (e.g., `12:34:56.7`, `-45:40:30`)

**Settings System**: Three-tier precedence for configuration:

1. Runtime overrides (in-memory)
2. Database persisted settings
3. Default values

Common settings include `grouping.threshold.value` (default `3.0`) and `grouping.threshold.unit` (default `deg`, also accepts `arcmin` and `arcsec`) for frame set clustering. Internally consumed via `SettingsManager::get_grouping_threshold_deg`.

**Auto-Generate Frame Sets**:

- Excludes frames already in any set to prevent duplicates
- Clusters by sky coordinates with configurable threshold
- Reports excluded frames with reasons (missing coordinates, etc.)
- Creates named sets with aggregated metadata (total exposure time, coordinates)

**FITS Parsing**: Uses `fitsio` crate. Key FITS keywords: OBJECT, DATE-OBS, TIME-OBS, TELESCOP, INSTRUME, EXPTIME, FILTER, IMAGETYP, GAIN, OFFSET, XBINNING, YBINNING, CCD-TEMP, SET-TEMP, FOCALLEN, RA, DEC, OBJCTRA, OBJCTDEC.

**XISF Parsing**: Must parse XML header according to XISF 1.0 spec and extract embedded FITS-like properties.

**DATE-OBS Normalization**: Parse various formats (ISO 8601, separate DATE-OBS + TIME-OBS, legacy) into consistent timestamp for calendar grouping.

**Duplicate Detection**: xxHash XXH3_64 computation available for identifying duplicate files (implementation in `duplicates/` module).

**IMAGETYP to FRAME_FOLDER Mapping**:

- LIGHT → `Lights`
- DARK → `Calibration/Darks`
- FLAT → `Calibration/Flats`
- BIAS → `Calibration/Bias`
- DARKFLAT → `Calibration/DarkFlats`

**Export Path Templating**: Supports tokens like `{OBJECT}`, `{DATE-OBS:%Y-%m-%d}`, `{TELESCOP}`, `{INSTRUME}`, `{EXPTIME}`, `{FILTER}`, `{IMAGETYP}`, `{FRAME_FOLDER}`, with fallbacks (`{OBJECT|Unknown}`) and transforms (`:slug` for slugification).

### Calibration Matching System

The calibration matching system is fully configurable via UI (Settings → Calibration Matching tab). All calibration-related settings are stored in a unified `CalibrationMatchingConfig` JSON structure in the `settings` table under key `calibration.matching_config`.

**Configuration Components**:
- **Parameter Matching Rules**: Configure which parameters must match exactly, warn on threshold, or be ignored
- **Clustering Settings**: Max age and time clustering thresholds per calibration type (flat, dark, bias, darkflat)
- **Scoring Config**: Temperature match weight for calibration candidate scoring
- **Warning Thresholds**: Temperature delta tolerance and date warning thresholds
- **Master Preferences**: Prefer master frames or frame sets when both available

**Source Types** (frames that need calibration):
- **Lights** → can link to Flat, Dark, Bias
- **Flats** → can link to DarkFlat, Dark, Bias (with fallback chain: DarkFlat → Dark → Bias)
- **Darks** → can link to Bias (when "BIAS for Dark Optimization" is enabled)

**Configurable Parameters** (8 parameters per source→calibration pair):
- `instrume` - Camera/instrument name
- `binning` - Binning mode (e.g., "1x1", "2x2")
- `gain` - Sensor gain value
- `offset` - Sensor offset value
- `exptime` - Exposure time
- `focallen` - Focal length
- `filter` - Filter name (only matched for Lights→Flat)
- `ccd_temp` - CCD temperature

**Match Modes**:
- `Exact` - Must match exactly (with small tolerance for floats)
- `Warning` - Match but warn if threshold exceeded (e.g., temperature delta > 2°C)
- `Ignore` - Don't check this parameter

**Key Files**:
- `src-tauri/src/calibration/config.rs` - Configuration data structures (`CalibrationMatchingConfig`, `ParameterConfig`, `MatchMode`, etc.)
- `src-tauri/src/calibration/configurable_matcher.rs` - Config-driven matching engine (`find_calibration_sets`, `load_config`, etc.)
- `src-tauri/src/calibration/hierarchy.rs` - Calibration hierarchy builder (uses configurable matcher)
- `src/types/calibration-config.ts` - TypeScript interfaces
- `src/components/calibration/` - UI components (`CalibrationMatchingConfig.tsx`, `MatchingMatrixTable.tsx`, etc.)

**Tauri Commands**:
- `get_calibration_matching_config` - Load config (returns default if not set)
- `set_calibration_matching_config` - Save config to database
- `reset_calibration_matching_config` - Reset to defaults

**Default Behavior**: The default configuration matches the original hardcoded behavior:
- Lights→Flat: Exact match on instrume, binning, gain, offset, focallen, filter
- Lights→Dark: Exact match on instrume, binning, gain, offset, exptime; Warning on ccd_temp (2°C threshold)
- Lights→Bias: Exact match on instrume, binning, gain, offset; Warning on ccd_temp
- Flats→DarkFlat/Dark: Same as Lights→Dark (no filter matching)
- Flats→Bias: Same as Lights→Bias

## Development Workflow

1. **Adding New Tauri Commands**:
   - Determine the appropriate module in `src-tauri/src/commands/` based on functionality:
     - `core.rs` - App initialization and basic operations
     - `scan_roots.rs` - Scanning directories for FITS files
     - `files.rs` - File browsing, searching, previewing
     - `settings.rs` - Application configuration
     - `frame_sets.rs` - Grouping frames into sets
     - `calibration.rs` - Calibration frame matching and library
     - `duplicates.rs` - Duplicate detection and black hole management
     - `cache.rs` - Image cache operations
     - `spatial.rs` - Sky coordinate queries and spatial operations
   - Add command function to the appropriate module with `#[tauri::command]`
   - Ensure it's exported in `commands/mod.rs` (use `pub use module_name::*;`)
   - Add to `invoke_handler` in `src-tauri/src/lib.rs` as `commands::command_name`
   - Call from React with `invoke('command_name', { args })`

   **Example**:

   ```rust
   // In src-tauri/src/commands/settings.rs
   #[tauri::command]
   pub async fn get_my_setting(state: State<'_, AppState>) -> Result<String, String> {
       // implementation
   }

   // Already re-exported in commands/mod.rs via: pub use settings::*;

   // In src-tauri/src/lib.rs, add to invoke_handler:
   commands::get_my_setting,
   ```

2. **Database Schema Changes**:
   - Update `src-tauri/src/db/schema.rs::init_db()`
   - Handle migration if schema already exists (or delete dev database during development)
   - Add corresponding operations in `src-tauri/src/db/operations.rs`

3. **Adding New Models**:
   - Define in `src-tauri/src/models.rs` with Serde derive
   - Create matching TypeScript interface in `src/types/models.ts`
   - Ensure field names and types match exactly

4. **UI Changes**:
   - Pages are in `src/pages/`
   - Shared components in `src/components/`
   - Use Tailwind classes, dark theme (bg-gray-800/900, text-gray-100)
   - Icons from `lucide-react`
   - Follow React hooks best practices (useCallback, useMemo, proper dependencies)

5. **Working with Settings**:
   - Settings are managed through `SettingsManager` in `src-tauri/src/settings/`
   - Use `get_setting` and `set_setting` Tauri commands from frontend
   - Settings support default values and database persistence
   - Access via `state.settings` in Rust commands

6. **Working with Frame Sets**:
   - Frame sets are created via `auto_generate_frame_sets` command
   - Clustering is performed by `src-tauri/src/clustering/` module
   - Coordinate conversion handled by `src-tauri/src/coordinates/` module
   - Always check if frames are already in sets before adding to prevent duplicates

7. **Working with Calibration Matching Config**:
   - Configuration is managed through `src-tauri/src/calibration/config.rs`
   - Use `get_calibration_matching_config` and `set_calibration_matching_config` Tauri commands
   - Load config in Rust with `configurable_matcher::load_config(conn)`
   - UI components are in `src/components/calibration/`
   - TypeScript interfaces in `src/types/calibration-config.ts`

## Testing Approach

- **Backend**: Use `cargo test` with mock file systems and in-memory SQLite
- **FITS Parser**: Test with real FITS files from common observatories
- **Coordinate Parsing**: Test HMS/DMS and decimal degree conversions with edge cases
- **Frame Set Clustering**: Test seed-and-grow algorithm with various coordinate distributions (including RA wraparound and zero-leg fallback to sexagesimal)
- **Settings System**: Test precedence (runtime > DB > default) and persistence
- **Export Templates**: Test token resolution with edge cases (missing values, special chars)
- **Duplicate Detection**: Verify hash consistency (when implemented)
- **Calibration Matching**: Test config loading, parameter matching modes, fallback chains

## File Organization Conventions

- Keep Rust modules focused (one responsibility per module)
- Frontend pages should be mostly presentational, delegate logic to hooks
- Use `anyhow::Result` for Rust error handling, convert to strings at Tauri command boundary
- Prefix all custom hooks with `use` (e.g., `useScanRoots`)

## Common Patterns

**Invoking Tauri Commands from React**:

```typescript
import { invoke } from '@tauri-apps/api/core';

const result = await invoke<ReturnType>('command_name', {
  arg1: value1,
  arg2: value2
});
```

**Auto-Generating Frame Sets**:

```typescript
// From frontend
const result = await invoke<AutoGenerateResult>('auto_generate_frame_sets', {
  projectId: 1  // NOTE: projectId parameter is kept for backwards compatibility but is currently ignored
});
console.log(`Created ${result.sets_created} sets with ${result.frames_clustered} frames`);
```

**Note on Projects**: The `projects` table exists in the database but is not currently linked to frame sets. The `project_id` parameter in commands like `get_frames_sets` and `auto_generate_frame_sets` is accepted for backwards compatibility but is ignored in the implementation. Frame sets are currently global and not scoped to any project.

**Working with Settings**:

```typescript
// Get a setting with default
const threshold = await invoke<string>('get_setting', {
  key: 'grouping.threshold.value',
  defaultValue: '3.0'
});

// Set a setting
await invoke('set_setting', {
  key: 'grouping.threshold.value',
  value: '15.0'
});
// Unit is configured separately:
await invoke('set_setting', {
  key: 'grouping.threshold.unit',
  value: 'arcmin'
});
```

**Querying Database in Rust**:

```rust
let conn = db.conn();
let mut stmt = conn.prepare("SELECT * FROM frames WHERE object = ?1")?;
let frames = stmt.query_map([object], |row| {
  // map row to Frame struct
})?;
```

**Accessing Settings in Rust Commands**:

```rust
#[tauri::command]
pub async fn my_command(state: State<'_, AppState>) -> Result<f64, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let threshold = state.settings
        .get_grouping_threshold_deg(&conn)
        .map_err(|e| e.to_string())?;

    Ok(threshold)
}
```

**Parsing Coordinates**:
```rust
use crate::coordinates::{parse_ra_to_degrees, parse_dec_to_degrees};

let ra_deg = parse_ra_to_degrees("12h34m56.7s")?;
let dec_deg = parse_dec_to_degrees("-45d40m30s")?;
```

**Working with Calibration Matching Config**:

```typescript
// From frontend - get current config
const config = await invoke<CalibrationMatchingConfig>('get_calibration_matching_config');

// Modify parameter matching rules
config.lights.dark.ccd_temp.warning_threshold = 3.0;

// Modify clustering settings
config.clustering.flat.max_age_days = 60;
config.clustering.flat.time_cluster_minutes = 45;

// Modify scoring config
config.scoring.temperature_match_weight = 0.5;

// Modify warning thresholds
config.warnings.temp_delta_celsius = 3.0;
config.warnings.flat_date_warning_days = 60;

// Save changes
await invoke('set_calibration_matching_config', { config });

// Reset to defaults
const defaultConfig = await invoke<CalibrationMatchingConfig>('reset_calibration_matching_config');
```

```rust
// In Rust - load and use config for matching
use crate::calibration::configurable_matcher::{load_config, find_calibration_sets};

let config = load_config(conn);

// Use parameter matching rules
let candidates = find_calibration_sets(conn, &frame, "lights", "dark", &config)?;

// Access clustering settings
let max_age = config.clustering.get("flat")
    .map(|c| c.max_age_days)
    .unwrap_or(30);

// Access warning thresholds
let temp_delta = config.warnings.temp_delta_celsius;

// Access scoring config
let temp_weight = config.scoring.temperature_match_weight;
```

## Dependencies

**Frontend**: React, React Router, Tailwind, TanStack Table, Lucide Icons, date-fns

**Backend**: Tauri, rusqlite, fitsio, xxhash-rust, chrono, rayon, walkdir, serde, anyhow, thiserror

## Reference Documentation

- [Tauri 2.0 Docs](https://tauri.app/start/)
- [FITS Standard](https://heasarc.gsfc.nasa.gov/docs/fcg/standard_dict.html)
- [XISF 1.0 Specification](https://pixinsight.com/doc/docs/XISF-1.0-spec/XISF-1.0-spec.html)
- [xxHash](https://xxhash.com/)
- Commands Refactoring: `src-tauri/REFACTORING.md` - Complete documentation of the 2025-11-17 modular refactoring