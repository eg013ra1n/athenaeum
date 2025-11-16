# Flat Calibration System - Implementation Tasks

**Status:** Backend Complete - Frontend In Progress
**Started:** 2025-01-16
**Backend Completed:** 2025-01-16
**Current Phase:** Phase 4 - Frontend UI

---

## Phase 1: Backend Core - Flat Group Detection ✅

### Task 1.1: Create flat_groups.rs Module ✅
**File:** `src-tauri/src/calibration/flat_groups.rs`

- [x] Create new module file
- [x] Define `FlatGroup` struct
- [x] Implement `detect_flat_groups()` function
  - [x] Query flat frames with exact parameter match
  - [x] Sort by date_obs
  - [x] Cluster by time threshold
  - [x] Calculate group metadata
- [x] Add unit tests for clustering algorithm
- [x] Add to `mod.rs` exports

**Deliverables:**
- `FlatGroup` struct ✅
- `detect_flat_groups()` function ✅
- Unit tests ✅

---

### Task 1.2: Create flat_matcher.rs Module ✅
**File:** `src-tauri/src/calibration/flat_matcher.rs`

- [x] Create new module file
- [x] Define `FlatGroupMatch` struct (group + match score)
- [x] Implement `find_flat_groups_for_light_frame()` function
  - [x] Get parameters from light frame
  - [x] Call detect_flat_groups()
  - [x] Filter by max_age setting
  - [x] Rank by date proximity
  - [x] Rank by temperature match (if available)
- [x] Implement `apply_pattern_selection()` function
  - [x] Handle `before_session` pattern
  - [x] Handle `after_session` pattern
  - [x] Handle `long_term` pattern
  - [x] Handle `before_filter_change` pattern (Phase 1.3)
- [x] Add unit tests for matching logic
- [x] Add to `mod.rs` exports

**Deliverables:**
- `FlatGroupMatch` struct ✅
- `find_flat_groups_for_light_frame()` function ✅
- `apply_pattern_selection()` function ✅
- Unit tests ✅

---

### Task 1.3: Filter Change Detection ✅
**File:** `src-tauri/src/calibration/flat_matcher.rs`

- [x] Define `FilterPeriod` struct
- [x] Implement `detect_filter_changes()` function
  - [x] Group consecutive frames by filter
  - [x] Identify transition timestamps
  - [x] Create filter periods with start/end times
- [x] Integrate into `apply_pattern_selection()` for `before_filter_change` pattern
- [x] Add unit tests for filter change detection

**Deliverables:**
- `FilterPeriod` struct ✅
- `detect_filter_changes()` function ✅
- Integration with pattern selection ✅
- Unit tests ✅

---

## Phase 2: Settings Integration ✅

### Task 2.1: Add Flat Calibration Settings ✅
**File:** `src-tauri/src/settings/mod.rs`

- [x] Add default constants:
  - [x] `FLATS_MAX_AGE_DAYS = 30`
  - [x] `FLATS_TIME_CLUSTER_MINUTES = 30`
  - [x] `TEMPERATURE_MATCH_WEIGHT = 0.3`
- [x] Add setting keys:
  - [x] `flats.max_age_days`
  - [x] `flats.time_cluster_minutes`
  - [x] `temperature.match_weight`
- [x] Add getter methods:
  - [x] `get_flats_max_age_days()`
  - [x] `get_flats_time_cluster_minutes()`
  - [x] `get_temperature_match_weight()`

**Deliverables:**
- New settings constants ✅
- New settings keys ✅
- Getter methods ✅

---

### Task 2.2: Database Schema Update ✅
**Files:** `src-tauri/src/db/schema.rs`, `src-tauri/src/models.rs`

- [x] Add migration for `frames_set.flat_pattern` column
- [x] Update `FramesSet` struct to include `flat_pattern: Option<String>`
- [x] Update SQL queries to include new column
- [x] Test schema migration

