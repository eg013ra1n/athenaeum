# Automated Calibration Finder - Implementation Plan

## Overview
Implement an automated system to find and link calibration frames (Flats, Darks, Bias, DarkFlats) to Light frames in frame sets, with hierarchical matching (Light→Flat→Dark/Bias, Light→Dark→Bias).

## Database Schema Design

### New Table: `calibration_set_to_frames`

Stores the relationships between frames/calibration sets and their required calibration sets.

```sql
CREATE TABLE calibration_set_to_frames (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id INTEGER NOT NULL,           -- frame.id OR calibration_set.id
    source_type TEXT NOT NULL,             -- 'frame' or 'calibration_set'
    calibration_set_id INTEGER NOT NULL,   -- links to calibration_set.id
    calibration_type TEXT NOT NULL,        -- 'Dark', 'Flat', 'Bias', 'DarkFlat'
    matched_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    match_score REAL,                      -- 0.0-1.0 confidence
    date_warning INTEGER DEFAULT 0,        -- 1 if date exceeds threshold
    temp_warning INTEGER DEFAULT 0,        -- 1 if temp outside tolerance
    FOREIGN KEY (calibration_set_id) REFERENCES calibration_set(id) ON DELETE CASCADE,
    UNIQUE(source_id, source_type, calibration_type)
);

CREATE INDEX idx_calib_source ON calibration_set_to_frames(source_id, source_type);
CREATE INDEX idx_calib_set ON calibration_set_to_frames(calibration_set_id);
```

**Relationship Examples:**
- Light frame #123 → Flat Set #5: `(123, 'frame', 5, 'Flat', ...)`
- Light frame #123 → Dark Set #10: `(123, 'frame', 10, 'Dark', ...)`
- Flat Set #5 → Dark Set #10: `(5, 'calibration_set', 10, 'Dark', ...)`
- Dark Set #10 → Bias Set #3: `(10, 'calibration_set', 3, 'Bias', ...)`

### Existing Tables Used

**calibration_set** - stores grouped calibration frames with metadata
**calibration_set_frames** - links individual calibration frames to sets (frame belongs to set)
**frames** - contains all frame metadata for matching

---

## Matching Rules & Tolerances

### For Flats → Lights
**Exact Match Required:**
- gain
- offset
- binning
- filter
- focallen
- instrume

**Tolerance Match:**
- Temperature: ±2°C (configurable)
- Date: No limit, but warning if >30 days old

**Scoring Priority:**
1. Nearest date/time
2. Nearest temperature

### For Darks → Lights/Flats
**Exact Match Required:**
- gain
- offset
- binning
- instrume
- exptime

**Tolerance Match:**
- Temperature: ±2°C (configurable)
- Date: No limit, but warning if >365 days old

**Scoring Priority:**
1. Nearest date/time
2. Nearest temperature

### For Bias → Darks/Flats
**Exact Match Required:**
- gain
- offset
- binning
- instrume

**Tolerance Match:**
- Temperature: ±2°C (configurable)
- Date: No limit

**Scoring Priority:**
1. Nearest date/time
2. Nearest temperature

---

## UI Design

### Objects Page - Frame Set View

**Layout:**
```
[Frame Set Header: "M31 - Andromeda"]
┌─────────────────────────────────────────────────┐
│ [View Sky] [Find Calibration Data] [Export]... │ ← Toolbar
├─────────────────────────────────────────────────┤
│ Frame List:                                      │
│ ┌─────────────────────────────────────────────┐ │
│ │ frame_001.fits ✓Flats ✓Darks ⚠Bias    [▼]  │ │ ← Badges + Expand
│ └─────────────────────────────────────────────┘ │
│ ┌─────────────────────────────────────────────┐ │
│ │ frame_002.fits ✓Flats ✗Darks           [▼]  │ │
│ └─────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────┘
```

**Badge System:**
- ✓ Green = Calibration linked with no warnings
- ⚠ Yellow = Calibration linked with warnings (date/temp)
- ✗ Red = Calibration not found

