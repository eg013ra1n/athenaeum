# Export System Refactoring - Technical Specification

## Document Information
- **Version**: 1.0
- **Date**: 2026-01-02
- **Status**: Draft

## 1. Executive Summary

This document specifies the refactoring of the Athenaeum export system to produce master light files through a complete Siril processing pipeline. The system will:

1. Group light frames into **Export Groups** based on filter AND camera type (OSC vs Mono)
2. Generate proper calibration masters for each group
3. Register all calibrated frames together (regardless of filter/camera)
4. Stack each Export Group into a final master light
5. Display the complete calibration route in the UI

## 2. Key Concepts

### 2.1 Master Light Definition

A **Master Light** is the stacked result of calibrated light frames that share:
- **Same Filter** - All frames use the same filter (or no filter for unfiltered)
- **Same Camera Type** - Either all OSC or all Mono cameras

### 2.2 Export Groups and Calibration Subgroups

An **Export Group** represents frames that will be stacked together into one master light.
Within an Export Group, frames may have **different calibration sets** linked (from different
cameras, nights, or optical setups). These are organized into **Calibration Subgroups**.

```
Export Group = {
    filter: String | None,
    camera_type: OSC | Mono,
    subgroups: Vec<CalibrationSubgroup>
}

CalibrationSubgroup = {
    frames: Vec<LightFrame>,        // Frames sharing the same calibration links
    flat_set_id: Option<i64>,       // Linked flat calibration set
    dark_set_id: Option<i64>,       // Linked dark calibration set
    bias_set_id: Option<i64>,       // Linked bias calibration set
}
```

**Example**: Ha (Mono) Export Group with 3 subgroups:

```
Ha (Mono) Export Group
├── Subgroup 1 (Flat=5, Dark=12, Bias=3)
│   └── Frames: [A, B, C] from Camera X, Night 1
├── Subgroup 2 (Flat=5, Dark=15, Bias=3)
│   └── Frames: [D, E] from Camera X, Night 2 (different dark)
└── Subgroup 3 (Flat=8, Dark=18, Bias=7)
    └── Frames: [F, G, H] from Camera Y (completely different setup)
```

### 2.3 Calibration Set Sub-Calibrations

Each calibration set can have its own linked sub-calibrations stored in `calibration_set_to_frames`:

```
Calibration Hierarchy:
├── Flat Set 5
│   ├── → DarkFlat Set 20 (or Dark as fallback)
│   │   └── → Bias Set 3
│   └── → Bias Set 3
├── Dark Set 12
│   └── → Bias Set 3 (if bias optimization enabled)
└── Bias Set 3
    └── (no sub-calibrations)
```

The export system must:
1. Traverse each calibration set's linked sub-calibrations
2. Create masters in dependency order (Bias first, then Dark/DarkFlat, then Flat)
3. Apply the correct masters when calibrating each calibration set

### 2.4 Master Reuse Strategy

Each unique calibration set is converted to a master **once**, then reused:

```
Unique Masters to Create (in order):
1. master_bias_3   (no dependencies)
2. master_bias_7   (no dependencies)
3. master_dark_12  (uses bias_3 if optimization on)
4. master_dark_15  (uses bias_3 if optimization on)
5. master_dark_18  (uses bias_7 if optimization on)
6. master_darkflat_20 (uses bias_3)
7. master_flat_5   (uses darkflat_20 + bias_3)
8. master_flat_8   (uses its own linked darks/bias)

Light Frame Calibration:
├── Frames A, B, C → calibrated with flat_5, dark_12, bias_3
├── Frames D, E    → calibrated with flat_5, dark_15, bias_3
└── Frames F, G, H → calibrated with flat_8, dark_18, bias_7
```

After calibration, ALL frames (pp_A through pp_H) are registered and stacked together.

### 2.5 Camera Type Detection

**OSC (One-Shot Color)** cameras are identified by the presence of `BAYERPAT` in the FITS header:
- `BAYERPAT = 'RGGB'` → OSC camera
- `BAYERPAT = None/missing` → Mono camera