**Deliverables:**
- Database migration ✅
- Updated model struct ✅
- Updated queries ✅

---

## Phase 3: Enhanced Calibration Finder ✅

### Task 3.1: Create Calibration Set from Flat Group ✅
**File:** `src-tauri/src/calibration/flat_groups.rs`

- [x] Implement `create_flat_calibration_set()` function
  - [x] Check for existing set with same parameters
  - [x] If exists, return existing set_id
  - [x] If not, create new calibration_set entry
  - [x] Link frames via calibration_set_frames
  - [x] Return set_id
- [x] Add error handling
- [x] Add unit tests

**Deliverables:**
- `create_flat_calibration_set()` function ✅
- Deduplication logic ✅
- Unit tests ✅

---

### Task 3.2: Integrate Flat Matching into Hierarchy Builder ✅
**File:** `src-tauri/src/calibration/hierarchy.rs`

- [x] Modify `build_complete_hierarchy()` to use new flat matching
- [x] For each light frame:
  - [x] Check if flat_pattern is set for frame set
  - [x] If manual, use manual_flat_selections parameter
  - [x] Otherwise, call flat_matcher with pattern
  - [x] Create/reuse flat calibration set
  - [x] Add to hierarchy
- [x] Add age warnings for old flats
- [x] Add temperature warnings
- [x] Update tests

**Deliverables:**
- Modified hierarchy builder ✅
- Warning generation ✅
- Updated tests ✅

---

### Task 3.3: Enhance find_calibration_for_frame_set Command ✅
**File:** `src-tauri/src/commands.rs`

- [x] Add new parameters:
  - [x] `flat_pattern: Option<String>`
  - [x] `manual_flat_selections: Option<HashMap<String, i64>>`
- [x] Load flat_pattern from frame set if not provided
- [x] Pass pattern to hierarchy builder
- [x] Update ProcessingStats to include flat-specific metrics
- [x] Test with various patterns

**Deliverables:**
- Enhanced command signature ✅
- Pattern handling ✅
- Updated return type ✅

---

### Task 3.4: Create get_flat_group_options Command ✅
**File:** `src-tauri/src/commands.rs`

- [x] Implement `get_flat_group_options_for_frame_set()` command
  - [x] Get unique filters from frame set
  - [x] For each filter, call detect_flat_groups()
  - [x] Filter by max_age
  - [x] Return HashMap<filter, Vec<FlatGroup>>
- [x] Add to lib.rs command registry
- [x] Test with various frame sets

**Deliverables:**
- New Tauri command ✅
- Command registration ✅
- Tests ✅

---

## Phase 4: Frontend UI

### Task 4.1: TypeScript Interfaces ✅
**File:** `src/types/models.ts`

- [x] Add `FlatGroup` interface
- [x] Add `FlatGroupMatch` interface
- [x] Add `FilterPeriod` interface
- [x] Update `FramesSet` to include `flat_pattern?: string`
- [x] Update `ProcessingStats` for flat metrics

**Deliverables:**
- TypeScript interfaces matching Rust structs ✅

---

### Task 4.2: Pattern Selection Modal ✅
**File:** `src/components/FlatPatternModal.tsx`

- [x] Create modal component
- [x] Radio button group for pattern selection:
  - [x] Before session
  - [x] After session
  - [x] Before filter change
  - [x] Long-term
  - [x] Manual
- [x] Checkbox for "Remember for this frame set"
- [x] Cancel/Continue buttons
- [x] Styling with Tailwind (match existing modals)

**Deliverables:**
- FlatPatternModal component ✅
- Pattern selection UI ✅
- Remember preference checkbox ✅

---

### Task 4.3: Manual Flat Selection Modal ✅
**File:** `src/components/ManualFlatSelectionModal.tsx`

- [x] Create modal component
- [x] For each filter in frame set:
  - [x] Display filter name
  - [x] List available flat groups
  - [x] Show metadata (count, date, relative time, temp)
  - [x] Radio selection for group
  - [x] Highlight recommended option
