# Calibration Linkage System - Implementation Plan

## Overview
This document tracks the implementation of the frame-to-calibration-set linkage system with recursive calibration chain finding.

## Requirements Summary
- **Recursive chain**: Light → Flat → Dark → Bias (complete dependency tree)
- **Matching**: Boolean (match/no-match, no quality scores)
- **UI**: Dedicated Calibration Manager page for manual linking
- **Uniqueness**: One calibration set per type per frame (enforced by UNIQUE constraint)

---

## Implementation Status

### ✅ Phase 1: Database & Backend Foundation (COMPLETED)

**Files Modified:**
- `src-tauri/src/db/schema.rs` - Added `frame_calibration_links` table with indexes
- `src-tauri/src/db/calibration_links.rs` - NEW FILE with 13 CRUD operations
- `src-tauri/src/db/mod.rs` - Added module export
- `src-tauri/src/models.rs` - Added `FrameCalibrationLink` and `CalibrationChain` structs

**Database Schema:**
```sql
CREATE TABLE frame_calibration_links (
    frame_id INTEGER NOT NULL,
    calibration_set_id INTEGER NOT NULL,
    calibration_type TEXT NOT NULL,  -- 'Bias', 'Dark', 'DarkFlat', 'Flat'
    is_auto INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    UNIQUE(frame_id, calibration_type),  -- Enforces one-per-type
    FOREIGN KEY (frame_id) REFERENCES frames(id) ON DELETE CASCADE,
    FOREIGN KEY (calibration_set_id) REFERENCES calibration_set(id) ON DELETE CASCADE
)
```

**Operations Available:**
- `create_link()` - INSERT OR REPLACE
- `delete_link()` - by frame + type
- `delete_all_links_for_frame()`
- `get_links_for_frame()`
- `get_links_for_frames()` - bulk
- `has_link_for_type()`
- `get_set_id_for_frame_and_type()`
- Statistics functions

---

### 🔄 Phase 2: Matching Algorithm (IN PROGRESS)

**Goal:** Implement tolerance-based calibration set finding

**File:** `src-tauri/src/calibration/mod.rs`

**Functions to Implement:**

#### 2.1. `find_matching_calibrations_for_frame()`
```rust
pub fn find_matching_calibrations_for_frame(
    conn: &Connection,
    frame: &Frame,
    calibration_type: &str,  // 'Bias', 'Dark', 'DarkFlat', 'Flat'
    settings: &SettingsManager,
) -> Result<Vec<CalibrationSetDetail>>
```

**Matching Logic:**

**For BIAS:**
- EXACT: instrume, gain (±tolerance), offset (±tolerance), binning
- NEAREST: ccd_temp (±tolerance), date_obs (±days)

**For DARK:**
- EXACT: instrume, gain (±tolerance), offset (±tolerance), binning
- NEAREST: exptime (±%), ccd_temp (±tolerance), date_obs (±days)

**For DARKFLAT:**
- Same as DARK

**For FLAT:**
- EXACT: instrume, gain (±tolerance), offset (±tolerance), binning, filter, focallen
- NEAREST: ccd_temp (±tolerance), date_obs (±days)

**Priority:** Prefer `is_master_library = 1` sets over regular sets

**SQL Pattern:**
```sql
SELECT * FROM calibration_set
WHERE instrume = ?
  AND imagetyp = ?
  AND gain BETWEEN (? - tolerance) AND (? + tolerance)
  AND offset BETWEEN (? - tolerance) AND (? + tolerance)
  -- ... more conditions
ORDER BY
  is_master_library DESC,  -- Masters first
  ABS(ccd_temp - ?) ASC,   -- Nearest temp
  ABS(julianday(date_start) - julianday(?)) ASC  -- Nearest date
LIMIT 10
```

#### 2.2. Settings Required

**File:** `src-tauri/src/settings/mod.rs`