**Rule**: OSC and Mono cameras NEVER combine, even with matching filters.

### 2.6 Plate Scale Handling

Frames with different focal lengths are registered together:
- Siril's registration resamples all frames to a common plate scale
- All frames within an Export Group (same filter + camera type) are registered together
- Final registration step aligns all Export Groups for multi-filter compositing

## 3. Required Database Changes

### 3.1 Add BAYERPAT to frames table

```sql
ALTER TABLE frames ADD COLUMN bayerpat TEXT;
```

**Location**: `src-tauri/src/db/schema.rs`

Add migration:
```rust
let has_bayerpat: Result<i64, _> = conn.query_row(
    "SELECT COUNT(*) FROM pragma_table_info('frames') WHERE name='bayerpat'",
    [],
    |row| row.get(0),
);
if let Ok(0) = has_bayerpat {
    conn.execute(
        "ALTER TABLE frames ADD COLUMN bayerpat TEXT",
        [],
    )?;
}
```

### 3.2 Update FITS/XISF Parser

**Location**: `src-tauri/src/fits_parser/mod.rs`

Add BAYERPAT extraction:
```rust
// In parse_fits():
let bayerpat = read_keyword_string(&mut fitsfile, &hdu, "BAYERPAT").ok();

// In parse_xisf():
let bayerpat = fits_keywords.get("BAYERPAT").cloned();
```

### 3.3 Update Frame Model

**Location**: `src-tauri/src/models.rs`

```rust
pub struct Frame {
    // ... existing fields ...
    pub bayerpat: Option<String>,
}
```

### 3.4 Update db/operations.rs

Add `bayerpat` to INSERT and SELECT statements for frames.

## 4. Export Data Model Refactoring

### 4.1 New Model: CameraType

**Location**: `src-tauri/src/export/models.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CameraType {
    /// One-shot color camera (has Bayer pattern)
    Osc,
    /// Monochrome camera (no Bayer pattern)
    Mono,
}

impl CameraType {
    pub fn from_bayerpat(bayerpat: Option<&str>) -> Self {
        match bayerpat {
            Some(pattern) if !pattern.is_empty() => CameraType::Osc,
            _ => CameraType::Mono,
        }
    }
}
```

### 4.2 Rename FilterExportGroup → ExportGroup

```rust
/// An export group - frames that will be stacked into one master light
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportGroup {
    /// Unique group key for identification (e.g., "Ha_Mono")
    pub group_key: String,

    /// Filter name (None for unfiltered/OSC)
    pub filter: Option<String>,

    /// Camera type (OSC or Mono)
    pub camera_type: CameraType,

    /// Display name for UI (e.g., "Ha (Mono)", "Luminance (OSC)")
    pub display_name: String,

    /// Calibration subgroups - frames grouped by their linked calibration sets
    pub subgroups: Vec<CalibrationSubgroup>,

    /// Total light frame count across all subgroups
    pub total_frames: i32,

    /// Total exposure time across all subgroups
    pub total_exposure: f64,

    /// Warnings specific to this group
    pub warnings: Vec<String>,
}
```

### 4.3 New Model: CalibrationSubgroup

```rust
/// A subgroup of frames that share the same calibration set links
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationSubgroup {
    /// Unique subgroup key (hash of calibration set IDs)
    pub subgroup_key: String,

    /// Display name (e.g., "Night 1 - Camera X" or auto-generated)
    pub display_name: String,

    /// Light frames in this subgroup
    pub frames: Vec<ExportFrame>,

    /// Linked Flat calibration set (with its own sub-calibrations)
    pub flat: Option<CalibrationSetInfo>,

    /// Linked Dark calibration set (with its own sub-calibrations)
    pub dark: Option<CalibrationSetInfo>,

    /// Linked Bias calibration set
    pub bias: Option<CalibrationSetInfo>,

    /// Warnings for this subgroup
    pub warnings: Vec<String>,
}
```

### 4.4 New Model: CalibrationSetInfo