- [x] Validate all filters have selection
- [x] Cancel/Apply buttons
- [x] Styling

**Deliverables:**
- ManualFlatSelectionModal component ✅
- Flat group selection UI ✅
- Validation ✅

---

### Task 4.4: Update CalibrationFinderButton ✅
**File:** `src/components/CalibrationFinderButton.tsx`

- [x] Check if frame set has flat_pattern set
- [x] If not, show FlatPatternModal first
- [x] If pattern == 'manual':
  - [x] Call `get_flat_group_options_for_frame_set`
  - [x] Show ManualFlatSelectionModal
  - [x] Get user selections
- [x] Pass pattern and selections to `find_calibration_for_frame_set`
- [x] Save pattern to frame set if "remember" checked
- [x] Handle all patterns
- [x] Create `update_frame_set_flat_pattern` backend command

**Deliverables:**
- Updated button component ✅
- Pattern modal integration ✅
- Manual selection integration ✅
- Backend command for saving pattern ✅

---

### Task 4.5: Update CalibrationProcessModal ⬜
**File:** `src/components/CalibrationProcessModal.tsx`

- [ ] Add flat-specific results section
- [ ] Display flat sets linked per filter
- [ ] Show age warnings
- [ ] Show temperature warnings
- [ ] Enhanced missing calibration messages

**Deliverables:**
- Enhanced results display
- Flat-specific metrics
- Warning display

---

### Task 4.6: Settings Page Integration ⬜
**File:** `src/pages/Settings.tsx`

- [ ] Add "Flat Calibration" section
- [ ] Add setting controls:
  - [ ] Max age (days) - number input
  - [ ] Time cluster threshold (minutes) - number input
  - [ ] Temperature match weight (0.0-1.0) - slider
- [ ] Save/load settings via Tauri commands
- [ ] Add tooltips/help text

**Deliverables:**
- Settings UI for flat calibration
- Controls for all flat settings

---

## Phase 5: Testing & Polish

### Task 5.1: Integration Testing ⬜
- [ ] Test with real frame set containing flats
- [ ] Test all patterns:
  - [ ] before_session
  - [ ] after_session
  - [ ] before_filter_change
  - [ ] long_term
  - [ ] manual
- [ ] Test edge cases:
  - [ ] No flats available
  - [ ] All flats too old
  - [ ] Multiple valid groups
  - [ ] Filter changes mid-session
  - [ ] Missing filter in flats
- [ ] Verify backwards compatibility

**Deliverables:**
- Test report
- Bug fixes
- Edge case handling

---

### Task 5.2: Documentation ⬜
- [ ] Update TASKS.md with Phase 7 completion
- [ ] Create PHASE7_COMPLETE.md
- [ ] Update IMPLEMENTATION_PLAN.md
- [ ] Add inline code documentation
- [ ] Update CLAUDE.md if needed

**Deliverables:**
- Complete documentation
- Phase 7 completion report

---

### Task 5.3: Performance Optimization ⬜
- [ ] Profile flat group detection queries
- [ ] Add database indexes if needed
- [ ] Optimize clustering algorithm
- [ ] Cache flat groups during processing
- [ ] Measure end-to-end performance

**Deliverables:**
- Performance benchmarks
- Optimizations applied

---

## Verification Checklist

- [ ] All Rust code compiles without errors
- [ ] All unit tests pass
- [ ] Frontend builds successfully
- [ ] Manual testing with real data successful
- [ ] All patterns work correctly
- [ ] Edge cases handled gracefully
- [ ] Settings persist correctly
- [ ] Pattern preference saves to frame set
- [ ] UI is intuitive and responsive
- [ ] Documentation is complete

---

## Notes

- Keep Dark/Bias calibration unchanged (stable sets approach)
- Flats use dynamic, pattern-based matching
- Frame set pattern preference is optional (can be null)
- Manual mode allows maximum user control
- System must handle missing flats gracefully