**Expandable Row Details:**
- Show linked calibration set summary (name, date range, frame count, temp range)
- Click badge → Navigate to Equipment tab with sets highlighted

### Equipment Tab

**Enhancements:**
- List all calibration sets with usage statistics (how many frames use this set)
- Click on usage count to see which frames use the set
- Highlight/blink sets when navigated from Objects page
- Filter to show only sets in use vs. unused

---

## Implementation Phases

### Phase 1: Database Schema & Models (Day 1)
**Tasks:**
1. Add `calibration_set_to_frames` table to schema.rs with migration
2. Create Rust models in models.rs:
   - `CalibrationLink`
   - `CalibrationMatchResult`
   - `CalibrationHierarchy`
   - `FrameCalibrationStatus`
3. Create TypeScript interfaces in types/models.ts
4. Create database operations module: `src-tauri/src/db/calibration_links.rs`

**Tests:**
- Table creation and constraints
- Foreign key cascade behavior
- Unique constraint on (source_id, source_type, calibration_type)

**Deliverables:**
- Migration script working
- Models defined in Rust and TypeScript
- Database operations module with CRUD functions

---

### Phase 2: Matching Algorithm Core (Day 2-3)
**Module:** `src-tauri/src/calibration/finder.rs`

**Tasks:**
1. Implement exact parameter matching functions:
   - `matches_exact_parameters()` - checks gain, offset, binning, etc.
   - `matches_flat_parameters()` - specific rules for flats
   - `matches_dark_parameters()` - specific rules for darks
   - `matches_bias_parameters()` - specific rules for bias

2. Implement tolerance matching:
   - `check_temperature_tolerance()` - ±2°C configurable
   - `calculate_date_proximity()` - returns score and warnings

3. Implement scoring system:
   - `score_calibration_match()` - returns 0.0-1.0 confidence
   - `rank_calibration_candidates()` - sorts by date then temp

**Tests:**
- Unit tests for each matching function
- Edge cases: missing parameters, null values, extreme values
- Tolerance boundary testing

**Deliverables:**
- Matching algorithm with configurable tolerances
- Scoring and ranking functions
- Comprehensive unit test suite

---

### Phase 3: Hierarchical Calibration Builder (Day 4-5)
**Module:** `src-tauri/src/calibration/hierarchy.rs`

**Tasks:**
1. Implement calibration finding functions:
   - `find_flat_sets_for_light_frame(conn, frame_id)` → Vec<CalibrationSet>
   - `find_dark_sets_for_light_frame(conn, frame_id)` → Vec<CalibrationSet>
   - `find_dark_sets_for_flat_set(conn, set_id)` → Vec<CalibrationSet>
   - `find_bias_sets_for_flat_set(conn, set_id)` → Vec<CalibrationSet>
   - `find_bias_sets_for_dark_set(conn, set_id)` → Vec<CalibrationSet>

2. Implement hierarchy builder:
   - `build_complete_hierarchy(conn, frame_id)` → CalibrationHierarchy
   - Handle partial matches (some calibration missing)
   - Generate suggestions for missing calibration

**Hierarchy Logic:**
- For each light frame: find Flat sets + Dark sets
- For each Flat set: find Dark sets OR Bias sets (fallback)
- For each Dark set: find Bias sets

**Tests:**
- Integration tests with sample frame data
- Test partial matching scenarios
- Test fallback logic (Bias when Dark not found)

**Deliverables:**
- Complete hierarchy building functions
- Partial matching support
- Missing calibration suggestions

---

### Phase 4: Set Creation from Individual Frames (Day 6)
**Module:** `src-tauri/src/calibration/auto_create.rs`

**Tasks:**
1. Implement frame search and grouping:
   - `find_matching_individual_frames()` - search frames table
   - `group_frames_into_set()` - cluster similar frames
   - `check_for_duplicate_set()` - prevent duplicates

2. Implement set suggestion:
   - `suggest_new_calibration_set()` - returns proposed set
   - User approval workflow

**Tests:**
- Deduplication testing
- Frame grouping accuracy
- Duplicate set prevention

**Deliverables:**
- Auto-creation logic for missing sets
- Deduplication verification
- Suggestion generation

