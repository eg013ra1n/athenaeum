# Calibration Finder - Task Checklist

## Phase 1: Database Schema & Models ✅

### Database Schema
- [x] Add `calibration_set_to_frames` table to schema.rs
- [x] Add migration logic for existing databases
- [x] Add indexes for performance
- [x] Test table creation and constraints

### Rust Models
- [x] Create `CalibrationLink` struct in models.rs
- [x] Create `CalibrationMatchResult` struct
- [x] Create `CalibrationHierarchy` struct
- [x] Create `FrameCalibrationStatus` struct
- [x] Create `CalibrationStats` struct

### TypeScript Interfaces
- [x] Create CalibrationLink interface
- [x] Create CalibrationMatchResult interface
- [x] Create CalibrationHierarchy interface
- [x] Create FrameCalibrationStatus interface

### Database Operations Module
- [x] Create `src-tauri/src/db/calibration_links.rs`
- [x] Implement `insert_calibration_link()`
- [x] Implement `get_links_for_frame()`
- [x] Implement `get_links_for_calibration_set()`
- [x] Implement `delete_links_for_frame_set()`
- [x] Implement `get_calibration_statistics()`

### Tests
- [x] Test table creation
- [x] Test foreign key constraints
- [x] Test cascade deletes
- [x] Test unique constraints
- [x] Test CRUD operations

**Status: COMPLETE ✅** - See `PHASE1_COMPLETE.md`

---

## Phase 2: Matching Algorithm Core ✅

### Parameter Matching Functions
- [x] Implement `matches_gain()`
- [x] Implement `matches_offset()`
- [x] Implement `matches_exptime()`
- [x] Implement `matches_focallen()`
- [x] Implement `matches_flat_parameters()`
- [x] Implement `matches_dark_parameters()`
- [x] Implement `matches_bias_parameters()`

### Tolerance Matching
- [x] Implement `check_temperature_tolerance()`
- [x] Implement `calculate_date_diff_days()`
- [x] Implement date warning logic

### Scoring System
- [x] Implement `score_calibration_match()`
- [x] Implement `rank_calibration_candidates()`
- [x] Implement priority: date → temperature

### Calibration Finders
- [x] Implement `find_flat_sets_for_light_frame()`
- [x] Implement `find_dark_sets_for_light_frame()`
- [x] Implement `find_bias_sets_for_frame()`

### Tests
- [x] Unit test exact parameter matching
- [x] Unit test tolerance matching
- [x] Unit test scoring system
- [x] Test edge cases (null values, missing data)
- [x] Test tolerance boundaries

**Status: COMPLETE ✅** - See `PHASE2_COMPLETE.md`

---

## Phase 3: Hierarchical Calibration Builder ✅

### Finding Functions
- [x] Implement `find_calibration_for_flat_set()` (Dark → Bias fallback)
- [x] Implement `find_calibration_for_dark_set()`
- [x] Reuse `find_flat_sets_for_light_frame()` from Phase 2
- [x] Reuse `find_dark_sets_for_light_frame()` from Phase 2
- [x] Reuse `find_bias_sets_for_frame()` from Phase 2

### Hierarchy Builder
- [x] Implement `build_complete_hierarchy()`
- [x] Handle partial matches
- [x] Generate suggestions for missing calibration
- [x] Implement fallback logic (Bias when Dark not found)
- [x] Implement `store_calibration_hierarchy()`

### Helper Functions
- [x] Implement `get_frame_by_id()`
- [x] Implement `get_calibration_set_by_id()`

### Tests
- [x] Test hierarchy structure
- [x] Test partial matching
- [x] Test fallback scenarios
- [x] Test warning accumulation

**Status: COMPLETE ✅** - See `PHASE3_COMPLETE.md`

---

## Phase 4: Set Creation from Individual Frames ✅

### Frame Search & Grouping
- [x] Implement `find_matching_individual_frames()`
- [x] Implement `group_frames_into_set()`
- [x] Implement `check_for_duplicate_set()`
- [x] Implement `create_calibration_set()`

### Set Suggestion
- [x] Create `SuggestedCalibrationSet` struct
- [x] Implement frame clustering by parameters

### Tests
- [x] Test suggested set structure
- [x] Test empty frame grouping

**Status: COMPLETE ✅** - See `PHASE4_COMPLETE.md`

---

## Phase 5: Frame Set Processor ✅

