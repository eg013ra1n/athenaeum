# Export Module - Technical Reference

## Module Dependencies

```
export/
├── models.rs          ← No internal dependencies
├── data_collector.rs  ← Uses models.rs
├── file_organizer.rs  ← Uses models.rs
└── siril/
    ├── templates.rs       ← Uses models.rs (SirilWorkflow)
    ├── script_generator.rs ← Uses templates.rs, file_organizer.rs, models.rs
    └── cli_runner.rs      ← Uses models.rs (ExportProgress, ExportStage)
```

## Key Data Structures

### ExportData (Full export preview)

```rust
pub struct ExportData {
    pub frame_set_id: i64,
    pub frame_set_name: String,
    pub object_name: Option<String>,
    pub filters: Vec<FilterExportGroup>,      // Light frames grouped by filter
    pub calibration_summary: CalibrationSummary,
    pub total_light_frames: i32,
    pub total_exposure_seconds: f64,
}
```

### FilterExportGroup (Per-filter data)

```rust
pub struct FilterExportGroup {
    pub filter: Option<String>,               // Filter name (None = unfiltered)
    pub light_frames: Vec<ExportFrame>,       // Light frames for this filter
    pub flat_sets: Vec<ExportCalibrationSet>, // Matched flat calibration sets
    pub dark_sets: Vec<ExportCalibrationSet>, // Matched dark calibration sets
    pub bias_sets: Vec<ExportCalibrationSet>, // Matched bias calibration sets
}
```

### ExportCalibrationSet (Calibration with sub-calibrations)

```rust
pub struct ExportCalibrationSet {
    pub set_id: i64,
    pub imagetyp: String,                           // "FLAT", "DARK", "BIAS", etc.
    pub frames: Vec<ExportFrame>,                   // Frames in this set
    pub sub_calibrations: Vec<ExportCalibrationSet>, // Nested calibrations
    pub match_score: Option<f64>,                   // Match quality (0.0-1.0)
    pub warnings: Vec<String>,                      // Warnings for this match
}
```

## Database Queries

### Get Light Frames for Frame Set

Uses the proven pattern from `calibration_links.rs`:

```sql
SELECT DISTINCT sm.frame_id
FROM session_members sm
JOIN sessions s ON sm.session_id = s.id
JOIN imaging_nights n ON s.imaging_night_id = n.id
JOIN frames f ON sm.frame_id = f.id
WHERE n.frames_set_id = ?1 AND f.imagetyp = 'Light'
```

### Get Calibration Links for Frame

```sql
SELECT calibration_set_id, calibration_type, match_score,
       date_warning, temp_warning
FROM calibration_set_to_frames
WHERE source_id = ?1 AND source_type = 'frame'
```

### Get Sub-Calibrations for Calibration Set

```sql
SELECT calibration_set_id, calibration_type, match_score,
       date_warning, temp_warning
FROM calibration_set_to_frames
WHERE source_id = ?1 AND source_type = 'calibration_set'
```

## Siril Script Template System

### Template Structure

Each workflow has a main template with section placeholders:

```
{bias_section}      → Populated with BIAS_SECTION_TEMPLATE or BIAS_SECTION_EMPTY
{dark_section}      → Populated with DARK_SECTION_TEMPLATE or DARK_SECTION_EMPTY
{flat_section}      → Populated with FLAT_SECTION_TEMPLATE or FLAT_SECTION_EMPTY
{calibrate_lights_cmd} → Populated based on available calibrations
```

### Section Selection Logic