Add to `Defaults`:
```rust
pub const CALIBRATION_TEMP_TOLERANCE: &str = "5.0";        // degrees C
pub const CALIBRATION_GAIN_TOLERANCE: &str = "5.0";        // gain units
pub const CALIBRATION_OFFSET_TOLERANCE: &str = "5.0";      // offset units
pub const CALIBRATION_DATE_PROXIMITY_DAYS: &str = "30";    // days
pub const CALIBRATION_EXPTIME_TOLERANCE_PCT: &str = "10";  // percentage
```

Add to `Keys`:
```rust
pub const CALIBRATION_TEMP_TOLERANCE: &str = "calibration.temp_tolerance";
pub const CALIBRATION_GAIN_TOLERANCE: &str = "calibration.gain_tolerance";
pub const CALIBRATION_OFFSET_TOLERANCE: &str = "calibration.offset_tolerance";
pub const CALIBRATION_DATE_PROXIMITY_DAYS: &str = "calibration.date_proximity_days";
pub const CALIBRATION_EXPTIME_TOLERANCE_PCT: &str = "calibration.exptime_tolerance_pct";
```

Add getter methods to `SettingsManager` impl.

---

### 🔄 Phase 3: Recursive Chain Logic (PENDING)

**Goal:** Find complete calibration dependency tree

**File:** `src-tauri/src/calibration/mod.rs`

#### 3.1. `find_complete_calibration_chain()`
```rust
pub fn find_complete_calibration_chain(
    conn: &Connection,
    frame: &Frame,
    settings: &SettingsManager,
) -> Result<CalibrationChain>
```

**Recursive Algorithm:**

```
1. For LIGHT frame:
   a. Find FLAT sets (match: focallen, instrume, gain, offset, filter; nearest: temp, date)
   b. Find DARK sets (match: instrume, gain, offset; nearest: exptime, temp, date)

2. For each FLAT set found:
   a. Create pseudo-frame from set metadata
   b. Find DARK or DARKFLAT sets for that flat (match: instrume, gain, offset; nearest: exptime to flat's exptime)
   c. For each DARK found → find BIAS (step 3)

3. For each DARK set found (from Light or from Flat):
   a. Create pseudo-frame from set metadata
   b. Find BIAS sets (match: instrume, gain, offset; nearest: temp, date)

4. Collect all results:
   - flat_sets: Vec<CalibrationSetDetail>
   - dark_sets: Vec<CalibrationSetDetail>
   - darkflat_sets: Vec<CalibrationSetDetail>
   - bias_sets: Vec<CalibrationSetDetail>
   - missing_types: Vec<String>  (e.g., ["Flat", "Bias"] if none found)
```

**Circular Dependency Prevention:**
- Track visited set IDs in a HashSet
- Don't recurse if set already visited

**Helper Function:**
```rust
fn calibration_set_to_pseudo_frame(set: &CalibrationSetDetail) -> Frame
```
Creates a Frame with metadata from CalibrationSetDetail for recursive matching.

#### 3.2. `apply_auto_calibration_chain()`
```rust
pub fn apply_auto_calibration_chain(
    conn: &Connection,
    frame: &Frame,
    settings: &SettingsManager,
) -> Result<i64>  // Returns count of links created
```

**Logic:**
1. Call `find_complete_calibration_chain()`
2. For each calibration type with results:
   - Take the first (best) match
   - Call `create_link(frame_id, set_id, type, is_auto=true)`
3. Return total links created

**Bulk Version for Frame Sets:**
```rust
pub fn apply_auto_calibrations_for_frame_set(
    conn: &Connection,
    frame_set_id: i64,
    settings: &SettingsManager,
) -> Result<(i64, i64)>  // (frames_processed, links_created)
```

---

### 🔄 Phase 4: Tauri Commands (PENDING)

**File:** `src-tauri/src/commands.rs`

**Commands to Add:**

