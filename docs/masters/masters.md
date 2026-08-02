# Investigation: Master Calibration Files in Athenaeum

## Overview

This document details how master calibration files are handled in the system, covering parsing, linking, and selection logic.

---

## 1. How the App Parses Data from Master Files

### Detection System (Two-Priority)

**File:** `src-tauri/src/fits_parser/mod.rs`

**Priority 1 - IMAGETYP Header (lines 310-315, 535-539):**
```rust
let imagetyp = imagetyp_str.and_then(|s| ImageType::from_str(&s));
let is_master = imagetyp.as_ref().map(|t| t.is_master()).unwrap_or(false);
```

**Priority 2 - Filename Patterns (lines 317-328, 541-552):**
```rust
let filename_is_master = if !is_master {
    let filename_lower = filename.to_lowercase();
    filename_lower.contains("master") ||
    filename_lower.contains("_calibrated_") ||
    filename_lower.contains("-calibrated-")
} else { false };
```

### Supported IMAGETYP Values

**File:** `src-tauri/src/models.rs` (lines 79-94)

| IMAGETYP Value | Parsed As |
|----------------|-----------|
| `MASTER LIGHT` / `MASTERLIGHT` | `MasterLight` |
| `MASTER DARK` / `MASTERDARK` | `MasterDark` |
| `MASTER FLAT` / `MASTERFLAT` | `MasterFlat` |
| `MASTER BIAS` / `MASTERBIAS` | `MasterBias` |
| `MASTER DARK FLAT` / `MASTERDARKFLAT` | `MasterDarkFlat` |

### ImageType Enum

**File:** `src-tauri/src/models.rs` (lines 64-102)

```rust
pub enum ImageType {
    Light, Dark, Flat, Bias, DarkFlat,
    MasterLight, MasterDark, MasterFlat, MasterBias, MasterDarkFlat,
}

impl ImageType {
    pub fn is_master(&self) -> bool {
        matches!(self, Self::MasterLight | Self::MasterDark |
                 Self::MasterFlat | Self::MasterBias | Self::MasterDarkFlat)
    }
}
```

### Database Storage

**Frames table** (`src-tauri/src/db/schema.rs` line 54):
- `is_master INTEGER NOT NULL DEFAULT 0` - marks individual master frames
- Indexed: `idx_frames_is_master`

**Calibration set table** (line 114):
- `is_master_library INTEGER NOT NULL DEFAULT 0` - marks master calibration sets
- Indexed: `idx_calibration_set_is_master`

### Master Set Creation

**File:** `src-tauri/src/calibration/scan_integration.rs` (lines 801-879)

Each master frame becomes its own calibration set (1:1 mapping):
```rust
conn.execute(
    "INSERT INTO calibration_set (..., frame_count, ..., is_master_library)
     VALUES (..., 1, ..., 1)",  // frame_count=1, is_master_library=1
    ...
)?;
```

---

## 2. How Masters Are Linked and Included in Manual Linking

### Linking Database Schema

**File:** `src-tauri/src/db/schema.rs` (lines 258-273)

```sql
CREATE TABLE calibration_set_to_frames (
    id INTEGER PRIMARY KEY,
    source_id INTEGER NOT NULL,
    source_type TEXT CHECK(source_type IN ('frame', 'calibration_set')),
    calibration_set_id INTEGER NOT NULL,
    calibration_type TEXT CHECK(calibration_type IN ('Dark', 'Flat', 'Bias', 'DarkFlat')),
    is_manual_override INTEGER NOT NULL DEFAULT 0,
    match_score REAL,
    date_warning INTEGER DEFAULT 0,
    temp_warning INTEGER DEFAULT 0,
    UNIQUE(source_id, source_type, calibration_type)
);
```

**Key fields:**
- `source_type = 'frame'`: Link from light frame to calibration
- `source_type = 'calibration_set'`: Link from calibration set to sub-calibration
- `is_manual_override = 1`: Manual assignment (preserved, auto-find skips)
- `is_manual_override = 0`: Auto-matched (can be overwritten)

### Manual Linking Commands

**File:** `src-tauri/src/commands/calibration.rs`

| Command | Lines | Purpose |
|---------|-------|---------|
| `manual_assign_calibration` | 874-929 | Assign calibration to light frames |
| `clear_manual_calibration_override` | 932-984 | Remove manual assignments |
| `manual_assign_subcalibration` | 1834-1877 | Assign sub-calibration to calibration sets |

**Manual assignment logic (lines 874-929):**
```rust
pub async fn manual_assign_calibration(
    frame_ids: Vec<i64>,
    calibration_set_id: i64,      // Can be master or regular set
    calibration_type: String,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    // Creates link with:
    // - is_manual_override = true
    // - match_score = 1.0 (perfect score for manual)
}
```

### Auto-Find Respects Manual Overrides

**File:** `src-tauri/src/db/calibration_links.rs` (lines 24-38)

```rust
// Skip if this is an auto-assignment and a manual override exists
if !is_manual && existing_is_manual {
    return Ok(None);  // Don't overwrite manual assignment
}
```

