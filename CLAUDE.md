# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Athenaeum is a desktop application for astrophotographers to manage FITS/XISF image files. It builds a searchable metadata catalog, detects duplicates using xxHash, and provides Lightroom-style file management with specialized astronomy features.

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

- **Routing**: React Router v7 with 5 main views (File Manager, Shoot Calendar, Objects, Equipment, Export)
- **State Management**: React hooks + Tauri commands for backend communication
- **Styling**: Tailwind CSS with dark theme
- **Component Structure**:
  - `src/components/Layout.tsx` - Main app shell with sidebar navigation
  - `src/pages/*` - One page component per mode
  - `src/hooks/*` - Custom hooks for Tauri command invocation
  - `src/types/*` - TypeScript interfaces matching Rust models

### Backend (Rust)

- **Module Organization**:
  - `models.rs` - Serde-compatible data structures (File, Frame, CalibrationSet, etc.)
  - `db/` - SQLite operations and schema
  - `fits_parser/` - FITS/XISF metadata extraction
  - `scanner/` - Multi-threaded directory traversal with walkdir + rayon
  - `duplicates/` - xxHash XXH3_64 computation and duplicate grouping
  - `calibration/` - Calibration frame matching algorithms
  - `export/` - Path template resolution and file copying
  - `commands.rs` - Tauri commands exposed to frontend

- **Tauri Commands**: Functions marked with `#[tauri::command]` in `commands.rs` are callable from React via `invoke()`

### Database Schema

See `src-tauri/src/db/schema.rs` for full schema. Key tables:
- `files` - Physical files with hash and duplicate_group_id
- `frames` - Metadata extracted from FITS/XISF (OBJECT, DATE-OBS, TELESCOP, INSTRUME, etc.)
- `scan_roots` - Monitored directory paths
- `calibration_sets` - Grouped calibration frames by type and parameters
- `tags` + `frame_tags` - User tagging system
- `export_templates` - Saved export path templates

Indexes on: content_hash, size, date_obs, object, telescop, instrume, imagetyp

### Key Technical Decisions

**xxHash over MD5/SHA**: Non-cryptographic xxHash XXH3_64 chosen for maximum scan throughput on large datasets. Optional byte-verify available before destructive operations.

**FITS Parsing**: Uses `fitsio` crate. Key FITS keywords: OBJECT, DATE-OBS, TIME-OBS, TELESCOP, INSTRUME, EXPTIME, FILTER, IMAGETYP, GAIN, OFFSET, XBINNING, YBINNING, CCD-TEMP, SET-TEMP.

**XISF Parsing**: Must parse XML header according to XISF 1.0 spec and extract embedded FITS-like properties.

**DATE-OBS Normalization**: Parse various formats (ISO 8601, separate DATE-OBS + TIME-OBS, legacy) into consistent timestamp for calendar grouping.

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
   - Handle migration if schema already exists (or delete dev database)

3. **Adding New Models**:
   - Define in `src-tauri/src/models.rs` with Serde derive
   - Create matching TypeScript interface in `src/types/`

4. **UI Changes**:
   - Pages are in `src/pages/`
   - Shared components in `src/components/`
   - Use Tailwind classes, dark theme (bg-gray-800/900, text-gray-100)
   - Icons from `lucide-react`

## Testing Approach

- **Backend**: Use `cargo test` with mock file systems and in-memory SQLite
- **FITS Parser**: Test with real FITS files from common observatories
- **xxHash**: Verify hash consistency and collision detection
- **Export Templates**: Test token resolution with edge cases (missing values, special chars)

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

**Querying Database in Rust**:
```rust
let conn = get_connection()?;
let mut stmt = conn.prepare("SELECT * FROM frames WHERE object = ?1")?;
let frames = stmt.query_map([object], |row| {
  // map row to Frame struct
})?;
```

**Computing File Hash**:
```rust
use crate::duplicates::compute_xxhash;
let hash = compute_xxhash(&path)?;
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