#### 4.1. Find Calibration Chain
```rust
#[tauri::command]
async fn find_calibration_chain(
    frame_set_id: i64,
    state: State<'_, AppState>,
) -> Result<HashMap<i64, CalibrationChain>, String>
```
- Get all LIGHT frames in the frame set
- For each frame, call `find_complete_calibration_chain()`
- Return map of frame_id → CalibrationChain

#### 4.2. Apply Auto Calibrations
```rust
#[tauri::command]
async fn apply_auto_calibrations(
    frame_set_id: i64,
    state: State<'_, AppState>,
) -> Result<AutoCalibrationResult, String>

#[derive(Serialize)]
struct AutoCalibrationResult {
    frames_processed: i64,
    links_created: i64,
    missing_calibrations: HashMap<i64, Vec<String>>,  // frame_id → missing types
}
```

#### 4.3. Get Links for Frame
```rust
#[tauri::command]
async fn get_calibration_links(
    frame_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<FrameCalibrationLink>, String>
```

#### 4.4. Create Manual Link
```rust
#[tauri::command]
async fn create_calibration_link(
    frame_id: i64,
    calibration_set_id: i64,
    calibration_type: String,
    state: State<'_, AppState>,
) -> Result<(), String>
```
Validates types and calls `create_link(..., is_auto=false)`

#### 4.5. Remove Link
```rust
#[tauri::command]
async fn remove_calibration_link(
    frame_id: i64,
    calibration_type: String,
    state: State<'_, AppState>,
) -> Result<(), String>
```

#### 4.6. Get Available Sets for Manual Linking
```rust
#[tauri::command]
async fn get_linkable_calibration_sets(
    frame_id: i64,
    calibration_type: String,
    state: State<'_, AppState>,
) -> Result<Vec<CalibrationSetDetail>, String>
```
Calls `find_matching_calibrations_for_frame()` and returns sorted results

**Add to `invoke_handler!` in `lib.rs:**
```rust
.invoke_handler(tauri::generate_handler![
    // ... existing commands
    find_calibration_chain,
    apply_auto_calibrations,
    get_calibration_links,
    create_calibration_link,
    remove_calibration_link,
    get_linkable_calibration_sets,
])
```

---

### 🔄 Phase 5: TypeScript Models (PENDING)

**File:** `src/types/models.ts`

#### 5.1. Update ImageType Enum
```typescript
export enum ImageType {
  Light = "Light",
  Dark = "Dark",
  Flat = "Flat",
  Bias = "Bias",
  DarkFlat = "DarkFlat",
  MasterLight = "MasterLight",      // ADD
  MasterDark = "MasterDark",        // ADD
  MasterFlat = "MasterFlat",        // ADD
  MasterBias = "MasterBias",        // ADD
  MasterDarkFlat = "MasterDarkFlat", // ADD
}
```

#### 5.2. Add New Interfaces
```typescript
export interface FrameCalibrationLink {
  frame_id: number;
  calibration_set_id: number;
  calibration_type: string;
  is_auto: boolean;
  created_at: string;
}

export interface CalibrationChain {
  frame_id: number;
  flat_sets: CalibrationSetDetail[];
  dark_sets: CalibrationSetDetail[];
  bias_sets: CalibrationSetDetail[];
  darkflat_sets: CalibrationSetDetail[];
  missing_types: string[];
}

