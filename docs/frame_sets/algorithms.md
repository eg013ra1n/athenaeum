# Frame Sets Algorithms

## Overview

This document describes the algorithms used for frame set operations including night matching, merging, splitting, deduplication, and metadata calculation.

## Night Matching Algorithm

### Purpose
Determine if two imaging nights from different frame sets represent the same observing session and should be merged.

### Criteria
Two nights match if **BOTH** conditions are met:
1. **Same Calendar Night:** They share at least one calendar date
2. **Overlapping Time Ranges:** Their time ranges overlap

### Implementation

```rust
pub fn nights_match(night_a: &ImagingNight, night_b: &ImagingNight) -> Result<bool>
```

**Algorithm:**

1. **Parse Timestamps:**
   - Convert start and end times from RFC3339 strings to DateTime objects
   - Convert to UTC timezone

2. **Extract Calendar Dates:**
   - Extract naive date (year-month-day) from each timestamp
   - Handle multi-day observations (e.g., starting at 23:00 on Oct 24, ending at 05:00 on Oct 25)
   - Create list of all calendar dates spanned by each night

3. **Check Date Overlap:**
   ```
   dates_a = [Oct 24, Oct 25]
   dates_b = [Oct 25]
   → Intersection = [Oct 25] → Same calendar night ✓
   ```

4. **Check Time Range Overlap:**
   Two ranges `[start_a, end_a]` and `[start_b, end_b]` overlap if:
   ```
   start_a < end_b AND start_b < end_a
   ```

5. **Return Result:**
   ```
   same_calendar_night AND time_ranges_overlap
   ```

### Example Scenarios

**Scenario 1: Matching Nights**
```
Night A: 2024-10-25 01:00:00Z → 2024-10-25 03:33:00Z
Night B: 2024-10-24 19:34:00Z → 2024-10-25 04:44:00Z

Calendar dates:
  A = [Oct 25]
  B = [Oct 24, Oct 25]
  Intersection = [Oct 25] ✓

Time overlap:
  01:00 < 04:44 AND 19:34 < 03:33 ✓

Result: MATCH
```

**Scenario 2: Same Date, No Time Overlap**
```
Night A: 2024-10-25 01:00:00Z → 2024-10-25 03:00:00Z
Night B: 2024-10-25 05:00:00Z → 2024-10-25 07:00:00Z

Calendar dates: [Oct 25] = [Oct 25] ✓

Time overlap:
  01:00 < 07:00 ✓ BUT 05:00 < 03:00 ✗

Result: NO MATCH
```

**Scenario 3: Different Dates**
```
Night A: 2024-10-25 01:00:00Z → 2024-10-25 03:00:00Z
Night B: 2024-10-26 01:00:00Z → 2024-10-26 03:00:00Z

Calendar dates: [Oct 25] ∩ [Oct 26] = ∅ ✗

Result: NO MATCH
```

## Time Range Union

### Purpose
When merging overlapping nights, calculate the combined time range that encompasses both.

### Implementation

```rust
pub fn calculate_time_range_union(
    time_a_start, time_a_end,
    time_b_start, time_b_end
) -> Result<(String, String)>
```

**Algorithm:**

1. Parse all four timestamps
2. Find earliest start: `min(start_a, start_b)`
3. Find latest end: `max(end_a, end_b)`
4. Return as (union_start, union_end)

**Example:**
```
Night A: 01:00 → 03:00
Night B: 02:00 → 05:00
Union:   01:00 → 05:00
```

## Merge Algorithm

### Purpose
Merge source frame set into target frame set, combining nights and sessions while avoiding duplicates.

### Implementation

```rust
pub async fn merge_frame_sets(source_id, target_id) -> Result<FrameSetDetail>
```

**Algorithm:**

1. **Validation:**
   - Ensure source ≠ target
   - Get all nights from source and target sets

2. **For Each Source Night:**

   a. **Find Matching Target Night:**
      - Use `find_matching_night()` algorithm
      - If match found → Merge sessions
      - If no match → Reassign night

   b. **If Match Found:**
      ```
      1. Calculate time range union
      2. Update target night time range
      3. Get all sessions from source night
      4. Move sessions to target night (UPDATE imaging_night_id)
      ```

   c. **If No Match:**
      ```
      1. Reassign night to target frame set (UPDATE frames_set_id)
      ```

3. **Deduplication:**
   ```
   1. Find all sessions in target frame set
   2. For each session:
      - Find duplicate frame_ids
      - DELETE all occurrences
      - INSERT single occurrence
   3. Return count of duplicates removed
   ```

4. **Recalculate Target Metadata:**
   ```
   1. Get all frame IDs in target set
   2. Calculate metadata (see Metadata Calculation)
   3. Mark target as is_custom = true
   4. UPDATE frames_set record
   ```