---

### Phase 5: Frame Set Processor (Day 7)
**Module:** `src-tauri/src/calibration/frame_set_processor.rs`

**Tasks:**
1. Implement frame set processing:
   - `process_frame_set(conn, frame_set_id)` - main entry point
   - Get all light frames via imaging_nights→sessions→session_members
   - For each light frame: build hierarchy and store links
   - Batch processing optimization
   - Progress reporting

2. Implement statistics tracking:
   - Count frames processed
   - Count calibration sets linked
   - Count warnings generated
   - Track missing calibration

**Tests:**
- Performance test with large frame sets (100+ frames)
- Progress reporting accuracy
- Statistics calculation

**Deliverables:**
- Frame set processor with progress tracking
- Performance optimizations
- Comprehensive result reporting

---

### Phase 6: Tauri Commands (Day 8)
**File:** `src-tauri/src/commands.rs`

**Tasks:**
1. Implement Tauri commands:
```rust
#[tauri::command]
pub async fn find_calibration_for_frame_set(
    state: State<'_, AppState>,
    frame_set_id: i64
) -> Result<CalibrationMatchResult, String>

#[tauri::command]
pub async fn get_calibration_status(
    state: State<'_, AppState>,
    frame_set_id: i64
) -> Result<Vec<FrameCalibrationStatus>, String>

#[tauri::command]
pub async fn get_frame_calibration_hierarchy(
    state: State<'_, AppState>,
    frame_id: i64
) -> Result<CalibrationHierarchy, String>

#[tauri::command]
pub async fn clear_calibration_links(
    state: State<'_, AppState>,
    frame_set_id: i64
) -> Result<(), String>

#[tauri::command]
pub async fn update_calibration_tolerance(
    state: State<'_, AppState>,
    temp_delta: f64,
    flat_date_warning_days: i64,
    dark_date_warning_days: i64
) -> Result<(), String>
```

2. Create database operations: `src-tauri/src/db/calibration_links.rs`
```rust
pub fn insert_calibration_link(conn: &Connection, link: &CalibrationLink) -> Result<()>
pub fn get_links_for_frame(conn: &Connection, frame_id: i64) -> Result<Vec<CalibrationLink>>
pub fn get_links_for_calibration_set(conn: &Connection, set_id: i64) -> Result<Vec<CalibrationLink>>
pub fn delete_links_for_frame_set(conn: &Connection, frame_set_id: i64) -> Result<()>
pub fn get_calibration_statistics(conn: &Connection, frame_set_id: i64) -> Result<CalibrationStats>
```

**Tests:**
- Command execution tests
- Error handling tests
- Database operation tests

**Deliverables:**
- All Tauri commands implemented and registered
- Database operations module complete
- Error handling comprehensive

---

### Phase 7: Frontend - Objects Page (Day 9-10)
**File:** `src/pages/Objects.tsx` + new components

**Tasks:**
1. Update Objects.tsx:
   - Add "Find Calibration Data" button in toolbar (when viewing frame set)
   - Integrate calibration status display in frame list
   - Add progress modal during processing

2. Create new components:
   - `src/components/CalibrationFinderButton.tsx` - toolbar button with progress modal
   - `src/components/CalibrationStatusBadges.tsx` - show ✓/⚠/✗ badges for Flats/Darks/Bias
   - `src/components/FrameCalibrationSummary.tsx` - expandable row content
   - `src/components/CalibrationProcessModal.tsx` - progress during matching

3. Implement navigation:
   - Click on badge → Navigate to Equipment tab with `?highlight_set={id}` param

**Tests:**
- Component rendering tests
- Button interaction tests
- Navigation flow tests

**Deliverables:**
- Objects page with calibration finder integration
- Badge display system
- Navigation to Equipment tab

---

### Phase 8: Frontend - Equipment Tab Enhancement (Day 11)
**File:** `src/pages/Equipment.tsx` + new components

**Tasks:**
1. Update Equipment.tsx:
   - Show usage statistics for each calibration set
   - Implement highlighting when navigated from Objects page
   - Add filter for "in use" vs "unused" sets

