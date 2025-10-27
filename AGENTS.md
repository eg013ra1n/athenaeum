# Repository Overview

## Project Description
- **What it does**: Athenaeum is a desktop application for astrophotographers that scans directories of FITS/XISF image files, extracts astronomical metadata, stores it in a local SQLite catalog, and provides rich UI tools for browsing, searching, de‑duplicating, managing calibration frames, and exporting files with customizable path templates.
- **Purpose & goals**: Enable fast, reliable management of large astrophotography datasets without relying on cloud services; give users searchable metadata, duplicate detection via xxHash, calendar view of shoots, and a flexible export workflow.
- **Key technologies**:
  - Front‑end: React 19 + TypeScript + Vite + Tailwind CSS (dark theme)
  - Desktop shell: Tauri 2.0 (Rust backend + webview)
  - Backend: Rust – `rusqlite`, `fitsio`, `xxhash-rust`, `rayon`, `walkdir`
  - Database: SQLite stored in the user’s app‑data folder

## Architecture Overview
- **High‑level**: A thin React UI communicates with a Rust backend via Tauri commands. The backend performs all heavy work (file scanning, FITS/XISF parsing, hashing, DB CRUD) and returns JSON data.
- **Main components**:
  - `src/components/` – reusable UI pieces (DirectoryTree, Layout, etc.)
  - `src/pages/` – five primary screens (File Manager, Shoot Calendar, Objects, Equipment, Export)
  - `src/hooks/` – custom hooks that wrap Tauri `invoke()` calls.
  - `src-tauri/src/models.rs` – shared data structures, serde‑compatible with the front‑end types in `src/types/models.ts`.
  - `src-tauri/src/db/` – SQLite schema (`schema.rs`) and CRUD helpers.
  - `src-tauri/src/fits_parser/` – reads FITS/XISF headers (OBJECT, DATE-OBS, TELESCOP, etc.).
  - `src-tauri/src/scanner/` – multi‑threaded directory walk, computes xxHash (`duplicates::compute_xxhash`).
  - `src-tauri/src/duplicates/` – groups files by hash, optional byte verification.
  - `src-tauri/src/calibration/` – matches calibration frames to lights based on exposure, temperature, binning, filter, etc.
  - `src-tauri/src/export/` – resolves user‑defined token templates (`{OBJECT}`, `{DATE-OBS:%Y-%m-%d}` …) and copies/moves files.
  - `src-tauri/src/commands.rs` – all Tauri commands exposed to the UI (scan, search, export, etc.).
- **Data flow**:
  1. UI triggers a command (`invoke('scan_folders', {roots})`).
  2. Rust scanner walks the roots, parses FITS/XISF metadata, computes hash, writes rows to SQLite.
  3. UI fetches frames/files via commands like `search_frames` and renders them.
  4. User actions (duplicate removal, export) call further commands that act on DB records and filesystem.

## Directory Structure
```
athenaeum/
├─ .continue/                 # Continue‑CLI config & custom rules
├─ src/                       # React front‑end
│   ├─ components/            # UI widgets (DirectoryTree, Layout …)
│   ├─ pages/                # Page components for each app mode
│   ├─ hooks/                # Tauri command wrappers
│   ├─ types/                # TypeScript interfaces mirroring Rust models
│   └─ lib/                  # Misc utilities (if any)
├─ src-tauri/                 # Rust back‑end
│   ├─ src/
│   │   ├─ db/               # SQLite schema & ops (`schema.rs`)
│   │   ├─ fits_parser/      # FITS/XISF header extraction
│   │   ├─ scanner/          # Directory traversal, hashing
│   │   ├─ duplicates/       # xxHash duplicate grouping
│   │   ├─ calibration/      # Calibration frame handling
│   │   ├─ export/           # Export templating logic
│   │   ├─ models.rs         # Shared data structs (serde)
│   │   └─ commands.rs       # Tauri‑exposed functions
│   └─ tauri.conf.json       # Tauri configuration (window, bundling)
├─ public/                    # Static assets (icons etc.)
├─ README.md                  # High‑level project description
├─ CLAUDE.md                  # Detailed architecture notes for Claude AI
├─ TS.md                      # Technical specification
└─ package.json / vite.config.ts / tailwind.config.js  # Front‑end tooling
```
- **Entry points**: `src/main.tsx` (React entry) and `src-tauri/src/main.rs` (Tauri bootstrap). Commands are registered in `src-tauri/src/lib.rs`.

## Development Workflow
- **Build / run**:
  ```bash
  npm install                # install Node deps
  npm run tauri dev          # start React + Tauri dev server with hot‑reload
  ```
- **Production build**: `npm run tauri build`.
- **Testing**:
  - Backend: `cd src-tauri && cargo test`
  - Frontend (once tests are added): `npm test`
- **Environment setup**:
  - Node 18+, Rust 1.70+ installed.
  - Follow Tauri prerequisites for macOS (Xcode, Cocoa libs, etc.).
- **Lint / format**:
  - Front‑end: `npx eslint . --fix` (if ESLint is added) and `npm run lint` can be defined later.
  - Rust: `cargo fmt && cargo clippy -- -D warnings`.

---
*Generated with [Continue](https://continue.dev)
Co-Authored-By: Continue <noreply@continue.dev>*