5. **Delete Source:**
   ```
   DELETE FROM frames_set WHERE id = source_id
   (Cascade deletes empty nights)
   ```

6. **Return Updated Target**

### Merge Complexity

- Time: O(N_source × N_target + F) where:
  - N_source = nights in source
  - N_target = nights in target
  - F = total frames (for deduplication)

- Space: O(N + F) for storing nights and frames

## Split Algorithm

### Purpose
Split selected items (nights/sessions/frames) from a source frame set into a new frame set.

### Implementation

```rust
pub async fn split_frame_set(
    source_set_id,
    selection: SplitSelection,
    new_name
) -> Result<FrameSetDetail>
```

**Algorithm:**

1. **Validation:**
   ```rust
   can_split(source_set_id, selection)
   ```
   - Ensure split won't leave source empty
   - Return error if all items selected

2. **Collect Frame IDs:**

   Based on selection type:

   a. **Nights:**
      ```sql
      SELECT DISTINCT sm.frame_id
      FROM session_members sm
      JOIN sessions s ON sm.session_id = s.id
      WHERE s.imaging_night_id IN (selected_night_ids)
      ```

   b. **Sessions:**
      ```sql
      SELECT sm.frame_id
      FROM session_members sm
      WHERE sm.session_id IN (selected_session_ids)
      ```

   c. **Frames:**
      ```
      Use selected frame IDs directly
      ```

3. **Create New Frame Set:**
   ```
   1. Calculate metadata from collected frame IDs
   2. Create frames_set record (is_custom = true)
   3. Detect nights/sessions from frames
   4. Create imaging_nights records
   5. Create sessions records
   6. INSERT session_members
   ```

4. **Remove from Source:**

   Based on selection type:

   a. **Nights:**
      ```sql
      DELETE FROM imaging_nights
      WHERE id IN (selected_night_ids)
      ```

   b. **Sessions:**
      ```sql
      DELETE FROM sessions
      WHERE id IN (selected_session_ids)
      ```

   c. **Frames:**
      ```sql
      DELETE FROM session_members
      WHERE frame_id IN (selected_frame_ids)
      ```

5. **Recalculate Source Metadata:**
   ```
   1. Get remaining frame IDs
   2. Calculate new metadata
   3. Mark source as is_custom = true
   4. UPDATE frames_set record
   ```

6. **Return New Frame Set**

### Split Validation

```rust
pub async fn can_split(source_set_id, selection) -> Result<bool>
```

**Algorithm:**

1. Count total items in source (nights/sessions/frames)
2. Count selected items
3. Return `selected < total`

**Example:**
```
Total nights: 5
Selected: 3
Result: 3 < 5 → true (can split)

Total nights: 5
Selected: 5
Result: 5 < 5 → false (cannot split - would be empty)
```

## Deduplication Algorithm

### Purpose
Remove duplicate frame references within a frame set after merge or other operations.

### Implementation

```rust
pub fn deduplicate_session_members_in_set(frames_set_id) -> Result<usize>
```

**Algorithm:**

1. **Get All Sessions:**
   ```sql
   SELECT s.id
   FROM sessions s
   JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
   WHERE in_tbl.frames_set_id = ?1
   ```

2. **For Each Session:**

   a. **Find Duplicates:**
      ```sql
      SELECT frame_id, COUNT(*) as count
      FROM session_members
      WHERE session_id = ?1
      GROUP BY frame_id
      HAVING count > 1
      ```

   b. **For Each Duplicate:**
      ```sql
      -- Remove all
      DELETE FROM session_members
      WHERE session_id = ?1 AND frame_id = ?2

      -- Insert exactly one
      INSERT INTO session_members (session_id, frame_id)
      VALUES (?1, ?2)
      ```

3. **Return Total Duplicates Removed**

### Deduplication Complexity

- Time: O(S × F) where:
  - S = sessions in frame set
  - F = average frames per session

## Metadata Calculation Algorithm

### Purpose
Calculate aggregated metadata for a frame set from its member frames.

### Implementation

```rust
pub fn calculate_metadata_from_frame_ids(frame_ids, conn) -> Result<FrameSetMetadata>
```

**Algorithm:**

1. **Query Frames:**
   ```sql
   SELECT * FROM frames WHERE id IN (frame_ids)
   ```

2. **Calculate Total Exposure:**
   ```rust
   total_exp_time = frames.iter()
       .filter_map(|f| f.exptime)
       .sum()
   ```

3. **Calculate Date Range:**
   ```rust
   dates = frames.iter()
       .filter_map(|f| f.date_obs)
       .collect()

   date_obs_start = dates.min()
   date_obs_end = dates.max()
   ```

