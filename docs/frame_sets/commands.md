# Frame Sets Commands API Reference

## Overview

This document provides complete API documentation for all Tauri commands related to frame set operations.

## Table of Contents

1. [Frame Set Query Commands](#frame-set-query-commands)
2. [Frame Set Creation Commands](#frame-set-creation-commands)
3. [Frame Set Modification Commands](#frame-set-modification-commands)
4. [Frame Set Deletion Commands](#frame-set-deletion-commands)
5. [Merge and Split Commands](#merge-and-split-commands)

---

## Frame Set Query Commands

### `get_frames_sets`

Get all frame sets with member counts.

**Signature:**
```rust
pub async fn get_frames_sets(
    project_id: i64,
    state: State<'_, AppState>
) -> Result<Vec<(FramesSet, usize)>, String>
```

**Parameters:**
- `project_id`: i64 - Ignored (kept for backwards compatibility)

**Returns:**
```typescript
Array<[FramesSet, number]>  // [frame_set, member_count]
```

**TypeScript Interface:**
```typescript
interface FramesSet {
  id: number | null;
  name: string | null;
  is_custom: boolean;
  date_obs_start: string | null;
  date_obs_end: string | null;
  objctra: string | null;
  objctdec: string | null;
  total_exp_time: number | null;
}
```

**Example:**
```typescript
const sets = await invoke<[FramesSet, number][]>('get_frames_sets', { projectId: 1 });
for (const [set, count] of sets) {
  console.log(`${set.name}: ${count} frames`);
}
```

**Notes:**
- Returns empty array if no frame sets exist
- `member_count` counts distinct frames across all sessions
- Ordered by `date_obs_start DESC, name ASC`

---

### `get_frame_set_detail`

Get complete frame set hierarchy including nights, sessions, and frames.

**Signature:**
```rust
pub async fn get_frame_set_detail(
    frames_set_id: i64,
    state: State<'_, AppState>
) -> Result<FrameSetDetail, String>
```

**Parameters:**
- `frames_set_id`: i64 - ID of frame set to retrieve

**Returns:**
```typescript
interface FrameSetDetail {
  frames_set: FramesSet;
  nights: ImagingNightWithSessions[];
}

interface ImagingNightWithSessions {
  imaging_night: ImagingNight;
  sessions: SessionWithFrames[];
}

interface SessionWithFrames {
  session: Session;
  frames: FileWithFrame[];
}
```

**Example:**
```typescript
const detail = await invoke<FrameSetDetail>('get_frame_set_detail', {
  framesSetId: 42
});

console.log(`Frame set: ${detail.frames_set.name}`);
console.log(`Nights: ${detail.nights.length}`);
for (const night of detail.nights) {
  console.log(`  Night: ${night.imaging_night.start_time}`);
  for (const session of night.sessions) {
    console.log(`    ${session.session.instrume}: ${session.frames.length} frames`);
  }
}
```

**Errors:**
- "Frame set not found" - Invalid frames_set_id
- "Database not initialized" - Database connection failed

---

## Frame Set Creation Commands

### `auto_generate_frame_sets`

Automatically generate frame sets from LIGHT frames using coordinate-based clustering.

**Signature:**
```rust
pub async fn auto_generate_frame_sets(
    project_id: i64,
    state: State<'_, AppState>
) -> Result<AutoGenerateResult, String>
```

**Parameters:**
- `project_id`: i64 - Ignored (processes all frames)

**Returns:**
```typescript
interface AutoGenerateResult {
  sets_created: number;
  frames_clustered: number;
  frames_excluded: number;
  frames_already_in_sets: number;
  exclusion_reasons: string[];
}
```

**Example:**
```typescript
const result = await invoke<AutoGenerateResult>('auto_generate_frame_sets', {
  projectId: 1
});

console.log(`Created ${result.sets_created} frame sets`);
console.log(`Clustered ${result.frames_clustered} frames`);
console.log(`Excluded ${result.frames_excluded} frames`);
```

**Behavior:**
- Only processes LIGHT frames
- Excludes frames already in any frame set
- Uses DBSCAN-like clustering on RA/Dec coordinates
- Threshold from setting: `grouping_threshold_arcmin` (default: 15)
- Creates `is_custom = false` frame sets
- Auto-detects nights and sessions

**Notes:**
- Idempotent: safe to run multiple times
- Will not duplicate frames in multiple sets
- Exclusion reasons include: missing coordinates, already assigned

---

### `create_custom_frames_set`

Create a custom frame set from selected sessions.

**Signature:**
```rust
pub async fn create_custom_frames_set(
    name: String,
    session_ids: Vec<i64>,
    state: State<'_, AppState>
) -> Result<i64, String>
```

**Parameters:**
- `name`: String - Name for the new frame set
- `session_ids`: Vec<i64> - IDs of sessions to include

**Returns:**
- `i64` - ID of newly created frame set

**Example:**
```typescript
const setId = await invoke<number>('create_custom_frames_set', {
  name: "M31 2024 October",
  sessionIds: [12, 15, 18]
});

console.log(`Created frame set ${setId}`);
```

**Behavior:**
- Clones selected sessions into new frame set
- Detects and creates imaging nights based on time gaps
- Calculates metadata from all frames
- Creates `is_custom = true` frame set

**Errors:**
- "No frames found in selected sessions" - Invalid session IDs or empty sessions
- "No imaging nights could be detected" - Frames missing date/time info

---

### `create_frame_set_from_selection`

Create a frame set from spatially selected frames (map selection).

**Signature:**
```rust
pub async fn create_frame_set_from_selection(
    state: State<'_, AppState>,
    name: String,
    frame_ids: Vec<i64>,
    description: Option<String>
) -> Result<i64, String>
```

**Parameters:**
- `name`: String - Name for the new frame set
- `frame_ids`: Vec<i64> - IDs of frames to include
- `description`: Option<String> - Optional description

**Returns:**
- `i64` - ID of newly created frame set

**Example:**
```typescript
const setId = await invoke<number>('create_frame_set_from_selection', {
  name: "M31 Core Region",
  frameIds: [101, 102, 103, 104],
  description: "Frames covering galaxy core"
});
```

**Behavior:**
- Creates new nights and sessions from scratch
- Detects nights based on time gaps
- Groups by instrument within nights
- Creates `is_custom = true` frame set
- Falls back to single night/session if detection fails

**Errors:**
- "Cannot create frame set with no frames" - Empty frame_ids
- "Failed to get frames" - Invalid frame IDs

---

## Frame Set Modification Commands

### `rename_frames_set`

Rename a frame set.

**Signature:**
```rust
pub async fn rename_frames_set(
    frames_set_id: i64,
    new_name: String,
    state: State<'_, AppState>
) -> Result<(), String>
```

**Parameters:**
- `frames_set_id`: i64 - ID of frame set to rename
- `new_name`: String - New name

**Example:**
```typescript
await invoke('rename_frames_set', {
  framesSetId: 42,
  newName: "M31 Widefield (Final)"
});
```

**Notes:**
- Only updates the name field
- Does not modify any other metadata

---

### `recalculate_frame_set_metadata`

Recalculate all metadata for a frame set from its member frames.

**Signature:**
```rust
pub async fn recalculate_frame_set_metadata(
    frames_set_id: i64,
    state: State<'_, AppState>
) -> Result<FramesSet, String>
```

**Parameters:**
- `frames_set_id`: i64 - ID of frame set to recalculate

**Returns:**
- `FramesSet` - Updated frame set with recalculated metadata

**Example:**
```typescript
const updated = await invoke<FramesSet>('recalculate_frame_set_metadata', {
  framesSetId: 42
});

console.log(`Updated coordinates: ${updated.objctra}, ${updated.objctdec}`);
console.log(`Total exposure: ${updated.total_exp_time}s`);
```

**Behavior:**
- Recalculates all metadata fields:
  - `date_obs_start`, `date_obs_end`
  - `objctra`, `objctdec`
  - `total_exp_time`
- Marks frame set as `is_custom = true`
- Useful after manual modifications or data corrections

**Use Cases:**
- After importing frames with corrected coordinates
- After fixing timestamp errors
- Recovery from inconsistent state

---

## Frame Set Deletion Commands

### `delete_frames_set`

Delete a frame set and all its associated nights/sessions.

**Signature:**
```rust
pub async fn delete_frames_set(
    frames_set_id: i64,
    state: State<'_, AppState>
) -> Result<(), String>
```

**Parameters:**
- `frames_set_id`: i64 - ID of frame set to delete

**Example:**
```typescript
await invoke('delete_frames_set', { framesSetId: 42 });
```

**Behavior:**
- Deletes the frame set record
- CASCADE deletes all imaging_nights
- CASCADE deletes all sessions
- CASCADE deletes all session_members
- Does NOT delete actual frame records

**Warning:**
- This operation is irreversible
- All organization (nights, sessions) is permanently lost
- Frame files and metadata are preserved

---

## Merge and Split Commands

### `merge_frame_sets`

Merge source frame set into target frame set.

**Signature:**
```rust
pub async fn merge_frame_sets(
    source_id: i64,
    target_id: i64,
    state: State<'_, AppState>
) -> Result<FrameSetDetail, String>
```

**Parameters:**
- `source_id`: i64 - Frame set to be merged (will be deleted)
- `target_id`: i64 - Frame set to merge into (will be updated)

**Returns:**
- `FrameSetDetail` - Updated target frame set with merged content

**Example:**
```typescript
const merged = await invoke<FrameSetDetail>('merge_frame_sets', {
  sourceId: 42,
  targetId: 43
});

console.log(`Merged into: ${merged.frames_set.name}`);
console.log(`Total nights: ${merged.nights.length}`);
```

**Behavior:**

1. **Night Matching:**
   - For each source night:
     - Find matching target night (same calendar date + overlapping times)
     - If match: merge sessions, update time range to union
     - If no match: reassign night to target set

2. **Deduplication:**
   - Removes duplicate frame references
   - Each frame appears only once in result

3. **Metadata Update:**
   - Recalculates all metadata for target
   - Marks target as `is_custom = true`

4. **Source Deletion:**
   - Deletes source frame set after successful merge
   - CASCADE handles cleanup

**Errors:**
- "Cannot merge a frame set into itself" - source_id == target_id
- "Failed to get source/target nights" - Invalid frame set IDs
- "Failed to deduplicate" - Database integrity issue

**Workflow:**
```
Before:
  Source: 2 nights, 50 frames
  Target: 3 nights, 75 frames

After:
  Source: DELETED
  Target: 4-5 nights, ~125 frames (deduplicated)
```

---

### `can_split`

Check if a split operation is valid (won't leave source empty).

**Signature:**
```rust
pub async fn can_split(
    source_set_id: i64,
    selection: SplitSelection,
    state: State<'_, AppState>
) -> Result<bool, String>
```

**Parameters:**
- `source_set_id`: i64 - Frame set to split from
- `selection`: SplitSelection - What to split (see below)

**SplitSelection:**
```typescript
type SplitSelection =
  | { type: "nights", ids: number[] }
  | { type: "sessions", ids: number[] }
  | { type: "frames", ids: number[] };
```

**Returns:**
- `boolean` - true if split is allowed, false if it would empty source

**Example:**
```typescript
const canSplit = await invoke<boolean>('can_split', {
  sourceSetId: 42,
  selection: { type: "nights", ids: [10, 11] }
});

if (!canSplit) {
  alert("Cannot split all nights - would leave source empty");
}
```

**Validation Logic:**
- Counts total items (nights/sessions/frames)
- Counts selected items
- Returns `selected < total`

**Use Cases:**
- UI validation before showing split dialog
- Preventing empty frame sets
- Conditional button display

---

### `split_frame_set`

Split selected items into a new frame set.

**Signature:**
```rust
pub async fn split_frame_set(
    source_set_id: i64,
    selection: SplitSelection,
    new_name: String,
    state: State<'_, AppState>
) -> Result<FrameSetDetail, String>
```

**Parameters:**
- `source_set_id`: i64 - Frame set to split from
- `selection`: SplitSelection - What to split
- `new_name`: String - Name for new frame set

**Returns:**
- `FrameSetDetail` - Newly created frame set with split content

**Example:**
```typescript
const newSet = await invoke<FrameSetDetail>('split_frame_set', {
  sourceSetId: 42,
  selection: { type: "nights", ids: [10, 11] },
  newName: "M31 Widefield - Split 1"
});

console.log(`Created: ${newSet.frames_set.name}`);
console.log(`ID: ${newSet.frames_set.id}`);
```

**Behavior:**

1. **Validation:**
   - Calls `can_split()` internally
   - Rejects if would empty source

2. **Frame Collection:**
   - Based on selection type:
     - Nights: all frames from selected nights
     - Sessions: all frames from selected sessions
     - Frames: selected frames directly

3. **New Set Creation:**
   - Calculates metadata from collected frames
   - Creates new frame set (`is_custom = true`)
   - Detects and creates nights/sessions
   - Inserts session members

4. **Source Update:**
   - Removes split items from source:
     - Nights: DELETE imaging_nights
     - Sessions: DELETE sessions
     - Frames: DELETE session_members
   - Recalculates source metadata
   - Marks source as `is_custom = true`

**Errors:**
- "Cannot split: operation would leave the source frame set empty"
- "No frames to split" - Empty selection or invalid IDs
- "Failed to detect sessions" - Frames missing date/time

**Workflow:**
```
Before:
  Source: 5 nights, 100 frames

Split (2 nights):
  Source: 3 nights, ~60 frames (recalculated)
  New:    2 nights, ~40 frames (new set)
```

---

## Command Workflow Examples

### Complete Merge Workflow

```typescript
// 1. User drags frame set 42 onto frame set 43
// 2. Show confirmation dialog
const confirmed = confirm("Merge 'M31 October' into 'M31 2024'?");
if (!confirmed) return;

// 3. Execute merge
try {
  const result = await invoke<FrameSetDetail>('merge_frame_sets', {
    sourceId: 42,
    targetId: 43
  });

  // 4. Update UI with merged frame set
  console.log(`Merge successful: ${result.frames_set.name}`);
  console.log(`Total nights: ${result.nights.length}`);

  // 5. Refresh frame sets list
  await refreshFrameSets();

} catch (error) {
  alert(`Merge failed: ${error}`);
}
```

### Complete Split Workflow

```typescript
// 1. User selects nights in frame set detail view
const selectedNights = [10, 11, 12];  // 3 nights selected

// 2. Check if split is valid
const canSplit = await invoke<boolean>('can_split', {
  sourceSetId: 42,
  selection: { type: "nights", ids: selectedNights }
});

if (!canSplit) {
  alert("Cannot split all nights from the frame set");
  return;
}

// 3. Prompt for name
const name = prompt("Name for new frame set:", "M31 Widefield - Split 1");
if (!name) return;

// 4. Execute split
try {
  const newSet = await invoke<FrameSetDetail>('split_frame_set', {
    sourceSetId: 42,
    selection: { type: "nights", ids: selectedNights },
    newName: name
  });

  // 5. Navigate to new frame set or refresh
  console.log(`Split successful: ${newSet.frames_set.name}`);
  navigateTo(`/objects/${newSet.frames_set.id}`);

} catch (error) {
  alert(`Split failed: ${error}`);
}
```

---

## Error Handling

All commands return `Result<T, String>` which translates to TypeScript as:
- Success: Returns value of type `T`
- Error: Throws string error message

**Common Error Patterns:**

```typescript
try {
  const result = await invoke<FrameSetDetail>('merge_frame_sets', {
    sourceId: 42,
    targetId: 43
  });
  // Success
} catch (error) {
  // Error is a string
  console.error(`Operation failed: ${error}`);

  // Display to user
  alert(`Failed to merge frame sets: ${error}`);
}
```

**Error Categories:**

1. **Validation Errors:**
   - "Cannot merge a frame set into itself"
   - "Cannot split: operation would leave the source frame set empty"
   - "Cannot create frame set with no frames"

2. **Not Found Errors:**
   - "Frame set not found"
   - "Target night not found"

3. **Database Errors:**
   - "Database not initialized"
   - "Failed to get nights: ..."
   - "Failed to create frame set: ..."

4. **Data Errors:**
   - "No frames found in selected sessions"
   - "No imaging nights could be detected"

---

## Performance Considerations

### Query Optimization

- `get_frames_sets`: Single JOIN query with COUNT
- `get_frame_set_detail`: Hierarchical queries (set → nights → sessions → frames)
- Large frame sets (>1000 frames) may take 1-2 seconds to load detail

### Bulk Operations

- `merge_frame_sets`: O(N_nights × M_nights + F_frames) complexity
- `split_frame_set`: O(F_frames) complexity
- Use transactions for atomicity

### Recommended Limits

- Frame sets: No hard limit, tested with 10,000+ frames
- Sessions per night: Practical limit ~20 (different instruments)
- Nights per set: Practical limit ~100 (multi-year projects)

---

## Migration Notes

### Breaking Changes from v1.0

1. **`FramesSet` Interface:**
   - Removed: `project_id`, `date_obs`
   - Added: `date_obs_start`, `date_obs_end`

2. **`create_custom_frames_set`:**
   - Removed `project_id` parameter

3. **`create_frame_set_from_selection`:**
   - Removed `project_id` parameter

### Migration Guide

```typescript
// v1.0 (OLD)
await invoke('create_custom_frames_set', {
  name: "M31",
  sessionIds: [1, 2, 3],
  projectId: 1  // ← Remove this
});

// v2.0 (NEW)
await invoke('create_custom_frames_set', {
  name: "M31",
  sessionIds: [1, 2, 3]
});
```

**Frontend Updates Required:**
- Remove project_id from all frame set creation calls
- Update FramesSet interface to use date_obs_start/date_obs_end
- Display date range instead of single date
