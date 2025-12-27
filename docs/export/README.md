# Export Module Documentation

## Overview

The Export module enables users to export frame sets with their linked calibration frames for processing in external stacking software. The primary integration is with **Siril**, a free and open-source astronomical image processing application.

## Current State

**Status: Phase 1 Complete (Backend + UI) - Needs Testing**

The export module has been implemented with the following capabilities:
- Frame set selection with metadata preview
- Calibration summary display (flats, darks, bias counts)
- Multiple export modes (scripts only, organize files, direct execution)
- Multiple Siril workflows (Mono, OSC, LRGB)
- File organization into standard folder structure
- Siril script generation (.ssf files)

### Known Issues
- Light frame detection may have issues with database query execution
- Direct Siril execution not yet tested
- Progress events during Siril execution need verification

---

## Architecture

### Backend Structure

```
src-tauri/src/export/
├── mod.rs                 # Module exports
├── models.rs              # Data structures (ExportConfig, ExportData, etc.)
├── data_collector.rs      # Collects frames + calibrations from database
├── file_organizer.rs      # Creates folder structure, copies/symlinks files
└── siril/
    ├── mod.rs             # Siril submodule exports
    ├── templates.rs       # Siril script templates
    ├── script_generator.rs # Generates .ssf scripts from templates
    └── cli_runner.rs      # Executes siril-cli with progress tracking
```

### Frontend Structure

```
src/
├── types/export.ts                    # TypeScript interfaces
├── hooks/useExportData.ts             # React hooks for export operations
├── components/export/
│   ├── index.ts                       # Component exports
│   ├── ExportWizard.tsx               # Main wizard component
│   ├── FrameSetSelector.tsx           # Frame set selection
│   ├── ExportModeSelector.tsx         # Export mode options
│   ├── WorkflowSelector.tsx           # Siril workflow selection
│   ├── CalibrationPreview.tsx         # Calibration summary display
│   └── ExportProgress.tsx             # Progress indicator
└── pages/Export.tsx                   # Export page
```

### Tauri Commands

| Command | Description |
|---------|-------------|
| `get_export_preview` | Collect frames and calibrations for preview |
| `export_frame_set` | Execute export (organize + generate scripts) |
| `get_siril_path` | Get configured Siril CLI path |
| `set_siril_path` | Save Siril CLI path to settings |
| `get_exportable_frame_sets` | List available frame sets |

---

## Data Flow

### 1. Frame Collection

```
Frame Set (frames_set)
    └── Imaging Nights (imaging_nights)
        └── Sessions (sessions)
            └── Session Members (session_members)
                └── Frames (frames) where imagetyp = 'Light'
```

The `data_collector.rs` traverses this hierarchy to find all light frames belonging to a frame set.

### 2. Calibration Linking

For each light frame, calibrations are retrieved from `calibration_set_to_frames`:

```
Light Frame
    ├── Flat Set (calibration_type = 'Flat')
    │   └── Sub-calibrations (Dark for Flat, or Bias)
    ├── Dark Set (calibration_type = 'Dark')
    │   └── Sub-calibrations (Bias for Dark)
    └── Bias Set (calibration_type = 'Bias')
```

### 3. Export Execution

```
User Selection
    │
    ▼
┌─────────────────────────────────┐
│  collect_export_data()          │  ← Gather all frames + calibrations
└─────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────┐
│  organize_files()               │  ← Create folders, copy/symlink files
└─────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────┐
│  generate_scripts()             │  ← Create .ssf Siril scripts
└─────────────────────────────────┘
    │
    ▼ (if DirectExecution mode)
┌─────────────────────────────────┐
│  run_siril_script()             │  ← Execute siril-cli
└─────────────────────────────────┘
```

---

## Folder Structure Created

When exporting, the following folder structure is created:

```
{output_dir}/
├── Lights/
│   ├── Ha/              # Lights grouped by filter
│   ├── OIII/
│   └── SII/
├── Calibration/
│   ├── Darks/
│   ├── Flats/
│   │   ├── Ha/          # Flats grouped by filter
│   │   ├── OIII/
│   │   └── SII/
│   ├── Bias/
│   └── DarkFlats/
├── masters/             # Generated master calibration frames
├── process/             # Siril working directory
├── result/              # Final stacked images
├── Ha_preprocessing.ssf # Siril script per filter
├── OIII_preprocessing.ssf
└── SII_preprocessing.ssf
```

---

## Siril Script Generation

### Workflow Types

1. **Mono Preprocessing** (`mono_preprocessing`)
   - Processes each filter separately
   - Best for narrowband imaging or LRGB with mono camera
   - Generates one script per filter

2. **OSC Preprocessing** (`osc_preprocessing`)
   - Processes one-shot color camera images
   - Includes debayering step
   - Single script for all lights

3. **LRGB Processing** (`lrgb_processing`)
   - Combines L, R, G, B channels
   - Assumes individual channels already preprocessed
   - Creates RGB composite

### Script Template Variables

| Variable | Description |
|----------|-------------|
| `{working_dir}` | Base output directory |
| `{filter}` | Filter name (e.g., "Ha", "OIII") |
| `{rejection_low}` | Low sigma for stacking rejection |
| `{rejection_high}` | High sigma for stacking rejection |
| `{lights_dir}` | Path to light frames |
| `{darks_dir}` | Path to dark frames |
| `{flats_dir}` | Path to flat frames |
| `{bias_dir}` | Path to bias frames |
| `{masters_dir}` | Path for master frames |
| `{process_dir}` | Siril working directory |
| `{result_dir}` | Output directory for stacked images |

### Example Generated Script

