# Phase 6: Tauri Commands - COMPLETE ✅

**Completion Date:** 2025-11-16

## Summary

Phase 6 of the automated calibration finder system has been successfully completed. This phase exposes all calibration functionality to the frontend via Tauri commands, enabling the UI to find, query, and manage calibration data.

## Deliverables

### 1. Tauri Commands ✅

**File:** `src-tauri/src/commands.rs` (6 new commands added, lines 2477-2674)

**Commands Implemented:**
- `find_calibration_for_frame_set()` - Main processing command
- `get_calibration_status()` - Get statistics for a frame set
- `get_frame_calibration_hierarchy()` - Get hierarchy for a specific frame
- `clear_calibration_links()` - Clear links before re-processing
- `get_frame_calibration_links()` - Get links for a specific frame
- `get_frame_status()` - Get frame's calibration status

### 2. TypeScript Interfaces ✅

**File:** `src/types/models.ts` (2 new interfaces added)

**Interfaces Added:**
- `ProcessingProgress` - Real-time progress tracking
- `ProcessingStats` - Comprehensive processing statistics

---

## Command Details

### `find_calibration_for_frame_set`

**Purpose:** Find and link calibration for all light frames in a frame set

**Parameters:**
```typescript
{
  frame_set_id: number;
  temp_delta_celsius?: number;  // Default: 2.0
  flat_date_warning_days?: number;  // Default: 30
  dark_date_warning_days?: number;  // Default: 365
}
```

**Returns:** `ProcessingStats`

**Features:**
- Accepts optional tolerance parameters (uses defaults if not provided)
- Processes all light frames in the frame set
- Builds calibration hierarchy for each frame
- Stores all links in database
- Returns comprehensive statistics
- Logs progress to console

**Frontend Usage:**
```typescript
import { invoke } from '@tauri-apps/api/core';
import type { ProcessingStats } from './types/models';

const stats = await invoke<ProcessingStats>('find_calibration_for_frame_set', {
  frameSetId: 123,
  tempDeltaCelsius: 2.0,
  flatDateWarningDays: 30,
  darkDateWarningDays: 365
});

console.log(`Processed ${stats.total_frames} frames`);
console.log(`Full calibration: ${stats.frames_with_full_calibration}`);
console.log(`Partial calibration: ${stats.frames_with_partial_calibration}`);
console.log(`No calibration: ${stats.frames_with_no_calibration}`);
console.log(`Warnings: ${stats.total_warnings} (${stats.date_warnings} date, ${stats.temp_warnings} temp)`);
```

---

### `get_calibration_status`

**Purpose:** Get calibration statistics for a frame set

**Parameters:**
```typescript
{
  frame_set_id: number;
}
```

**Returns:** `CalibrationStats`

**Features:**
- Retrieves statistics from database
- Faster than reprocessing
- Shows current calibration status

**Frontend Usage:**
```typescript
import { invoke } from '@tauri-apps/api/core';
import type { CalibrationStats } from './types/models';

const stats = await invoke<CalibrationStats>('get_calibration_status', {
  frameSetId: 123
});

console.log(`Total frames: ${stats.total_frames}`);
console.log(`Frames with flats: ${stats.frames_with_flats}`);
console.log(`Frames with darks: ${stats.frames_with_darks}`);
```

---

### `get_frame_calibration_hierarchy`

**Purpose:** Get complete calibration hierarchy for a specific frame

**Parameters:**
```typescript
{
  frame_id: number;
  temp_delta_celsius?: number;  // Default: 2.0
  flat_date_warning_days?: number;  // Default: 30
  dark_date_warning_days?: number;  // Default: 365
}
```

**Returns:** `CalibrationHierarchy`

**Features:**
- Builds fresh hierarchy for a single frame
- Includes tolerance parameters
- Shows complete calibration tree
- Includes all warnings and missing calibration

