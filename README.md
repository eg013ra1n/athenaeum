# Athenaeum

A desktop application for astrophotographers to manage, organize, and export FITS/XISF image files.

Website and documentation: [artfrom.space](https://artfrom.space)

[![ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/N4N81UR2EE)

## Features

### File Manager

Multi-root directory scanning with automatic FITS/XISF metadata extraction. Browse files with inline metadata display, track missing files, and search across your entire image library. Scanning is multi-threaded using rayon for fast indexing of large collections. A two-pane file browser handles moves (cross-volume moves are hash-verified), renames, bulk metadata edits and catalog search, keeping the catalog in sync with every operation.

### Shoot Calendar

Month and year views of imaging sessions grouped by imaging night (noon to noon, so a session that crosses midnight stays one night). See at a glance which nights you captured data, with equipment and target breakdowns per session.

### Objects Library

Automatic frame set grouping by sky coordinates: frames with nearby RA/Dec values are clustered into sets representing the same target. Supports merging and splitting sets, manual assignment, and configurable clustering thresholds.

### Sky Chart

Interactive all-sky map powered by d3-celestial. Visualize where your frame sets are located on the celestial sphere, with rectangle selection for spatial queries.

### Equipment Library

Track camera and telescope usage across your captures. Includes a dark library view per camera showing available calibration coverage.

### Calibration Library

Fully configurable calibration frame matching with 8 matchable parameters (instrument, binning, gain, offset, exposure time, focal length, filter, CCD temperature), 3 match modes (Exact, Warning, Ignore), and fallback chains (e.g., DarkFlat -> Dark -> Bias for flat calibration). All matching rules are editable in the UI.

### Master Calibration Library

Build master darks, flats, bias and dark-flats in-app from a matched raw calibration set -- no external stacker required. Masters register into the catalog exactly as a scanned file would, every consumer relinks onto them automatically, and the originals can be archived in the same step.

### Stacking

Stack a frame set's lights into its own master light entirely in-app: calibration from the linked masters, hot-pixel correction and VNG debayering, PSF-based frame weighting, quad-seeded registration with polynomial or thin-plate-spline distortion (plate-solve seeded across pixel scales, so a bin-2 group or a second telescope stacks with the rest), local normalization, banded weighted integration with a choice of rejection algorithms, drizzle (including Bayer drizzle for OSC) and FITS or XISF output with WCS. Every per-frame stage is cached on disk, so a re-run only recomputes what changed.

### Export

Four export modes -- lights only, raw frames with their calibration sets, raw frames with masters, or **calibrated lights** generated on the fly as 32-bit float FITS that WBPP or Siril consume with their own calibration step disabled -- laid out in the folder hierarchy and keyword layout PixInsight's WeightedBatchPreprocessing expects, with symlinks instead of copies where the platform supports them. Includes calibration chain visualization showing which calibration frames will accompany each export.

### Transfers

Peer-to-peer transfers between your own devices over [iroh](https://iroh.computer), with per-file progress, resume, deduplication against the receiver's catalog and configurable bandwidth. A frame set can be sent in any export mode straight from the Export tab. **Perseus**, the bundled capture-agent CLI, watches an observatory machine's folders and ships new frames on a schedule.

### Archive

Move a finished frame set into one ZIP per frame type in an archive folder of your choice, keeping all catalog metadata. Restore is reconcile-based: it fills what is missing on disk and never overwrites.

### Plate Solving

Blind and hinted solving against a tiered Gaia-derived star catalog, with the resulting WCS written back into the frame's metadata and used by the Sky Chart.

### Blink Viewer

Built-in image viewer for FITS/XISF files with dual caching modes (in-memory and disk). Supports Bayer demosaicing for OSC cameras and multi-resolution JPEG output for responsive loading.

### Duplicate Detection

xxHash XXH3_64-based file hashing for fast duplicate identification across large libraries. Detected duplicates can be soft-deleted to a "Black Hole" for review before permanent removal.

## Technology Stack

- **Frontend**: React 19, TypeScript, Vite, Tailwind CSS, TanStack Table, d3-celestial
- **Desktop Framework**: Tauri 2.0
- **Backend**: Rust
- **Database**: SQLite (rusqlite)
- **FITS/XISF Rendering**: [rustafits](https://github.com/eg013ra1n/rustafits) -- a pure Rust FITS/XISF image rendering library. Handles Bayer demosaicing, debayering, auto-stretch, and multi-resolution JPEG output with zero C dependencies for image processing.
- **FITS/XISF Metadata**: Custom pure-Rust parser — FITS headers are read block-by-block per the FITS standard; XISF headers are parsed from embedded XML using quick-xml
- **File Scanning**: walkdir + rayon (multi-threaded directory traversal)
- **Hashing**: xxhash-rust (XXH3_64)

## Downloads

Pre-built releases are available at [artfrom.space/releases/download](https://artfrom.space/releases/download):

- **Windows**: MSI installer, EXE (NSIS)
- **macOS**: DMG (universal binary -- Apple Silicon + Intel)
- **Linux**: AppImage, DEB
- **Docker**: `docker pull eg013ra1n/athenaeum` (web build for NAS and home servers)

The desktop app checks for updates itself and installs them in place (signed with minisign).

## Building from Source

### Clone

```bash
git clone --recursive https://github.com/eg013ra1n/athenaeum.git
```

`--recursive` is required: [rustafits](https://github.com/eg013ra1n/rustafits)
(image rendering) and
[solvemyastro](https://github.com/eg013ra1n/solvemyastro) (plate solving) are
submodules *and* Cargo workspace members. Without them the workspace does not
build. Already cloned flat? Run `git submodule update --init --recursive`.

### Prerequisites

- Node.js 22.12 or newer, and npm
- Rust -- the toolchain is pinned in `rust-toolchain.toml`, so rustup installs
  the right version automatically
- Platform-specific Tauri prerequisites: <https://tauri.app/start/prerequisites/>

On Debian or Ubuntu that means:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev \
  build-essential curl wget file
```

### Desktop

```bash
npm install
npm run tauri dev      # hot-reload development build
npm run tauri build    # packaged application
```

### Web and Docker

The same core, served over HTTP with SSE instead of Tauri IPC:

```bash
npm run dev:web              # frontend, VITE_TARGET=web
cargo run -p athenaeum-web   # backend, in a second terminal
```

A multi-stage `docker/Dockerfile` builds the containerized version. The catalog
database lives in the OS application-data directory on desktop, and in `/data`
(or `$ATHENAEUM_DB_PATH`) under Docker.

### Checks

```bash
cargo build --workspace
cargo test -p athenaeum-core
npx tsc --noEmit
```

These three are what CI runs. See [CONTRIBUTING.md](./CONTRIBUTING.md) before
opening a pull request.

## Project Structure

```text
athenaeum/
├── crates/
│   ├── athenaeum-core/           # Shared library — all non-IPC logic
│   │   └── src/
│   │       ├── db/               # SQLite schema and operations
│   │       ├── fits_parser/      # FITS/XISF metadata extraction
│   │       ├── scanner/          # Multi-threaded directory traversal
│   │       ├── clustering/       # Sky-coordinate frame-set grouping
│   │       ├── calibration/      # Calibration matching engine and config
│   │       ├── calibration_library/  # Master creation and light calibration
│   │       ├── integration/      # Banded frame integration and combiners
│   │       ├── stacking/         # Light stacking pipeline (measure, register, normalize, drizzle)
│   │       ├── geometry/         # Pixel maps, distortion models, thin-plate splines
│   │       ├── plate_solve/      # Plate-solving adapter
│   │       ├── archive/          # ZIP archive lifecycle
│   │       ├── file_op/          # Move pipeline with cross-volume verify
│   │       ├── export/           # WBPP folder/keyword export
│   │       ├── sync/             # Device-to-device transfers
│   │       ├── sharing/          # iroh transport and wire protocol
│   │       ├── collab/           # Collaboration projects (hub client)
│   │       ├── updates/          # In-app update check
│   │       └── services/         # ServiceContext, queues, ProgressEmitter
│   ├── athenaeum-tauri/          # Desktop shell — commands/ wraps core
│   ├── athenaeum-web/            # Axum HTTP/SSE server — routes/ mirrors commands
│   ├── perseus/                  # Capture-agent CLI for observatory machines
│   ├── catalog-builder/          # Star-catalog build tool
│   └── log-mcp/                  # Log query server for development
├── rustafits/                    # Submodule — FITS/XISF image rendering
├── solvemyastro/                 # Submodule — plate solver
├── src/                          # React frontend
│   ├── api/                      # The only place Tauri IPC or HTTP is touched
│   ├── components/               # UI components
│   ├── pages/                    # One per view
│   ├── hooks/                    # Custom React hooks
│   └── types/                    # TypeScript mirrors of the Rust models
└── docs/                         # Per-subsystem references, design specs, plans, research
```

## Documentation

- **Users**: [artfrom.space](https://artfrom.space) -- guides, release posts, downloads.
- **Contributors**: [`CLAUDE.md`](./CLAUDE.md) is the map of the codebase -- the
  rules, the workspace layout and a one-paragraph summary per subsystem with a
  link to its reference. The references live under `docs/`
  (`docs/stacking/README.md`, `docs/transfers/README.md`,
  `docs/export/README.md`, `docs/masters/README.md`, `docs/catalog/README.md`,
  …) and hold the details, the design rulings and the acceptance history.
  Design specs, plans and research notes are in `docs/superpowers/`.
  [`CONTRIBUTING.md`](./CONTRIBUTING.md) covers the pull-request process.

## Architecture

Athenaeum runs on two backends over one shared library. `athenaeum-core` holds
everything that is not transport: the SQLite catalog, FITS/XISF parsing,
frame-set clustering, calibration matching, master creation, stacking,
archiving, export and peer-to-peer transfers. The desktop shell is **Tauri 2**,
exposing 259 commands across 24 domain modules over Tauri's IPC; the web build is an **Axum**
server whose routes mirror those commands one-for-one and stream progress over
SSE. The React frontend reaches whichever is active through a single `api`
object, so no component knows which host it is running under.

Image rendering is [rustafits](https://github.com/eg013ra1n/rustafits), a pure
Rust FITS/XISF library -- Bayer demosaicing, auto-stretch and multi-resolution
JPEG output with no C dependencies. Plate solving is
[solvemyastro](https://github.com/eg013ra1n/solvemyastro), using quad matching
against a Gaia-derived star catalog.

## Roadmap

- Code-signing certificate for Windows (macOS builds are signed and notarized)
- Automated panoramic acquisitions grouping
- Online collaboration: shared projects with multi-source distribution (hub in private beta)
- Planning and acquisition software integrations

## License

[Apache-2.0](./LICENSE) -- Copyright 2024-2026 Vilen Sharifov