```rust
/// Information about a calibration set and its sub-calibrations
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationSetInfo {
    /// Calibration set ID
    pub set_id: i64,

    /// Image type (FLAT, DARK, BIAS, DARKFLAT)
    pub imagetyp: String,

    /// Frames in this calibration set
    pub frames: Vec<ExportFrame>,

    /// Frame count
    pub frame_count: i32,

    /// Sub-calibrations for this set (recursive)
    /// - Flat → may have DarkFlat/Dark and Bias
    /// - Dark → may have Bias (if optimization enabled)
    /// - Bias → None
    pub dark_flat: Option<Box<CalibrationSetInfo>>,
    pub dark: Option<Box<CalibrationSetInfo>>,
    pub bias: Option<Box<CalibrationSetInfo>>,

    /// Match quality score (0.0 - 1.0)
    pub match_score: Option<f64>,

    /// Warnings (date, temperature, etc.)
    pub warnings: Vec<String>,
}
```

### 4.5 New Model: MasterCreationPlan

```rust
/// Plan for creating all required master calibration files
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterCreationPlan {
    /// Ordered list of masters to create (respects dependencies)
    pub masters: Vec<MasterInfo>,

    /// Map of set_id → master file path for reference
    pub master_paths: HashMap<i64, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterInfo {
    /// Calibration set ID
    pub set_id: i64,

    /// Master type (Bias, Dark, DarkFlat, Flat)
    pub master_type: String,

    /// Output filename (e.g., "master_bias_3.fit")
    pub output_name: String,

    /// Source frames
    pub source_frames: Vec<ExportFrame>,

    /// Dependencies - set IDs of masters needed before this one
    pub depends_on: Vec<i64>,

    /// Calibration masters to apply when creating this master
    pub apply_bias: Option<i64>,
    pub apply_dark: Option<i64>,
}
```