```rust
// Bias section
if has_bias { BIAS_SECTION_TEMPLATE } else { BIAS_SECTION_EMPTY }

// Dark section
if has_dark {
    if has_bias && config.create_masters {
        DARK_SECTION_WITH_BIAS_TEMPLATE  // Calibrate darks with bias
    } else {
        DARK_SECTION_TEMPLATE            // Stack darks without bias
    }
} else { DARK_SECTION_EMPTY }

// Flat section
if has_flat {
    if has_dark { FLAT_SECTION_TEMPLATE }        // Calibrate flats with dark
    else if has_bias { FLAT_SECTION_BIAS_ONLY }  // Calibrate flats with bias
    else { FLAT_SECTION_EMPTY }
} else { FLAT_SECTION_EMPTY }

// Light calibration command
if has_dark && has_flat { CALIBRATE_LIGHTS_FULL }
else if has_dark { CALIBRATE_LIGHTS_DARK_ONLY }
else if has_flat { CALIBRATE_LIGHTS_FLAT_ONLY }
else { CALIBRATE_LIGHTS_NONE }
```

## File Organization

### ExportFolders Structure

```rust
pub struct ExportFolders {
    pub root: PathBuf,           // Base output directory
    pub lights: PathBuf,         // {root}/Lights
    pub calibration: PathBuf,    // {root}/Calibration
    pub darks: PathBuf,          // {root}/Calibration/Darks
    pub flats: PathBuf,          // {root}/Calibration/Flats
    pub bias: PathBuf,           // {root}/Calibration/Bias
    pub dark_flats: PathBuf,     // {root}/Calibration/DarkFlats
    pub masters: PathBuf,        // {root}/masters
    pub process: PathBuf,        // {root}/process
    pub result: PathBuf,         // {root}/result
}
```

### Filter Subfolders

```rust
// Lights per filter
folders.lights_for_filter(Some("Ha")) // → {root}/Lights/Ha
folders.lights_for_filter(None)       // → {root}/Lights

// Flats per filter
folders.flats_for_filter(Some("Ha"))  // → {root}/Calibration/Flats/Ha
folders.flats_for_filter(None)        // → {root}/Calibration/Flats
```

## Progress Events

### Event Name
`export-progress`

### Payload Structure

```rust
pub struct ExportProgress {
    pub stage: ExportStage,
    pub progress: f64,           // 0-100
    pub message: String,
    pub current_file: Option<String>,
}

pub enum ExportStage {
    Collecting,
    Organizing,
    GeneratingScripts,
    SirilCalibrating,
    SirilRegistering,
    SirilStacking,
    Complete,
    Failed,
}
```

### Listening in Frontend

```typescript
import { listen } from '@tauri-apps/api/event';

useEffect(() => {
  const unlisten = listen<ExportProgress>('export-progress', (event) => {
    setProgress(event.payload);
  });
  return () => { unlisten.then((fn) => fn()); };
}, []);
```

## Error Handling

### Rust Side
- Uses `anyhow::Result` for internal operations
- Converts to `String` at Tauri command boundary
- Graceful fallbacks for missing calibrations

### Frontend Side
- Errors stored in hook state
- Displayed in UI via ExportResult
- Warnings collected but don't block export

## Testing Notes

### Manual Testing Checklist

1. [ ] Select frame set with various filter configurations
2. [ ] Verify calibration summary shows correct counts
3. [ ] Export with each mode (scripts, organize, both)
4. [ ] Verify folder structure creation
5. [ ] Verify script content placeholders replaced
6. [ ] Test symlink vs copy option
7. [ ] Test with missing calibrations (should still export lights)
8. [ ] Test direct execution with Siril installed

### Unit Tests

Located in each module with `#[cfg(test)]` blocks:
- `data_collector.rs` - Filter sorting tests
- `file_organizer.rs` - Folder structure tests
- `templates.rs` - Template selection tests
- `script_generator.rs` - Placeholder replacement tests
- `cli_runner.rs` - Output parsing tests

## Performance Considerations

1. **Frame Collection**: Uses indexed queries on `frames_set_id` and `imagetyp`
2. **File Copy**: Large files copied sequentially to avoid memory issues
3. **Symlinks**: Preferred for large datasets (no disk space duplication)
4. **Script Generation**: Templates pre-compiled as static strings
5. **Siril Execution**: Runs in separate thread with progress streaming