### Frame Set Processing
- [x] Implement `get_light_frames_from_frame_set()`
- [x] Implement `process_frame_set()`
- [x] Implement `process_frame_set_with_progress()` with callback
- [x] Build hierarchy for each light frame
- [x] Store links in database
- [x] Progress reporting with percentage calculation
- [x] Implement `clear_calibration_links_for_frame_set()`

### Statistics Tracking
- [x] Create `ProcessingStats` struct
- [x] Count frames processed
- [x] Count calibration sets linked (flat + dark)
- [x] Count warnings generated (date + temperature)
- [x] Track missing calibration (flats, darks, bias)
- [x] Track full vs partial calibration
- [x] Implement `update_from_hierarchy()` method

### Progress Reporting
- [x] Create `ProcessingProgress` struct
- [x] Track total frames and processed count
- [x] Calculate percent complete
- [x] Track current frame ID

### Tests
- [x] Test ProcessingStats initialization
- [x] Test progress calculation
- [x] Test stats update with full calibration

**Status: COMPLETE ✅** - See `PHASE5_COMPLETE.md`

---

## Phase 6: Tauri Commands ✅

### Commands Implementation
- [x] Implement `find_calibration_for_frame_set()`
- [x] Implement `get_calibration_status()`
- [x] Implement `get_frame_calibration_hierarchy()`
- [x] Implement `clear_calibration_links()`
- [x] Implement `get_frame_calibration_links()`
- [x] Implement `get_frame_status()`
- [x] Register all commands in lib.rs

### TypeScript Interfaces
- [x] Add ProcessingStats interface
- [x] Add ProcessingProgress interface
- [x] Update models.ts with all calibration types

### Serialization
- [x] Add Serialize/Deserialize to ProcessingStats
- [x] Add Serialize/Deserialize to ProcessingProgress
- [x] Test command compilation

**Status: COMPLETE ✅** - See `PHASE6_COMPLETE.md`

---

## Phase 7: Frontend - Objects Page ⏳

### Objects.tsx Updates
- [ ] Add "Find Calibration Data" button in toolbar
- [ ] Integrate calibration status in frame list
- [ ] Add progress modal

### New Components
- [ ] Create `CalibrationFinderButton.tsx`
- [ ] Create `CalibrationStatusBadges.tsx`
- [ ] Create `FrameCalibrationSummary.tsx`
- [ ] Create `CalibrationProcessModal.tsx`

### Navigation
- [ ] Implement click badge → navigate to Equipment
- [ ] Pass highlight_set parameter

### Tests
- [ ] Test component rendering
- [ ] Test button interactions
- [ ] Test navigation flow

---

## Phase 8: Frontend - Equipment Tab ⏳

### Equipment.tsx Updates
- [ ] Show usage statistics for calibration sets
- [ ] Implement highlighting from navigation
- [ ] Add "in use" vs "unused" filter

### New Components
- [ ] Create `CalibrationSetUsageList.tsx`
- [ ] Create `BlinkHighlight.tsx`

### URL Parameter Handling
- [ ] Parse `?highlight_set={id}`
- [ ] Auto-scroll to highlighted set
- [ ] Apply blink animation

### Tests
- [ ] Test URL parameter parsing
- [ ] Test highlighting behavior
- [ ] Test usage statistics

---

## Phase 9: Settings & Configuration ⏳

### Settings UI
- [ ] Add calibration settings section in Settings.tsx
- [ ] Temperature tolerance input
- [ ] Flat date warning threshold input
- [ ] Dark date warning threshold input

### Settings Persistence
- [ ] Store settings in database
- [ ] Load settings on app startup
- [ ] Apply to matching algorithm

### Tests
- [ ] Test settings persistence
- [ ] Test validation
- [ ] Test default values

---

## Phase 10: Documentation ⏳

### Documentation Files
- [ ] Create `README.md`
- [ ] Create `ALGORITHM.md`
- [ ] Create `HIERARCHY.md`
- [ ] Create `DATABASE.md`
- [ ] Create `API.md`
- [ ] Create `USER_GUIDE.md`
- [ ] Create `TESTING.md`

### Content
- [ ] Add code examples
- [ ] Add workflow diagrams
- [ ] Add screenshots

---

## Phase 11: Testing & Polish ⏳

### Testing
- [ ] End-to-end workflow test
- [ ] Performance benchmarks
- [ ] Edge case testing

### Polish
- [ ] Error message improvements
- [ ] UI/UX refinements
- [ ] Code review and refactoring

---

## Legend
- ⏳ Not started
- 🔄 In progress
- ✅ Complete
- ❌ Blocked