### 4.6 Update ExportFrame

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFrame {
    pub frame_id: i64,
    pub file_id: i64,
    pub file_path: String,
    pub filename: String,
    pub exptime: Option<f64>,
    pub filter: Option<String>,
    pub ccd_temp: Option<f64>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub binning: Option<String>,
    pub date_obs: Option<String>,
    pub focallen: Option<f64>,    // NEW: for plate scale info
    pub bayerpat: Option<String>, // NEW: for OSC detection
    pub instrume: Option<String>, // NEW: camera name for display
}
```

### 4.7 New Model: CalibrationRoute (for UI display)

```rust
/// Calibration route for UI display
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationRoute {
    /// Export groups and their calibration trees
    pub groups: Vec<CalibrationRouteGroup>,

    /// Generated Siril script preview
    pub script_preview: Vec<SirilScriptPreview>,

    /// Overall summary
    pub summary: CalibrationRouteSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationRouteGroup {
    /// Group display name (e.g., "Ha (Mono)")
    pub name: String,

    /// Number of light frames
    pub light_count: i32,

    /// Total exposure time
    pub total_exposure: f64,

    /// Calibration tree nodes
    pub calibration_tree: Vec<CalibrationTreeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationTreeNode {
    /// Node type: "Light", "Flat", "Dark", "Bias", "DarkFlat"
    pub node_type: String,

    /// Display label
    pub label: String,

    /// Frame count
    pub count: i32,

    /// Child nodes (sub-calibrations)
    pub children: Vec<CalibrationTreeNode>,

    /// Warnings
    pub warnings: Vec<String>,

    /// Whether this node is missing/incomplete
    pub is_missing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SirilScriptPreview {
    /// Script name
    pub name: String,

    /// Script purpose description
    pub description: String,

    /// Full script content
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationRouteSummary {
    /// Total export groups
    pub group_count: i32,

    /// Total light frames
    pub total_lights: i32,

    /// Total exposure time (seconds)
    pub total_exposure: f64,

    /// Calibration completeness
    pub flats_complete: bool,
    pub darks_complete: bool,
    pub bias_complete: bool,

    /// Overall warnings
    pub warnings: Vec<String>,
}
```

## 5. Export Workflow

### 5.1 Phase 1: Data Collection

```
1. Load all Light frames from frame set
2. Load BAYERPAT for each frame (from DB or re-parse if null)
3. Group frames by (filter, camera_type) → Export Groups
4. For each Export Group:
   a. Query each frame's linked calibration sets from calibration_set_to_frames
   b. Group frames by (flat_set_id, dark_set_id, bias_set_id) → Subgroups
   c. For each unique calibration set:
      - Load its sub-calibrations (Flat→DarkFlat/Dark+Bias, Dark→Bias)
      - Build CalibrationSetInfo with full hierarchy
5. Build MasterCreationPlan:
   a. Collect all unique calibration set IDs across all subgroups
   b. Determine dependencies (which masters need other masters first)
   c. Topologically sort to get creation order
6. Return ExportData with all groups and master plan
```

### 5.1.1 Subgroup Creation Algorithm

```rust
fn create_subgroups(frames: Vec<Frame>, conn: &Connection) -> Vec<CalibrationSubgroup> {
    // 1. Query calibration links for each frame
    let frame_calibrations: HashMap<i64, CalibrationLinks> = frames.iter()
        .map(|f| (f.id, query_calibration_links(conn, f.id)))
        .collect();

    // 2. Group frames by their calibration set ID combination
    let mut subgroup_map: HashMap<(Option<i64>, Option<i64>, Option<i64>), Vec<Frame>> = HashMap::new();

    for frame in frames {
        let links = &frame_calibrations[&frame.id];
        let key = (links.flat_set_id, links.dark_set_id, links.bias_set_id);
        subgroup_map.entry(key).or_default().push(frame);
    }

    // 3. Build CalibrationSubgroup for each unique combination
    subgroup_map.into_iter()
        .map(|((flat_id, dark_id, bias_id), frames)| {
            CalibrationSubgroup {
                subgroup_key: format!("{}_{}_{}",
                    flat_id.unwrap_or(0),
                    dark_id.unwrap_or(0),
                    bias_id.unwrap_or(0)),
                frames,
                flat: flat_id.map(|id| load_calibration_set_with_subs(conn, id)),
                dark: dark_id.map(|id| load_calibration_set_with_subs(conn, id)),
                bias: bias_id.map(|id| load_calibration_set_with_subs(conn, id)),
                ..
            }
        })
        .collect()
}
```

### 5.1.2 Master Creation Plan Algorithm

```rust
fn build_master_creation_plan(export_groups: &[ExportGroup]) -> MasterCreationPlan {
    let mut all_sets: HashSet<(i64, String)> = HashSet::new(); // (set_id, type)
    let mut dependencies: HashMap<i64, Vec<i64>> = HashMap::new();

    // 1. Collect all unique calibration sets and their dependencies
    for group in export_groups {
        for subgroup in &group.subgroups {
            collect_sets_recursive(&subgroup.flat, &mut all_sets, &mut dependencies);
            collect_sets_recursive(&subgroup.dark, &mut all_sets, &mut dependencies);
            collect_sets_recursive(&subgroup.bias, &mut all_sets, &mut dependencies);
        }
    }

    // 2. Topological sort by dependencies
    // Bias sets have no dependencies → first
    // Dark/DarkFlat sets depend on Bias → second
    // Flat sets depend on Dark + Bias → last
    let ordered = topological_sort(&all_sets, &dependencies);

    // 3. Build MasterInfo for each set
    MasterCreationPlan {
        masters: ordered.into_iter()
            .map(|(set_id, set_type)| MasterInfo {
                set_id,
                master_type: set_type,
                output_name: format!("master_{}_{}.fit", set_type.to_lowercase(), set_id),
                depends_on: dependencies.get(&set_id).cloned().unwrap_or_default(),
                ..
            })
            .collect(),
        master_paths: HashMap::new(), // Filled during file organization
    }
}
```

### 5.2 Phase 2: File Organization

Directory structure with subgroup support:

```
<output_root>/
├── masters/                         # All master calibration files
│   ├── master_bias_3.fit
│   ├── master_bias_7.fit
│   ├── master_dark_12.fit
│   ├── master_dark_15.fit
│   ├── master_darkflat_20.fit
│   ├── master_flat_5.fit
│   └── master_flat_8.fit
├── calibration/                     # Source calibration frames (by set ID)
│   ├── bias_3/
│   ├── bias_7/
│   ├── dark_12/
│   ├── dark_15/
│   ├── darkflat_20/
│   ├── flat_5/
│   └── flat_8/
├── Ha_Mono/                         # Export Group
│   ├── lights/                      # All light frames for this group
│   ├── process/                     # Working directory
│   └── result/                      # Stacked result
├── OIII_Mono/
│   ├── lights/
│   ├── process/
│   └── result/
├── scripts/
│   ├── 00_create_masters.ssf        # Creates all calibration masters
│   ├── 01_calibrate_Ha_Mono.ssf     # Calibrates and stacks Ha
│   ├── 02_calibrate_OIII_Mono.ssf
│   └── 99_register_all.ssf          # Cross-group registration
└── result/                          # Final registered masters
```

Key changes:
- **Shared masters folder**: All calibration masters are created in one central location
- **Calibration by set ID**: Source frames organized by calibration set ID, not by type
- **Master reuse**: Each master is created once, referenced by multiple subgroups

### 5.3 Phase 3: Script Generation

#### 5.3.1 Master Creation Script (00_create_masters.ssf)

First script creates all calibration masters in dependency order:

```siril
requires 1.2.0

# ===========================================
# Master Calibration Creation
# Generated by Athenaeum
# ===========================================

cd {output_root}

# ========== BIAS MASTERS ==========
# Master Bias 3 (no dependencies)
cd calibration/bias_3
convert bias_ -out=../../masters/process
cd ../../masters/process
stack bias_ rej 3 3 -norm=no -out=../master_bias_3

# Master Bias 7 (no dependencies)
cd ../../calibration/bias_7
convert bias_ -out=../../masters/process
cd ../../masters/process
stack bias_ rej 3 3 -norm=no -out=../master_bias_7

# ========== DARK MASTERS ==========
# Master Dark 12 (uses Bias 3)
cd ../../calibration/dark_12
convert dark_ -out=../../masters/process
cd ../../masters/process
calibrate dark_ -bias=../master_bias_3
stack pp_dark_ rej 3 3 -norm=no -out=../master_dark_12

# Master Dark 15 (uses Bias 3)
cd ../../calibration/dark_15
convert dark_ -out=../../masters/process
cd ../../masters/process
calibrate dark_ -bias=../master_bias_3
stack pp_dark_ rej 3 3 -norm=no -out=../master_dark_15

# Master Dark 18 (uses Bias 7)
cd ../../calibration/dark_18
convert dark_ -out=../../masters/process
cd ../../masters/process
calibrate dark_ -bias=../master_bias_7
stack pp_dark_ rej 3 3 -norm=no -out=../master_dark_18

# ========== DARKFLAT MASTERS ==========
# Master DarkFlat 20 (uses Bias 3)
cd ../../calibration/darkflat_20
convert darkflat_ -out=../../masters/process
cd ../../masters/process
calibrate darkflat_ -bias=../master_bias_3
stack pp_darkflat_ rej 3 3 -norm=no -out=../master_darkflat_20

# ========== FLAT MASTERS ==========
# Master Flat 5 (uses DarkFlat 20 + Bias 3)
cd ../../calibration/flat_5
convert flat_ -out=../../masters/process
cd ../../masters/process
calibrate flat_ -bias=../master_bias_3 -dark=../master_darkflat_20
stack pp_flat_ rej 3 3 -norm=mul -out=../master_flat_5

# Master Flat 8 (uses Dark 18 + Bias 7)
cd ../../calibration/flat_8
convert flat_ -out=../../masters/process
cd ../../masters/process
calibrate flat_ -bias=../master_bias_7 -dark=../master_dark_18
stack pp_flat_ rej 3 3 -norm=mul -out=../master_flat_8

cd ..
close
```

#### 5.3.2 Per-Group Calibration Script

For each Export Group, generate a calibration script that handles multiple subgroups:

**Ha (Mono) Script - 01_calibrate_Ha_Mono.ssf:**

```siril
requires 1.2.0

# ===========================================
# Calibration Script: Ha (Mono)
# Generated by Athenaeum
# Subgroups: 3
# ===========================================

cd {output_root}/Ha_Mono

# ========== SUBGROUP 1: Frames A, B, C ==========
# Uses: Flat 5, Dark 12, Bias 3

cd lights
# Convert subgroup 1 frames
convert A B C -out=../process/sg1
cd ../process/sg1
calibrate A B C -bias=../../masters/master_bias_3 -dark=../../masters/master_dark_12 -flat=../../masters/master_flat_5
# Move calibrated files to common folder
cd ..
# (files are now pp_A, pp_B, pp_C in process/sg1)

# ========== SUBGROUP 2: Frames D, E ==========
# Uses: Flat 5, Dark 15, Bias 3

cd ../lights
convert D E -out=../process/sg2
cd ../process/sg2
calibrate D E -bias=../../masters/master_bias_3 -dark=../../masters/master_dark_15 -flat=../../masters/master_flat_5

# ========== SUBGROUP 3: Frames F, G, H ==========
# Uses: Flat 8, Dark 18, Bias 7

cd ../../lights
convert F G H -out=../process/sg3
cd ../process/sg3
calibrate F G H -bias=../../masters/master_bias_7 -dark=../../masters/master_dark_18 -flat=../../masters/master_flat_8

# ========== REGISTER ALL CALIBRATED FRAMES ==========
cd ..
# Link all calibrated frames to common folder
link sg1/pp_* sg2/pp_* sg3/pp_* -out=all_calibrated
cd all_calibrated
register pp_

# ========== STACK ==========
stack r_pp_ rej 3 3 -norm=addscale -output_norm -out=../result/Ha_Mono_stacked

cd ../..
close
```

**OSC Group Script differences:**
- Adds `-debayer` flag to each `calibrate` command
- Siril auto-detects Bayer pattern from FITS header

#### 5.3.3 Cross-Group Registration Script

After individual group processing, generate a registration script to align all masters:

```
requires 1.2.0

# ===========================================
# Cross-Group Registration
# Aligns all master lights for compositing
# ===========================================

cd {output_root}

# Load reference (typically Luminance or Ha)
load Ha_Mono/result/Ha_Mono_stacked

# Register other channels to reference
register OIII_Mono/result/OIII_Mono_stacked -transf
register SII_Mono/result/SII_Mono_stacked -transf

# Save registered versions
save result/Ha_registered
save result/OIII_registered
save result/SII_registered

close
```

### 5.4 Phase 4: Siril Execution (Optional)

If `DirectExecution` mode is selected:

1. Execute each calibration script in sequence
2. Execute cross-group registration script
3. Emit progress events for each stage
4. Report results

## 6. UI Requirements

### 6.1 Calibration Route Display

The UI should display a tree view showing the complete calibration hierarchy with subgroups:

```
Export Preview for "M42"
├── Ha (Mono) - 45 frames, 3h 45m total
│   ├── Subgroup 1: Camera X, Night 1 (20 frames)
│   │   ├── 📁 Flat Set 5 (30 frames)
│   │   │   └── 📁 DarkFlat Set 20 (20 frames)
│   │   │       └── 📁 Bias Set 3 (50 frames)
│   │   ├── 📁 Dark Set 12 (25 frames) ⚠️ temp warning
│   │   │   └── 📁 Bias Set 3 (shared)
│   │   └── 📁 Bias Set 3 (shared)
│   ├── Subgroup 2: Camera X, Night 2 (15 frames)
│   │   ├── 📁 Flat Set 5 (shared)
│   │   ├── 📁 Dark Set 15 (25 frames)
│   │   │   └── 📁 Bias Set 3 (shared)
│   │   └── 📁 Bias Set 3 (shared)
│   └── Subgroup 3: Camera Y (10 frames)
│       ├── 📁 Flat Set 8 (20 frames)
│       │   └── 📁 Dark Set 18 (15 frames)
│       │       └── 📁 Bias Set 7 (30 frames)
│       ├── 📁 Dark Set 18 (shared)
│       │   └── 📁 Bias Set 7 (shared)
│       └── 📁 Bias Set 7 (shared)
├── OIII (Mono) - 40 frames, 3h 20m total
│   └── Subgroup 1: (all frames share same calibrations)
│       ├── 📁 Flat Set 5 (shared with Ha)
│       ├── 📁 Dark Set 12 (shared with Ha)
│       └── 📁 Bias Set 3 (shared)
└── RGB (OSC) - 60 frames, 2h total
    └── Subgroup 1:
        ├── 📁 Flat Set 10 (15 frames)
        └── 📁 Dark Set 22 (20 frames)

Master Creation Summary:
├── 7 unique calibration sets to convert to masters
├── 2 Bias masters (Set 3, Set 7)
├── 3 Dark masters (Set 12, Set 15, Set 18)
├── 1 DarkFlat master (Set 20)
└── 2 Flat masters (Set 5, Set 8)
```

Legend:
- **(shared)** - This calibration set is used by multiple subgroups/groups, master created once
- **⚠️** - Warning flag (temperature mismatch, old calibration, etc.)

### 6.2 Script Preview Panel

Collapsible panel showing:
- List of scripts that will be generated
- Click to expand and see full script content
- Syntax highlighting for Siril commands

### 6.3 Warnings Display

- Temperature mismatches
- Date warnings (old calibrations)
- Missing calibrations
- Mixed camera types detected

## 7. Implementation Plan

### Phase 1: Database & Parser Updates
1. Add `bayerpat` column to `frames` table
2. Update `Frame` model in `models.rs`
3. Update FITS parser to extract BAYERPAT
4. Update XISF parser to extract BAYERPAT
5. Update `db/operations.rs` INSERT/SELECT statements
6. Add migration in `schema.rs`

### Phase 2: Export Model Refactoring
1. Create `CameraType` enum
2. Rename `FilterExportGroup` → `ExportGroup`
3. Create `CalibrationChain` and `CalibrationStep` models
4. Create `CalibrationRoute` models for UI
5. Update `ExportFrame` with new fields

### Phase 3: Data Collector Refactoring
1. Update frame queries to include `bayerpat`
2. Implement grouping by (filter, camera_type)
3. Build `CalibrationChain` for each group
4. Generate `CalibrationRoute` for UI preview

### Phase 4: Script Generator Updates
1. Update folder structure creation
2. Generate per-group calibration scripts
3. Handle OSC vs Mono differences (debayer flag)
4. Generate cross-group registration script
5. Use bias optimization setting for dark calibration

### Phase 5: Tauri Commands
1. Update `get_export_preview` to return new models
2. Add `get_calibration_route` command for detailed UI
3. Update `export_frame_set` for new workflow

### Phase 6: Frontend Updates
1. Create CalibrationRouteTree component
2. Create ScriptPreviewPanel component
3. Update Export page layout
4. Add warnings display

## 8. API Changes

### 8.1 get_export_preview

**Input**: `frame_set_id: i64`

**Output**:

```typescript
interface ExportData {
  frameSetId: number;
  frameSetName: string;
  objectName: string | null;
  groups: ExportGroup[];
  masterPlan: MasterCreationPlan;
  calibrationSummary: CalibrationSummary;
  totalLightFrames: number;
  totalExposureSeconds: number;
}

interface ExportGroup {
  groupKey: string;
  filter: string | null;
  cameraType: 'osc' | 'mono';
  displayName: string;
  subgroups: CalibrationSubgroup[];
  totalFrames: number;
  totalExposure: number;
  warnings: string[];
}

interface CalibrationSubgroup {
  subgroupKey: string;
  displayName: string;
  frames: ExportFrame[];
  flat: CalibrationSetInfo | null;
  dark: CalibrationSetInfo | null;
  bias: CalibrationSetInfo | null;
  warnings: string[];
}

interface CalibrationSetInfo {
  setId: number;
  imagetyp: string;
  frames: ExportFrame[];
  frameCount: number;
  // Sub-calibrations (recursive)
  darkFlat: CalibrationSetInfo | null;
  dark: CalibrationSetInfo | null;
  bias: CalibrationSetInfo | null;
  matchScore: number | null;
  warnings: string[];
}

interface MasterCreationPlan {
  masters: MasterInfo[];
  masterPaths: Record<number, string>;
}

interface MasterInfo {
  setId: number;
  masterType: string;
  outputName: string;
  sourceFrames: ExportFrame[];
  dependsOn: number[];
  applyBias: number | null;
  applyDark: number | null;
}
```

### 8.2 get_calibration_route (NEW)

**Input**: `frame_set_id: i64`

**Output**:
```typescript
interface CalibrationRoute {
  groups: CalibrationRouteGroup[];
  scriptPreview: SirilScriptPreview[];
  summary: CalibrationRouteSummary;
}
```

## 9. Error Handling

### 9.1 Missing BAYERPAT

If a frame has no `bayerpat` value in the database:
1. Attempt to re-read from the FITS file
2. If file is inaccessible, assume Mono (conservative default)
3. Log warning

### 9.2 Mixed Camera Types Warning

If frames with the same filter have mixed camera types:
1. Split into separate export groups
2. Display warning in UI
3. Continue processing

### 9.3 Missing Calibrations

If an export group is missing calibrations:
1. Mark affected steps as "missing" in CalibrationRoute
2. Display prominent warning
3. Generate script with commented-out calibration steps
4. Allow export to proceed (user may have masters already)

## 10. Performance Considerations

### 10.1 Lazy BAYERPAT Loading

- Only query BAYERPAT when preparing export
- Don't load during initial scan (optional optimization)
- Cache in memory during export session

### 10.2 Parallel Script Execution

- Run independent calibration scripts in parallel
- Only serialize cross-dependent operations
- Use Siril's built-in parallelization

## 11. Testing Plan

### 11.1 Unit Tests

1. `CameraType::from_bayerpat()` - various patterns
2. Export group creation logic
3. Calibration chain building
4. Script generation correctness

### 11.2 Integration Tests

1. Full export workflow with mock files
2. Database migration for existing data
3. UI component rendering

### 11.3 Manual Testing

1. Export real FITS files from OSC camera
2. Export real FITS files from Mono camera
3. Mixed camera type frame set
4. Missing calibration scenarios

## 12. Backward Compatibility

### 12.1 Database Migration

- New `bayerpat` column is nullable
- Existing frames will have NULL bayerpat
- These are treated as Mono (conservative default)
- Users can re-scan to populate BAYERPAT

### 12.2 API Compatibility

- Old `FilterExportGroup` replaced with `ExportGroup`
- Frontend must be updated simultaneously
- No gradual migration path needed (internal API)

## 13. Open Questions

1. **Q**: Should we support manual override of camera type?
   **A**: TBD - Could add a UI toggle if needed

2. **Q**: What if the same object was captured with different filters on different nights?
   **A**: They become separate export groups, each producing its own master

3. **Q**: Should we detect and warn about significantly different plate scales?
   **A**: Yes, add warning if focal length varies by >10% within a group

---

## Appendix A: FITS BAYERPAT Values

Common Bayer pattern values:
- `RGGB` - Most common
- `BGGR` - Some cameras
- `GRBG` - Rare
- `GBRG` - Rare

## Appendix B: Siril Command Reference

Key Siril commands used:
- `convert` - Convert files to Siril format
- `calibrate` - Apply calibration masters
- `preprocess` - Combined calibration (legacy)
- `register` - Align frames
- `stack` - Combine frames
- `-debayer` - Extract color from Bayer pattern
