# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

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
  - `calibration/` - Calibration frame matching algorithms
  - `clustering/` - Sky coordinate-based frame set clustering with DBSCAN
  - `coordinates/` - Astronomical coordinate conversions (RA/Dec string parsing, decimal degrees)
  - `settings/` - Settings management with runtime and database persistence
  - `export/` - Path template resolution and file copying
  - `commands.rs` - Tauri commands exposed to frontend

- **Tauri Commands**: Functions marked with `#[tauri::command]` in `commands.rs` are callable from React via `invoke()`

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
- `projects` - Top-level organization for imaging projects
- `frames_set` - Top-level frame sets grouped by sky coordinates
- `imaging_nights` - Imaging nights/sessions within a frame set (linked via `frames_set_id`)
- `sessions` - Groups frames by instrument within an imaging night
- `session_members` - Junction table linking frames to sessions (the actual many-to-many between frames and sessions)
- `tags` + `frame_tags` - User tagging system
- `export_templates` - Saved export path templates
- `fits_header` - Complete original FITS header storage
- `settings` - Application configuration (key-value pairs)

Indexes on: filename, date_obs, object, instrume, ra, dec, objctra, objctdec, exptime, filter

### Key Technical Decisions

**Frame Set Clustering**: Uses DBSCAN algorithm to group LIGHT frames by sky coordinates (RA/Dec). Frames within a configurable threshold distance are automatically grouped into frame sets. This enables organizing frames by target object without manual tagging.

**Coordinate Parsing**: Supports multiple RA/Dec formats:
- Decimal degrees (e.g., `123.456`, `-45.678`)
- HMS/DMS strings (e.g., `12h34m56.7s`, `-45d40m30s`)
- Colon-separated (e.g., `12:34:56.7`, `-45:40:30`)

**Settings System**: Three-tier precedence for configuration:
1. Runtime overrides (in-memory)
2. Database persisted settings
3. Default values

Common settings include `grouping_threshold_arcmin` for frame set clustering.

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

## Development Workflow

1. **Adding New Tauri Commands**:
   - Define function in `src-tauri/src/commands.rs` with `#[tauri::command]`
   - Add to `invoke_handler` in `src-tauri/src/lib.rs`
   - Call from React with `invoke('command_name', { args })`

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

## Testing Approach

- **Backend**: Use `cargo test` with mock file systems and in-memory SQLite
- **FITS Parser**: Test with real FITS files from common observatories
- **Coordinate Parsing**: Test HMS/DMS and decimal degree conversions with edge cases
- **Frame Set Clustering**: Test DBSCAN algorithm with various coordinate distributions
- **Settings System**: Test precedence (runtime > DB > default) and persistence
- **Export Templates**: Test token resolution with edge cases (missing values, special chars)
- **Duplicate Detection**: Verify hash consistency (when implemented)

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
  projectId: 1
});
console.log(`Created ${result.sets_created} sets with ${result.frames_clustered} frames`);
```

**Working with Settings**:
```typescript
// Get a setting with default
const threshold = await invoke<string>('get_setting', {
  key: 'grouping_threshold_arcmin',
  defaultValue: '15'
});

// Set a setting
await invoke('set_setting', {
  key: 'grouping_threshold_arcmin',
  value: '20'
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

## Dependencies

**Frontend**: React, React Router, Tailwind, TanStack Table, Lucide Icons, date-fns

**Backend**: Tauri, rusqlite, fitsio, xxhash-rust, chrono, rayon, walkdir, serde, anyhow, thiserror

## Reference Documentation

- [Tauri 2.0 Docs](https://tauri.app/start/)
- [FITS Standard](https://heasarc.gsfc.nasa.gov/docs/fcg/standard_dict.html)
- [XISF 1.0 Specification](https://pixinsight.com/doc/docs/XISF-1.0-spec/XISF-1.0-spec.html)
- [xxHash](https://xxhash.com/)
- Technical Specification: `TS.md` in repository root