### Frontend Manual Linking

**File:** `src/components/ManualCalibrationModal.tsx`

1. Loads frame parameters: `get_light_frame_parameters` (lines 87-99)
2. Loads candidates (includes masters): `get_calibration_sets_for_manual_selection` (lines 100-111)
3. User selects from list containing both masters and regular sets
4. Applies via `manual_assign_calibration`

---

## 3. How System Chooses Master Instead of Calibration Set

### Master Preference Configuration

**File:** `src-tauri/src/calibration/config.rs` (lines 239-252)

```rust
pub enum MasterPreference {
    PreferMaster,    // Prioritize master calibration sets (default)
    PreferFrameset,  // Prioritize regular frame sets
    NoPreference,    // Use score alone
}
```

**Stored per calibration type** (lines 338-339, 474-478):
```rust
pub master_preferences: HashMap<String, MasterPreference>,

// Defaults:
config.master_preferences.insert("flat".to_string(), MasterPreference::PreferMaster);
config.master_preferences.insert("dark".to_string(), MasterPreference::PreferMaster);
config.master_preferences.insert("bias".to_string(), MasterPreference::PreferMaster);
config.master_preferences.insert("darkflat".to_string(), MasterPreference::PreferMaster);
```

### Selection Algorithm

**File:** `src-tauri/src/calibration/configurable_matcher.rs`

**Step 1 - Query Based on Preference (lines 279-302):**
```rust
let master_pref = config.get_master_preference(calibration_type);

if matches!(master_pref, MasterPreference::PreferMaster) {
    // Query includes BOTH master and regular sets
    "SELECT ... WHERE imagetyp IN ('Flat', 'MasterFlat') ..."
} else {
    // Query excludes masters entirely
    "SELECT ... WHERE imagetyp = 'Flat' AND is_master_library = 0 ..."
}
```

**Step 2 - Score All Candidates Identically (lines 446-477):**
```rust
pub fn score_match(
    date_diff_days: Option<i64>,
    temp_diff: Option<f64>,
    exptime_diff: Option<f64>,
    config: &ScoringConfig,
) -> f64 {
    // Same scoring for masters and regular sets
    // Components: date proximity, temperature match, exposure time match
}
```

**Step 3 - Reorder by Preference (lines 480-513):**
```rust
fn apply_master_preference(candidates, preference) -> Vec<CalibrationCandidate> {
    match preference {
        NoPreference => candidates,  // Keep score order
        PreferMaster => {
            // Put masters first, then regular sets
            masters.extend(framesets);
            masters
        }
        PreferFrameset => {
            // Put regular sets first, then masters
            framesets.extend(masters);
            framesets
        }
    }
}
```

### Master Sets Skip Sub-Calibration

**File:** `src-tauri/src/calibration/hierarchy.rs` (lines 151-169, 215-233)

```rust
let is_master_library = conn.query_row(
    "SELECT is_master_library FROM calibration_set WHERE id = ?1",
    [set_id], |row| Ok(row.get::<_, i32>(0).unwrap_or(0) == 1),
)?;

if is_master_library {
    // Master sets are already calibrated - no sub-calibration needed
    return Ok(Vec::new());
}
```

### UI Configuration

**File:** `src/components/calibration/CalibrationMatchingConfig.tsx` (lines 492-542)

Master Preferences section with dropdowns for flat, dark, bias, darkflat:
- No Preference (default)
- Prefer Master
- Prefer Frameset

---

## Summary Flow

```
1. PARSING
   FITS/XISF file → Check IMAGETYP header → Check filename patterns
   → Set frame.is_master = true if master detected
   → Create calibration_set with is_master_library = 1

2. MANUAL LINKING
   User selects frames → Modal loads all candidates (masters + regular)
   → User picks calibration set → Link created with is_manual_override = true
   → Auto-find respects manual overrides

3. AUTOMATIC SELECTION
   Load preference from config → Query candidates based on preference
   → Score all candidates equally → Reorder by preference
   → Best candidate selected → Master sets skip sub-calibration
```

---

## Key Files Reference

| File | Purpose |
|------|---------|
| `src-tauri/src/fits_parser/mod.rs` | Master detection in FITS/XISF |
| `src-tauri/src/models.rs:64-102` | ImageType enum with master types |
| `src-tauri/src/db/schema.rs` | is_master, is_master_library columns |
| `src-tauri/src/calibration/config.rs:239-252` | MasterPreference enum |
| `src-tauri/src/calibration/configurable_matcher.rs` | Selection algorithm |
| `src-tauri/src/calibration/hierarchy.rs` | Sub-calibration skip logic |
| `src-tauri/src/commands/calibration.rs:874-929` | Manual assignment command |
| `src-tauri/src/db/calibration_links.rs` | Link storage operations |
| `src/components/ManualCalibrationModal.tsx` | Manual linking UI |
| `src/components/calibration/CalibrationMatchingConfig.tsx` | Master preferences UI |