4. **Calculate Average Coordinates:**

   a. **Collect Coordinate Pairs:**
      ```rust
      for frame in frames {
          if let (Some(ra), Some(dec)) = (frame.ra, frame.dec) {
              coords.push((ra, dec))
          } else if let (Some(ra_str), Some(dec_str)) = (frame.objctra, frame.objctdec) {
              // Parse sexagesimal strings
              coords.push((parse_ra(ra_str), parse_dec(dec_str)))
          }
      }
      ```

   b. **Spherical Mean:**
      ```rust
      pub fn spherical_mean(coords: &[(f64, f64)]) -> Result<(f64, f64)>
      ```

      Uses proper spherical geometry:
      ```
      1. Convert (RA, Dec) to Cartesian (x, y, z)
      2. Calculate mean vector (x̄, ȳ, z̄)
      3. Normalize mean vector
      4. Convert back to (RA, Dec)
      ```

   c. **Format as Sexagesimal:**
      ```rust
      objctra = format_ra_sexagesimal(ra_deg)   // "HH:MM:SS.S"
      objctdec = format_dec_sexagesimal(dec_deg) // "±DD:MM:SS.S"
      ```

5. **Return FrameSetMetadata:**
   ```rust
   FrameSetMetadata {
       date_obs_start,
       date_obs_end,
       objctra,
       objctdec,
       total_exp_time,
   }
   ```

### Spherical Mean Details

**Why Not Simple Average?**
Simple averaging fails near RA=0°/360° boundary and at poles.

**Proper Method:**
1. Convert to unit vectors on celestial sphere
2. Average vectors
3. Convert back to spherical coordinates

**Example:**
```
RA values: 359°, 1°
Simple average: (359 + 1) / 2 = 180° ✗ (wrong!)
Spherical mean: 0° ✓ (correct)
```

## Session Detection Algorithm

### Purpose
Group frames into imaging nights and sessions based on time gaps and instrument.

### Implementation

```rust
pub fn detect_sessions(frames, gap_threshold_hours) -> Result<Vec<ImagingNight>>
```

**Algorithm:**

1. **Filter Valid Frames:**
   - Keep only frames with valid date_obs

2. **Sort by Time:**
   ```rust
   frames.sort_by_key(|f| f.date_obs)
   ```

3. **Detect Night Boundaries:**
   ```rust
   for each pair of consecutive frames:
       time_gap = frame[i+1].date_obs - frame[i].date_obs
       if time_gap > gap_threshold:
           create new night
       else:
           add to current night
   ```

4. **Group by Instrument:**
   ```rust
   for each night:
       for each unique instrume:
           create session
           add frames with matching instrume
   ```

5. **Calculate Session Metadata:**
   - frame_count = frames.len()
   - total_exp_time = frames.sum(exptime)

6. **Return Detected Nights**

### Gap Threshold

Default: 6.0 hours

**Rationale:**
- Typical astronomical observation: sunset to sunrise (~12 hours)
- 6-hour gap likely indicates separate observing sessions
- Configurable via settings for different use cases

## Performance Characteristics

### Time Complexity Summary

| Operation | Complexity | Notes |
|-----------|------------|-------|
| Night Matching | O(D₁ × D₂) | D = days spanned, typically D ≤ 2 |
| Merge | O(N₁ × N₂ + F) | N = nights, F = frames |
| Split | O(F + S) | F = selected frames, S = sessions |
| Deduplicate | O(S × F) | S = sessions, F = frames per session |
| Metadata Calc | O(F) | F = frames in set |
| Session Detect | O(F log F) | F = frames (sorting dominates) |

### Space Complexity Summary

| Operation | Complexity | Notes |
|-----------|------------|-------|
| Night Matching | O(1) | Constant date arrays |
| Merge | O(N + F) | N nights + F frames |
| Split | O(F) | Frame ID collection |
| Deduplicate | O(S) | Session ID array |
| Metadata Calc | O(F) | Frame array |
| Session Detect | O(F) | Frame array |

## Algorithm Trade-offs

### Night Matching: Calendar Date + Time Overlap

**Pros:**
- Handles multi-day observations correctly
- Handles overlapping observations from different instruments
- Clear, unambiguous matching criteria

**Cons:**
- More complex than simple date comparison
- Requires parsing timestamps

**Alternative Considered:** Match by calendar date only
- Rejected: Would merge non-overlapping observations on same night

### Deduplication: Delete + Reinsert

**Pros:**
- Simple, guaranteed correctness
- Works with any number of duplicates

**Cons:**
- Not optimized for case with only 2 duplicates

**Alternative Considered:** Keep first, delete rest
- Rejected: More complex query logic, minimal performance gain

### Spherical Mean: Vector averaging

**Pros:**
- Mathematically correct for celestial coordinates
- Handles RA wrap-around correctly
- Handles polar regions correctly

**Cons:**
- More computationally expensive than simple average

**Alternative Considered:** Simple arithmetic mean
- Rejected: Produces incorrect results near RA boundaries
