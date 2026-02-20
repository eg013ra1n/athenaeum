# Athenaeum

Astrophotography File Manager - A desktop application for managing FITS/XISF astronomical image files with metadata cataloging, duplicate detection, and intelligent export capabilities.

## Features

- **File Manager**: Scan directories for FITS/XISF files and build a searchable metadata catalog
- **Shoot Calendar**: Browse captures by date with equipment and target information
- **Objects Library**: Organize captures by astronomical objects with drill-down views
- **Equipment Library**: Track telescope and camera usage across captures
- **Calibration Library**: Link calibration frames (Bias, Dark, Flat, DarkFlat) to capture sessions
- **Export Tool**: Export files with customizable path templates using metadata tokens
- **Duplicate Detection**: Fast xxHash-based duplicate detection for large file sets

## Technology Stack

- **Frontend**: React 19 + TypeScript + Vite + Tailwind CSS
- **Desktop Framework**: Tauri 2.0
- **Backend**: Rust
- **Database**: SQLite
- **Key Libraries**:
  - `fitsio` - FITS file parsing
  - `xxhash-rust` - Fast non-cryptographic hashing
  - `rusqlite` - SQLite database
  - `walkdir` + `rayon` - Multi-threaded file scanning

## Prerequisites

- Node.js 18+ and npm
- Rust 1.70+
- Platform-specific Tauri prerequisites: https://tauri.app/start/prerequisites/

## Development

```bash
# Install dependencies
npm install

# Run in development mode
npm run tauri dev

# Build for production
npm run tauri build
```

## Project Structure

```
athenaeum/
├── src/                    # React frontend
│   ├── components/        # Reusable UI components
│   ├── pages/            # Page components for each mode
│   ├── hooks/            # Custom React hooks
│   ├── lib/              # Utility functions
│   └── types/            # TypeScript type definitions
├── src-tauri/             # Rust backend
│   └── src/
│       ├── db/           # Database operations
│       ├── fits_parser/  # FITS/XISF parsing
│       ├── scanner/      # File scanning
│       ├── duplicates/   # Duplicate detection
│       ├── calibration/  # Calibration management
│       ├── export/       # Export with templating
│       ├── models.rs     # Data models
│       └── commands/     # Tauri commands (frontend API)
```

## Architecture

Athenaeum uses a **Tauri 2.0** stack: a Rust backend handles file scanning, FITS/XISF parsing, SQLite storage, and calibration matching, while a React/TypeScript frontend provides the UI. Commands are organized into focused modules by domain (scanning, calibration, export, etc.) and called from the frontend via Tauri's `invoke()` IPC.

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

## License

[Apache-2.0](./LICENSE)