2. Create new components:
   - `src/components/CalibrationSetUsageList.tsx` - shows frames using a set
   - `src/components/BlinkHighlight.tsx` - visual highlight effect

3. Implement URL parameter handling:
   - Parse `?highlight_set={id}` from URL
   - Auto-scroll to highlighted set
   - Apply blink/highlight animation

**Tests:**
- URL parameter parsing
- Highlighting behavior
- Usage statistics accuracy

**Deliverables:**
- Equipment tab with usage tracking
- Highlight/blink functionality
- Navigation integration complete

---

### Phase 9: Settings & Configuration (Day 12)
**File:** `src/pages/Settings.tsx`

**Tasks:**
1. Add calibration settings section:
   - Temperature tolerance (default: 2.0°C)
   - Flat date warning threshold (default: 30 days)
   - Dark date warning threshold (default: 365 days)

2. Implement settings persistence:
   - Store in settings table
   - Load on app startup
   - Apply to matching algorithm

**Tests:**
- Settings persistence
- Validation tests (e.g., negative values)
- Default value handling

**Deliverables:**
- Calibration settings UI
- Settings persistence working
- Integration with matching algorithm

---

### Phase 10: Documentation (Day 13)
**Location:** `docs/calibration/`

**Tasks:**
1. Create comprehensive documentation:
   - `README.md` - Overview and quick start
   - `ALGORITHM.md` - Matching algorithm details with examples
   - `HIERARCHY.md` - Calibration hierarchy explanation with diagrams
   - `DATABASE.md` - Schema documentation for calibration_set_to_frames
   - `API.md` - Tauri commands reference
   - `USER_GUIDE.md` - End-user workflow guide
   - `TESTING.md` - Test coverage and strategy

**Deliverables:**
- Complete documentation suite
- Code examples
- Workflow diagrams

---

### Phase 11: Testing & Polish (Day 14)
**Tasks:**
1. End-to-end workflow testing
2. Performance optimization for large catalogs
3. Error message improvements
4. UI/UX polish based on testing
5. Code review and refactoring

**Deliverables:**
- Comprehensive E2E test suite
- Performance benchmarks
- Polished user experience

---

## Success Criteria

✓ `calibration_set_to_frames` table stores all relationships correctly
✓ Light frames automatically matched to Flat/Dark sets
✓ Flat sets linked to Dark/Bias sets
✓ Dark sets linked to Bias sets
✓ Partial linking with suggestions when calibration missing
✓ Date/temperature warnings displayed correctly
✓ Badges show calibration status in Objects page
✓ Click badge → navigate to Equipment tab with sets highlighted
✓ Complete documentation in docs/calibration/
✓ All tests passing
✓ Performance acceptable for large datasets (1000+ frames)

---

## Technical Notes

### Calibration Hierarchy Example

```
Light Frame #123 (Ha, 300s, gain=100)
├─ Flat Set #5 (Ha, gain=100, offset=10, 20 frames)
│  ├─ Dark Set #10 (300s, gain=100, 15 frames) ✓
│  └─ Bias Set #3 (gain=100, 50 frames) ✓ (fallback if no darks)
└─ Dark Set #8 (300s, gain=100, 20 frames) ✓
   └─ Bias Set #3 (gain=100, 50 frames) ✓
```

### Database Relationships

```
frames (id=123, imagetyp=Light)
  ↓ via calibration_set_to_frames
calibration_set (id=5, imagetyp=Flat)
  ↓ via calibration_set_frames
frames (id=201, 202, 203..., imagetyp=Flat)

calibration_set (id=5, imagetyp=Flat)
  ↓ via calibration_set_to_frames (source_id=5, source_type='calibration_set')
calibration_set (id=10, imagetyp=Dark)
  ↓ via calibration_set_frames
frames (id=301, 302, 303..., imagetyp=Dark)
```

### Settings Keys

- `calibration.temp_tolerance` → 2.0 (°C)
- `calibration.flat_date_warning` → 30 (days)
- `calibration.dark_date_warning` → 365 (days)
