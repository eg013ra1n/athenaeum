# Athenaeum

A desktop application for astrophotographers to manage, organize, and export FITS/XISF image files.

Website and documentation: [artfrom.space](https://artfrom.space)

## Features

### File Manager

Multi-root directory scanning with automatic FITS/XISF metadata extraction. Browse files with inline metadata display, track missing files, and search across your entire image library. Scanning is multi-threaded using rayon for fast indexing of large collections.

### Shoot Calendar

Month and year views of imaging sessions grouped by DATE-OBS. See at a glance which nights you captured data, with equipment and target breakdowns per session.

### Objects Library

Automatic frame set grouping by sky coordinates using DBSCAN clustering. Frames with nearby RA/Dec values are grouped into sets representing the same target. Supports merging and splitting sets, manual assignment, and configurable clustering thresholds.

### Sky Chart

Interactive all-sky map powered by d3-celestial. Visualize where your frame sets are located on the celestial sphere, with rectangle selection for spatial queries.

### Equipment Library

Track camera and telescope usage across your captures. Includes a dark library view per camera showing available calibration coverage.

### Calibration Library

Fully configurable calibration frame matching with 8 matchable parameters (instrument, binning, gain, offset, exposure time, focal length, filter, CCD temperature), 3 match modes (Exact, Warning, Ignore), and fallback chains (e.g., DarkFlat -> Dark -> Bias for flat calibration). All matching rules are editable in the UI.

### Export

Organize files into PixInsight WBPP-compatible folder structures using metadata token templates. Tokens like `{OBJECT}`, `{DATE-OBS:%Y-%m-%d}`, `{FILTER}`, `{FRAME_FOLDER}` resolve from FITS headers, with fallback values and slug transforms. Includes calibration chain visualization showing which calibration frames will accompany each export.

### Blink Viewer

Built-in image viewer for FITS/XISF files with dual caching modes (in-memory and disk). Supports Bayer demosaicing for OSC cameras and multi-resolution JPEG output for responsive loading.

### Duplicate Detection

xxHash XXH3_64-based file hashing for fast duplicate identification across large libraries. Detected duplicates can be soft-deleted to a "Black Hole" for review before permanent removal.

## Technology Stack

- **Frontend**: React 19, TypeScript, Vite, Tailwind CSS, TanStack Table, d3-celestial
- **Desktop Framework**: Tauri 2.0
- **Backend**: Rust
- **Database**: SQLite (rusqlite)
- **FITS/XISF Rendering**: [rustafits](https://github.com/eg013ra1n/rustafits) -- a pure Rust FITS/XISF image rendering library by the same author. Handles Bayer demosaicing, debayering, auto-stretch, and multi-resolution JPEG output with zero C dependencies for image processing.
- **FITS Metadata**: fitsio (cfitsio bindings for header extraction)
- **File Scanning**: walkdir + rayon (multi-threaded directory traversal)
- **Hashing**: xxhash-rust (XXH3_64)

## Downloads

Pre-built releases are available at [artfrom.space/releases/download](https://artfrom.space/releases/download):

- **Windows**: MSI installer, EXE (NSIS)
- **macOS**: DMG (universal binary -- Apple Silicon + Intel)
- **Linux**: AppImage, DEB

## Building from Source

### Prerequisites

- Node.js 18+ and npm
- Rust 1.70+
- Platform-specific Tauri prerequisites: <https://tauri.app/start/prerequisites/>

### Build

```bash
# Install dependencies
npm install

# Run in development mode
npm run tauri dev

# Build for production
npm run tauri build
```

## Project Structure

```text
athenaeum/
├── src/                          # React frontend
│   ├── components/               # UI components
│   ├── pages/                    # Page components (one per view)
│   ├── hooks/                    # Custom React hooks
│   ├── lib/                      # Utility functions
│   └── types/                    # TypeScript type definitions
├── src-tauri/                    # Rust backend
│   └── src/
│       ├── commands/             # Tauri commands organized by domain
│       ├── db/                   # SQLite schema and operations
│       ├── fits_parser/          # FITS/XISF metadata extraction
│       ├── scanner/              # Multi-threaded directory traversal
│       ├── calibration/          # Calibration matching engine and config
│       ├── clustering/           # DBSCAN sky coordinate clustering
│       ├── coordinates/          # RA/Dec parsing and conversion
│       ├── duplicates/           # Hash computation and duplicate detection
│       ├── export/               # Path template resolution and file copy
│       ├── sessions/             # Imaging session grouping
│       ├── settings/             # Settings management with DB persistence
│       ├── rustafits_processor/  # Image rendering pipeline
│       ├── cache/                # Image cache management
│       ├── fingerprint/          # File fingerprinting
│       ├── relinking/            # Missing file relinking
│       └── models.rs             # Serde-compatible data structures
```

## Architecture

Athenaeum uses a **Tauri 2.0** architecture with a Rust backend and React/TypeScript frontend communicating over Tauri's IPC (`invoke()`). The backend is organized into 68+ commands across focused domain modules (scanning, calibration, export, frame sets, spatial queries, etc.). All metadata is stored in a local SQLite database. Image rendering is handled by rustafits, a pure Rust library that processes FITS/XISF pixel data into viewable JPEGs with Bayer demosaicing and auto-stretch.

## Roadmap

- Code-signing certificates for Windows and macOS
- Automated panoramic acquisitions grouping
- Dockerized version for NAS deployment (Synology, Unraid, TrueNAS)
- Internal stacking with modern algorithms
- Online collaboration with download client
- Planning and acquisition software integrations

## License

[Apache-2.0](./LICENSE) -- Copyright 2024-2026 Vilen Sharifov