**Frontend Usage:**
```typescript
import { invoke } from '@tauri-apps/api/core';
import type { CalibrationHierarchy } from './types/models';

const hierarchy = await invoke<CalibrationHierarchy>('get_frame_calibration_hierarchy', {
  frameId: 456,
  tempDeltaCelsius: 2.0,
  flatDateWarningDays: 30,
  darkDateWarningDays: 365
});

console.log(`Light frame: ${hierarchy.light_frame_id}`);
console.log(`Flat sets: ${hierarchy.flat_sets.length}`);
console.log(`Dark sets: ${hierarchy.dark_sets.length}`);
console.log(`Missing: ${hierarchy.missing_calibration.join(', ')}`);
console.log(`Warnings: ${hierarchy.warnings.length}`);

// Display hierarchy tree
for (const flatSet of hierarchy.flat_sets) {
  console.log(`  Flat Set ${flatSet.set.id}`);
  for (const link of flatSet.sub_calibration) {
    console.log(`    └─ ${link.calibration_type} Set ${link.calibration_set_id}`);
  }
}
```

---

### `clear_calibration_links`

**Purpose:** Clear all calibration links for a frame set

**Parameters:**
```typescript
{
  frame_set_id: number;
}
```

**Returns:** `number` (count of deleted links)

**Features:**
- Removes all calibration links for frames in the set
- Useful before re-processing with different tolerances
- Returns count of links deleted
- Logs to console

**Frontend Usage:**
```typescript
import { invoke } from '@tauri-apps/api/core';

const deletedCount = await invoke<number>('clear_calibration_links', {
  frameSetId: 123
});

console.log(`Cleared ${deletedCount} calibration links`);
```

---

### `get_frame_calibration_links`

**Purpose:** Get calibration links for a specific frame

**Parameters:**
```typescript
{
  frame_id: number;
}
```

**Returns:** `CalibrationLink[]`

**Features:**
- Retrieves stored links from database
- Shows what calibration is linked to the frame
- Includes match scores and warnings

**Frontend Usage:**
```typescript
import { invoke } from '@tauri-apps/api/core';
import type { CalibrationLink } from './types/models';

const links = await invoke<CalibrationLink[]>('get_frame_calibration_links', {
  frameId: 456
});

for (const link of links) {
  console.log(`${link.calibration_type}: Set ${link.calibration_set_id} (score: ${link.match_score})`);
  if (link.date_warning) console.log('  ⚠ Date warning');
  if (link.temp_warning) console.log('  ⚠ Temperature warning');
}
```

---

### `get_frame_status`

**Purpose:** Get frame's calibration status summary

**Parameters:**
```typescript
{
  frame_id: number;
}
```

**Returns:** `FrameCalibrationStatus`

**Features:**
- Quick status check for a frame
- Shows which calibration types are linked
- Includes warning counts

**Frontend Usage:**
```typescript
import { invoke } from '@tauri-apps/api/core';
import type { FrameCalibrationStatus } from './types/models';

const status = await invoke<FrameCalibrationStatus>('get_frame_status', {
  frameId: 456
});

console.log(`Frame ${status.frame_id}:`);
console.log(`  Has Flat: ${status.has_flat_calibration}`);
console.log(`  Has Dark: ${status.has_dark_calibration}`);
console.log(`  Has Bias: ${status.has_bias_calibration}`);
console.log(`  Warnings: ${status.total_warnings}`);
```

---

## TypeScript Interfaces

### ProcessingProgress

```typescript
export interface ProcessingProgress {
  total_frames: number;
  processed_frames: number;
  current_frame_id: number | null;
  percent_complete: number;
}
```

**Purpose:** Real-time progress tracking (for future event streaming)

**Fields:**
- `total_frames` - Total number of frames to process
- `processed_frames` - Number of frames processed so far
- `current_frame_id` - ID of frame currently being processed
- `percent_complete` - Completion percentage (0-100)

---

### ProcessingStats

```typescript
export interface ProcessingStats {
  total_frames: number;
  frames_with_full_calibration: number;
  frames_with_partial_calibration: number;
  frames_with_no_calibration: number;
  total_flat_sets_linked: number;
  total_dark_sets_linked: number;
  total_warnings: number;
  date_warnings: number;
  temp_warnings: number;
  missing_flats: number;
  missing_darks: number;
  missing_bias: number;
}
```

**Purpose:** Comprehensive processing statistics

**Calibration Completeness:**
- `frames_with_full_calibration` - Both Flat and Dark linked
- `frames_with_partial_calibration` - Either Flat or Dark (but not both)
- `frames_with_no_calibration` - Neither Flat nor Dark

**Sets Linked:**
- `total_flat_sets_linked` - Total Flat sets linked
- `total_dark_sets_linked` - Total Dark sets linked