```
############################################
# Siril Preprocessing Script - Ha
# Generated by Athenaeum
############################################

requires 1.2.0
cd /Users/astro/export/M42

# Create Master Bias
cd /Users/astro/export/M42/Calibration/Bias
convert bias -out=/Users/astro/export/M42/process
cd /Users/astro/export/M42/process
stack bias rej 3 3 -nonorm -out=/Users/astro/export/M42/masters/master_bias

# Create Master Dark
cd /Users/astro/export/M42/Calibration/Darks
convert darks -out=/Users/astro/export/M42/process
cd /Users/astro/export/M42/process
stack darks rej 3 3 -nonorm -out=/Users/astro/export/M42/masters/master_dark

# Create Master Flat
cd /Users/astro/export/M42/Calibration/Flats/Ha
convert flats -out=/Users/astro/export/M42/process
cd /Users/astro/export/M42/process
calibrate flats -dark=/Users/astro/export/M42/masters/master_dark
stack pp_flats rej 3 3 -norm=mul -out=/Users/astro/export/M42/masters/master_flat

# Process Lights
cd /Users/astro/export/M42/Lights/Ha
convert lights -out=/Users/astro/export/M42/process
cd /Users/astro/export/M42/process
calibrate pp_lights -dark=/Users/astro/export/M42/masters/master_dark -flat=/Users/astro/export/M42/masters/master_flat -cc=dark

# Register
register pp_lights

# Stack
stack r_pp_lights rej 3 3 -norm=addscale -output_norm -out=/Users/astro/export/M42/result/Ha_stacked

close
```

---

## Export Modes

| Mode | Description | Files Copied | Scripts Generated | Siril Executed |
|------|-------------|:------------:|:-----------------:|:--------------:|
| `generate_scripts` | Scripts only | No | Yes | No |
| `organize_files` | Organize only | Yes | No | No |
| `organize_and_script` | Both | Yes | Yes | No |
| `direct_execution` | Full pipeline | Yes | Yes | Yes |

---

## Configuration Options

### Export Config

```typescript
interface ExportConfig {
  frameSetId: number;       // Frame set to export
  outputDir: string;        // Output directory path
  mode: ExportMode;         // Export operation mode
  workflow: SirilWorkflow;  // Siril workflow type
  createMasters: boolean;   // Create master calibration frames
  rejectionLow: number;     // Low rejection sigma (default: 3.0)
  rejectionHigh: number;    // High rejection sigma (default: 3.0)
  useSymlinks: boolean;     // Use symlinks instead of copying
}
```

### Siril Path

The Siril CLI path is stored in the `settings` table with key `siril_cli_path`. If not configured, the application attempts to auto-detect Siril in common locations:

**macOS:**
- `/Applications/Siril.app/Contents/MacOS/siril-cli`
- `/usr/local/bin/siril-cli`
- `/opt/homebrew/bin/siril-cli`

**Linux:**
- `/usr/bin/siril-cli`
- `/usr/local/bin/siril-cli`

**Windows:**
- `siril-cli.exe` (in PATH)

---

## Future Plans

### Phase 2: Enhanced Siril Integration
- [ ] Real-time progress parsing from Siril output
- [ ] Better error handling and recovery
- [ ] Support for Siril's native star detection
- [ ] Drizzle integration for super-resolution

### Phase 3: PixInsight Integration
- [ ] Generate PixInsight process icons (.xpsm files)
- [ ] Integration via PixInsight's command-line interface
- [ ] Support for WBPP (Weighted Batch Preprocessing) workflow

### Phase 4: Advanced Features
- [ ] Custom script templates (user-defined)
- [ ] Batch export (multiple frame sets)
- [ ] Export presets (save/load configurations)
- [ ] Post-processing scripts integration
- [ ] Master library reuse (detect existing masters)

### Phase 5: Quality Assurance
- [ ] Pre-flight checks (verify all files exist)
- [ ] Calibration quality warnings (age, temperature)
- [ ] Disk space estimation before export
- [ ] Export history and logging

---

## Database Tables Used

### Primary Tables

- `frames_set` - Top-level frame set info
- `imaging_nights` - Nights within a frame set
- `sessions` - Camera sessions within a night
- `session_members` - Links sessions to frames
- `frames` - Frame metadata
- `files` - File paths and info

### Calibration Tables

- `calibration_set` - Calibration set metadata
- `calibration_set_frames` - Links sets to frames
- `calibration_set_to_frames` - Links sources to calibration sets

### Settings

- `settings` - Application settings (includes `siril_cli_path`)

---

## Troubleshooting

### No frames found in frame set

1. Check that frames have `imagetyp = 'Light'` in the database
2. Verify the frame set hierarchy exists:
   - `frames_set` → `imaging_nights` → `sessions` → `session_members` → `frames`
3. Check console output for debug messages

### Siril not found

1. Install Siril from https://siril.org/
2. Configure the path in Settings → Export → Siril CLI Path
3. Or ensure `siril-cli` is in your system PATH

### Scripts not generating

1. Ensure output directory is writable
2. Check that at least one filter group has light frames
3. Review console for error messages

---

## API Reference

### useExportData Hook

```typescript
function useExportData(frameSetId: number | null): {
  data: ExportData | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}
```

### useExport Hook

```typescript
function useExport(): {
  execute: (config: ExportExecuteConfig) => Promise<ExportResult>;
  loading: boolean;
  error: string | null;
  result: ExportResult | null;
}
```

### useSirilPath Hook

```typescript
function useSirilPath(): {
  path: string | null;
  loading: boolean;
  error: string | null;
  setPath: (path: string) => Promise<void>;
  refresh: () => void;
}
```