export interface AutoCalibrationResult {
  frames_processed: number;
  links_created: number;
  missing_calibrations: Record<number, string[]>;
}
```

---

### 🔄 Phase 6: UI Components (PENDING)

#### 6.1. Calibration Manager Page

**File:** `src/pages/CalibrationManager.tsx` (NEW)

**Features:**
- Frame set selector dropdown
- "Find All Calibrations" button (calls `apply_auto_calibrations`)
- Frame list table with columns:
  - Filename, Object, Filter, Exptime, Type
  - Calibration status badges (✓ Flat, ✓ Dark, ✗ Bias, etc.)
  - "View Chain" button → opens CalibrationChainModal
  - "Edit Links" button → opens CalibrationSetPicker
- Summary stats: "X/Y frames fully calibrated"
- Filter: "Show only missing calibrations"

**State Management:**
```typescript
const [frameSetId, setFrameSetId] = useState<number | null>(null);
const [frames, setFrames] = useState<Frame[]>([]);
const [links, setLinks] = useState<Map<number, FrameCalibrationLink[]>>(new Map());
const [loading, setLoading] = useState(false);
```

#### 6.2. Calibration Chain Modal

**File:** `src/components/CalibrationChainModal.tsx` (NEW)

**Features:**
- Tree visualization using indentation/icons
- Example structure:
  ```
  🌟 Light Frame: NGC7000_Ha_001.fits
    └─ 📐 Flat Set #42 (15 frames, 2024-10)
       └─ 🌑 Dark Set #38 (20 frames, exptime=1s)
          └─ ⚫ Bias Set #12 (50 frames)
    └─ 🌑 Dark Set #40 (25 frames, exptime=300s)
       └─ ⚫ Bias Set #12 (50 frames)
  ```
- Color coding: Auto (blue badge), Manual (yellow badge)
- "Unlink" button per node
- "Find Alternative" if missing
- Close button

#### 6.3. Calibration Set Picker Modal

**File:** `src/components/CalibrationSetPicker.tsx` (NEW)

**Features:**
- Calibration type selector (Bias, Dark, DarkFlat, Flat)
- Table of available sets:
  - ID, Type, Frame Count, Date Range, Temp Range
  - Match quality indicators (exact match params highlighted)
  - Distance metrics (temp diff, date gap, etc.)
- Sort by: Best Match, Date, Temperature
- "Link" button to create association
- Cancel button

#### 6.4. Navigation Update

**File:** `src/components/Layout.tsx`

Add navigation link:
```tsx
<Link to="/calibration" className="...">
  <Wand2 size={20} />
  Calibration
</Link>
```

#### 6.5. Frame Set Detail Enhancement

**File:** `src/pages/FrameSetDetail.tsx`

Add calibration indicator in header (around line 300):
```tsx
<div className="flex items-center gap-2">
  <Badge variant="outline">
    {calibratedCount}/{totalFrames} Calibrated
  </Badge>
  <button onClick={() => navigate('/calibration', { state: { frameSetId } })}>
    Manage Calibrations
  </button>
</div>
```

#### 6.6. Router Update

**File:** `src/main.tsx` or router config

Add route:
```tsx
<Route path="/calibration" element={<CalibrationManager />} />
```

---

### 🔄 Phase 7: Testing & Polish (PENDING)

#### 7.1. Backend Tests

**File:** `src-tauri/src/calibration/mod.rs` (add #[cfg(test)] module)

Test cases:
- Matching with exact parameters
- Matching with tolerance ranges
- Preference for master sets
- Recursive chain finding
- Circular dependency prevention
- Missing calibrations handling

#### 7.2. Integration Tests

- Create dark library → verify sets exist
- Find calibrations for light frame → verify results
- Apply auto calibrations → verify links created
- Manual link creation → verify UNIQUE constraint
- Delete calibration set → verify CASCADE delete of links

#### 7.3. UI Polish

- Loading states for all async operations
- Error handling with user-friendly messages
- Success notifications ("12 links created")
- Empty states ("No calibrations found")
- Responsive design for tables
- Keyboard shortcuts (Esc to close modals)

#### 7.4. Documentation

- Update README with calibration workflow
- Add screenshots to docs
- Update CLAUDE.md with new patterns

---

## Next Steps

**Current Position:** Phase 1 completed and tested ✅

**Continue with:**
1. Phase 2: Implement matching algorithm
2. Phase 3: Implement recursive chain logic
3. Phase 4: Add Tauri commands
4. Phase 5: Update TypeScript types
5. Phase 6: Build UI components
6. Phase 7: Test and polish

---

## Notes

- Database recreated successfully with new schema
- Compilation working with 0 errors
- All Phase 1 CRUD operations tested and functional
- Ready to proceed with matching algorithm implementation