**Warnings:**
- `total_warnings` - Total warnings generated
- `date_warnings` - Warnings due to calibration age
- `temp_warnings` - Warnings due to temperature mismatch

**Missing Calibration:**
- `missing_flats` - Frames missing Flat calibration
- `missing_darks` - Frames missing Dark calibration
- `missing_bias` - Frames missing Bias calibration

---

## Command Registration

All commands registered in `src-tauri/src/lib.rs` (lines 121-126):

```rust
.invoke_handler(tauri::generate_handler![
    // ... existing commands ...
    commands::find_calibration_for_frame_set,
    commands::get_calibration_status,
    commands::get_frame_calibration_hierarchy,
    commands::clear_calibration_links,
    commands::get_frame_calibration_links,
    commands::get_frame_status,
    commands_rustafits::read_fits_image_rustafits,
])
```

---

## Serialization

Added `Serialize` and `Deserialize` derives to:

1. **ProcessingStats** (`src-tauri/src/calibration/processor.rs:19`)
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingStats { ... }
```

2. **ProcessingProgress** (`src-tauri/src/calibration/processor.rs:10`)
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingProgress { ... }
```

This enables these types to be returned from Tauri commands and serialized to JSON for the frontend.

---

## Error Handling

All commands implement comprehensive error handling:

1. **Database Connection**: Check that database is initialized
2. **Frame/Set Existence**: Validate IDs exist in database
3. **Processing Errors**: Catch and format errors from calibration modules
4. **User-Friendly Messages**: Convert anyhow::Error to String with context

**Example Error Flow:**
```rust
let stats = process_frame_set(&conn, frame_set_id, &tolerance)
    .map_err(|e| format!("Failed to process frame set: {}", e))?;
```

---

## Console Logging

All commands include informative console logging:

1. **Start Messages**: Log operation start with parameters
2. **Progress Updates**: Log intermediate progress (from processor module)
3. **Completion Messages**: Log success with key statistics
4. **Error Messages**: Log errors with context

**Example Output:**
```
Finding calibration for frame set 123 with tolerance: temp=±2°C, flat_date=30 days, dark_date=365 days
Progress: 10/50 frames (20.0%)
Progress: 20/50 frames (40.0%)
Progress: 30/50 frames (60.0%)
Progress: 40/50 frames (80.0%)
Progress: 50/50 frames (100.0%)
✅ Calibration processing complete: 50 frames, 45 with full calibration
```

---

## Integration with Previous Phases

### Phase 1: Database Schema
- Uses `calibration_set_to_frames` table ✓
- Uses database operations from `calibration_links.rs` ✓

### Phase 2: Matching Algorithm
- Uses CalibrationTolerance struct ✓
- Passes tolerance parameters to matching functions ✓

### Phase 3: Hierarchical Builder
- Uses `build_complete_hierarchy()` ✓
- Returns CalibrationHierarchy ✓

### Phase 4: Auto-Creation
- Can process auto-created calibration sets ✓

### Phase 5: Frame Set Processor
- Main command uses `process_frame_set()` ✓
- Returns ProcessingStats ✓
- Uses `clear_calibration_links_for_frame_set()` ✓

---

## Usage Workflow Example

### Complete Frontend Workflow

```typescript
import { invoke } from '@tauri-apps/api/core';
import type { ProcessingStats, CalibrationHierarchy } from './types/models';

async function processFrameSet(frameSetId: number) {
  try {
    // 1. Clear existing links (optional - for reprocessing)
    const cleared = await invoke<number>('clear_calibration_links', {
      frameSetId
    });
    console.log(`Cleared ${cleared} existing links`);

    // 2. Find calibration with custom tolerances
    const stats = await invoke<ProcessingStats>('find_calibration_for_frame_set', {
      frameSetId,
      tempDeltaCelsius: 3.0,  // More lenient
      flatDateWarningDays: 60,
      darkDateWarningDays: 730
    });

    // 3. Display results
    console.log('Processing complete!');
    console.log(`Total: ${stats.total_frames} frames`);
    console.log(`Full calibration: ${stats.frames_with_full_calibration}`);
    console.log(`Partial calibration: ${stats.frames_with_partial_calibration}`);
    console.log(`No calibration: ${stats.frames_with_no_calibration}`);

    if (stats.total_warnings > 0) {
      console.warn(`⚠ ${stats.total_warnings} warnings:`);
      console.warn(`  Date: ${stats.date_warnings}`);
      console.warn(`  Temperature: ${stats.temp_warnings}`);
    }

    if (stats.missing_flats > 0 || stats.missing_darks > 0 || stats.missing_bias > 0) {
      console.warn('Missing calibration:');
      if (stats.missing_flats > 0) console.warn(`  Flats: ${stats.missing_flats} frames`);
      if (stats.missing_darks > 0) console.warn(`  Darks: ${stats.missing_darks} frames`);
      if (stats.missing_bias > 0) console.warn(`  Bias: ${stats.missing_bias} frames`);
    }

  } catch (error) {
    console.error('Failed to process frame set:', error);
  }
}

async function showFrameDetails(frameId: number) {
  try {
    // Get detailed hierarchy for a specific frame
    const hierarchy = await invoke<CalibrationHierarchy>('get_frame_calibration_hierarchy', {
      frameId
    });

    console.log(`Calibration for frame ${hierarchy.light_frame_id}:`);

    // Show Flat sets
    for (const flatSet of hierarchy.flat_sets) {
      console.log(`  Flat Set ${flatSet.set.id}:`);
      console.log(`    Frames: ${flatSet.set.frame_count}`);
      console.log(`    Filter: ${flatSet.set.filter || 'N/A'}`);

      for (const subLink of flatSet.sub_calibration) {
        console.log(`    └─ ${subLink.calibration_type} Set ${subLink.calibration_set_id}`);
      }
    }

    // Show Dark sets
    for (const darkSet of hierarchy.dark_sets) {
      console.log(`  Dark Set ${darkSet.set.id}:`);
      console.log(`    Frames: ${darkSet.set.frame_count}`);
      console.log(`    Exposure: ${darkSet.set.exptime}s`);
    }

    // Show warnings
    for (const warning of hierarchy.warnings) {
      console.warn(`  ⚠ ${warning.calibration_type}: ${warning.message}`);
    }

    // Show missing calibration
    if (hierarchy.missing_calibration.length > 0) {
      console.warn(`  Missing: ${hierarchy.missing_calibration.join(', ')}`);
    }

  } catch (error) {
    console.error('Failed to get frame hierarchy:', error);
  }
}
```

---

## Files Modified/Created

1. **Modified:** `src-tauri/src/commands.rs` (added 6 commands, 198 lines)
2. **Modified:** `src-tauri/src/lib.rs` (registered 6 commands)
3. **Modified:** `src-tauri/src/calibration/processor.rs` (added Serialize/Deserialize)
4. **Modified:** `src/types/models.ts` (added 2 interfaces)
5. **Modified:** `docs/calibration/TASKS.md` (marked Phase 6 complete)
6. **Created:** `docs/calibration/PHASE6_COMPLETE.md` (this file)

---

## Compilation Status

**Status:** ✅ Compiles successfully

**Warnings:** General project warnings (unused variables, lifetime syntax)

**Errors:** None

---

## Next Steps

**Phase 7: Frontend - Objects Page**
- Add "Find Calibration Data" button in toolbar
- Integrate calibration status in frame list
- Add progress modal
- Create `CalibrationFinderButton.tsx` component
- Create `CalibrationStatusBadges.tsx` component
- Create `FrameCalibrationSummary.tsx` component
- Create `CalibrationProcessModal.tsx` component
- Implement click badge → navigate to Equipment
- Pass highlight_set parameter

---

## Validation

- [x] find_calibration_for_frame_set() command implemented
- [x] get_calibration_status() command implemented
- [x] get_frame_calibration_hierarchy() command implemented
- [x] clear_calibration_links() command implemented
- [x] get_frame_calibration_links() command implemented
- [x] get_frame_status() command implemented
- [x] All commands registered in lib.rs
- [x] ProcessingStats interface added
- [x] ProcessingProgress interface added
- [x] Serialize/Deserialize derives added
- [x] Error handling implemented
- [x] Console logging implemented
- [x] Code compiles without errors
- [x] Documentation complete

**Phase 6: COMPLETE ✅**

Ready to proceed to Phase 7: Frontend - Objects Page UI.
