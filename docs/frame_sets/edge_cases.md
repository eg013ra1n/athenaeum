# Frame Sets Edge Cases

## Overview

This document describes edge cases, boundary conditions, and special scenarios in frame set operations, along with how the system handles them.

---

## Merge Edge Cases

### Empty Frame Sets

**Scenario:** Merging when source or target has no nights/sessions/frames.

**Behavior:**
```
Source: 0 nights
Target: 3 nights
→ Result: Target unchanged, source deleted
```

**Handling:**
- Loop over source nights (0 iterations)
- Deduplication runs but finds nothing
- Metadata recalculation succeeds (uses existing frames)
- Source deleted successfully

**Test Case:**
```rust
#[test]
fn test_merge_empty_source() {
    // Create source with no nights
    // Create target with nights
    // Merge should succeed
    assert!(merge_frame_sets(source_id, target_id).is_ok());
}
```

---

### Identical Frame Sets

**Scenario:** Source and target contain exactly the same frames.

**Behavior:**
```
Source: frames [1, 2, 3]
Target: frames [1, 2, 3]
→ Result: Target unchanged, source deleted
```

**Handling:**
1. Nights may or may not match depending on timestamps
2. Sessions merged or nights reassigned
3. Deduplication removes all duplicate frames
4. Result: Target has same frames, possibly reorganized nights
5. Metadata remains approximately same (coordinates, exposure)

**Why This Works:**
- Deduplication ensures each frame appears only once
- Metadata calculation is idempotent
- Frame references are preserved

---

### Partial Overlap

**Scenario:** Source and target share some frames.

**Behavior:**
```
Source: frames [1, 2, 3, 4, 5]
Target: frames [3, 4, 5, 6, 7]
→ Result: Target has frames [1, 2, 3, 4, 5, 6, 7]
```

**Handling:**
1. All nights from source are merged/reassigned
2. Frames 3, 4, 5 appear in both
3. Deduplication removes duplicates
4. Frames 1, 2 are new to target
5. Metadata recalculated with all unique frames

**Deduplication Example:**
```
Before dedup:
  Session A: frames [1, 2, 3]
  Session B: frames [3, 4, 5]  ← frame 3 appears twice

After dedup:
  Session A: frames [1, 2, 3]
  Session B: frames [4, 5]     ← duplicate removed
```

---

### Night Matching Ambiguity

**Scenario:** Multiple source nights could match the same target night.

**Example:**
```
Source Night 1: 2024-10-25 01:00 → 03:00
Source Night 2: 2024-10-25 02:00 → 04:00
Target Night:   2024-10-25 00:00 → 05:00
```

**Behavior:**
- Both source nights match the same target night
- Both get merged into target night
- Time range updated to union (earliest to latest)

**Handling:**
```rust
for source_night in source_nights {
    if let Some(target_night_id) = find_matching_night(source_night, &target_nights) {
        // Both merge into same target_night_id
        move_sessions_to_night(&session_ids, target_night_id);
    }
}
```

**Result:**
```
Target Night: 2024-10-25 00:00 → 05:00
  ← Sessions from Source Night 1
  ← Sessions from Source Night 2
```

**Why This Works:**
- Multiple sessions in one night is valid
- Represents continuous observing session
- Time range union encompasses all observations

---

### Same Night, Different Instruments

**Scenario:** Source and target both have observations from same night but different instruments.

**Example:**
```
Source: Oct 25, ZWO ASI 294MM
Target: Oct 25, Canon EOS Ra
```

**Behavior:**
```
Before merge:
  Target Night (Oct 25):
    Session A: ZWO ASI 294MM (from target)

After merge:
  Target Night (Oct 25):
    Session A: ZWO ASI 294MM (from target)
    Session B: Canon EOS Ra (from source)
```

**Handling:**
- Nights match (same date, overlapping times)
- Sessions moved into same night
- Each instrument has separate session
- Both sessions coexist in merged night

**Use Case:**
- Imaging with multiple cameras on same night
- Different filters on different instruments
- Parallel observations

---

### No Matching Nights

**Scenario:** Source nights don't match any target nights.

**Example:**
```
Source: Oct 25, 26, 27
Target: Nov 1, 2, 3
```

**Behavior:**
```
Before merge:
  Source: 3 nights
  Target: 3 nights

After merge:
  Target: 6 nights (all reassigned from source)
```

**Handling:**
```rust
for source_night in source_nights {
    let match = find_matching_night(source_night, &target_nights);
    if match.is_none() {
        // Reassign entire night to target
        reassign_imaging_night_to_frame_set(source_night_id, target_id);
    }
}
```

**Why Reassignment:**
- Preserves complete night structure
- Avoids unnecessary session moves
- Efficient database operation (single UPDATE)

---

### Cross-Midnight Observations

**Scenario:** Observation spans midnight (e.g., 23:00 → 02:00 next day).

**Example:**
```
Night A: 2024-10-24 23:00 → 2024-10-25 02:00
Night B: 2024-10-25 01:00 → 2024-10-25 04:00
```

**Behavior:**
- Nights match (both include Oct 25, times overlap)
- Merged into single night
- Time range: 2024-10-24 23:00 → 2024-10-25 04:00

**Handling:**
```rust
// Extract all calendar dates spanned
dates_a = [Oct 24, Oct 25]
dates_b = [Oct 25]
// Intersection non-empty → match
```

**Why This Works:**
- Astronomical observations naturally cross midnight
- Calendar date comparison handles multi-day spans
- Time range union preserves full extent

---

## Split Edge Cases

### Splitting All Items

**Scenario:** User tries to split all nights/sessions/frames.

**Behavior:**
```
Source: 5 nights
Selection: all 5 nights
→ Result: ERROR "Cannot split: operation would leave the source frame set empty"
```

**Handling:**
```rust
pub async fn can_split(source_set_id, selection) -> Result<bool> {
    let total_items = count_total_items(source_set_id, selection.type);
    let selected = selection.ids.len();
    Ok(selected < total_items)  // false if selected == total
}
```

**Prevention:**
- UI should not show split button if `can_split() == false`
- Backend validation rejects operation
- User must leave at least one item in source

**Rationale:**
- Prevents creating empty frame sets
- Empty sets have no meaningful metadata
- User should delete set if they want it gone

---

### Splitting Non-Existent Items

**Scenario:** Selection includes IDs that don't exist in the frame set.

**Example:**
```
Source: nights [10, 11, 12]
Selection: nights [10, 99]  ← 99 doesn't exist
```

**Behavior:**
- Frame collection query returns frames for valid IDs only
- Invalid IDs silently ignored
- Split proceeds with available frames

**Handling:**
```sql
SELECT frame_id FROM session_members
WHERE imaging_night_id IN (10, 99)
→ Returns frames for night 10 only
```

**Why Silent Ignore:**
- User may have stale UI data
- Graceful degradation better than error
- Result is still valid (splits existing items)

**Alternative Considered:** Strict validation
- Rejected: Too fragile to UI race conditions
- Current approach: Idempotent and forgiving

---

### Splitting Single Frame

**Scenario:** Split just one frame from a large set.

**Example:**
```
Source: 1000 frames
Selection: frames [42]
```

**Behavior:**
```
Source: 999 frames (recalculated metadata)
New Set: 1 frame
```

**Handling:**
- Valid operation (1 < 1000)
- New set created with single frame
- Session detection may create single night/session
- Metadata calculated from one frame

**Use Case:**
- Isolating bad frame for review
- Extracting test frame
- Quality control workflow

---

### Splitting by Sessions in Multi-Instrument Night

**Scenario:** Night has multiple sessions (different instruments), split one session.

**Example:**
```
Night 1:
  Session A (ZWO): frames [1, 2, 3]
  Session B (Canon): frames [4, 5, 6]

Selection: sessions [A]
```

**Behavior:**
```
Source:
  Night 1:
    Session B (Canon): frames [4, 5, 6]

New Set:
  Night 1 (new):
    Session A (ZWO): frames [1, 2, 3]
```

**Handling:**
```sql
-- Remove from source
DELETE FROM sessions WHERE id = A

-- New set gets its own night
-- (session detection may merge or split based on timestamps)
```

**Note:**
- Source night persists (still has Session B)
- New set may or may not have separate night
- Depends on timestamps and gap threshold

---

### Splitting Creates Duplicate Nights

**Scenario:** After split, source and new set have nights with overlapping times.

**Example:**
```
Before split:
  Source Night: 2024-10-25 00:00 → 05:00 (frames 1-10)

Split (frames 1-5):
  Source Night: 2024-10-25 00:00 → 05:00 (frames 6-10)
  New Night: 2024-10-25 00:00 → 03:00 (frames 1-5)
```

**Behavior:**
- This is allowed and correct
- Source and new set are independent
- Nights in different sets can overlap
- Each represents actual observation times

**Why This is OK:**
- Frame sets are independent collections
- Same frames can theoretically be in multiple sets
- Timestamps reflect actual observation times
- No integrity violation

---

## Metadata Calculation Edge Cases

### No Coordinates

**Scenario:** Frames have no RA/Dec or OBJCTRA/OBJCTDEC.

**Behavior:**
```
Input: 10 frames, 0 with coordinates
Output: FrameSetMetadata {
    objctra: None,
    objctdec: None,
    ...
}
```

**Handling:**
```rust
let coords: Vec<(f64, f64)> = frames.iter()
    .filter_map(|f| extract_coordinates(f))
    .collect();

let (objctra, objctdec) = if !coords.is_empty() {
    spherical_mean(&coords)
} else {
    (None, None)
};
```

**Use Cases:**
- Frames from cameras without plate solving
- Test data without WCS
- Manual uploads

---

### Mixed Coordinate Formats

**Scenario:** Some frames have numeric RA/Dec, others have sexagesimal OBJCTRA/OBJCTDEC.

**Behavior:**
- Both formats parsed and converted to decimal degrees
- Combined for spherical mean calculation
- Output always in sexagesimal format

**Handling:**
```rust
let coord = if let (Some(ra), Some(dec)) = (frame.ra, frame.dec) {
    Some((ra, dec))  // Already decimal
} else if let (Some(ra_str), Some(dec_str)) = (&frame.objctra, &frame.objctdec) {
    Some((parse_ra_sexagesimal(ra_str)?, parse_dec_sexagesimal(dec_str)?))
} else {
    None
};
```

**Why This Works:**
- All coordinates normalized to decimal degrees
- Spherical mean operates on uniform format
- Output format standardized

---

### Coordinates Near RA Boundaries

**Scenario:** Frames near RA = 0°/360° boundary.

**Example:**
```
Frame 1: RA = 359° (23h 56m)
Frame 2: RA = 1°   (00h 04m)
```

**Naive Average:**
```
(359 + 1) / 2 = 180°  ✗ WRONG! (halfway around sky)
```

**Spherical Mean:**
```
1. Convert to unit vectors
2. Average vectors
3. Convert back
→ RA = 0° ✓ CORRECT!
```

**Handling:**
```rust
pub fn spherical_mean(coords: &[(f64, f64)]) -> Result<(f64, f64)> {
    let mut x = 0.0, y = 0.0, z = 0.0;

    for &(ra_deg, dec_deg) in coords {
        let (ra_rad, dec_rad) = (ra_deg.to_radians(), dec_deg.to_radians());
        x += dec_rad.cos() * ra_rad.cos();
        y += dec_rad.cos() * ra_rad.sin();
        z += dec_rad.sin();
    }

    let n = coords.len() as f64;
    let (x, y, z) = (x / n, y / n, z / n);

    let dec_rad = z.atan2((x * x + y * y).sqrt());
    let ra_rad = y.atan2(x);

    Ok((ra_rad.to_degrees().rem_euclid(360.0), dec_rad.to_degrees()))
}
```

**Why Necessary:**
- Celestial sphere is not flat
- Modular arithmetic breaks near boundaries
- Spherical geometry required for correctness

---

### Single Frame Metadata

**Scenario:** Frame set with only one frame.

**Behavior:**
```
Input: 1 frame
Output: FrameSetMetadata {
    date_obs_start: frame.date_obs,
    date_obs_end: frame.date_obs,
    objctra: frame.objctra,
    objctdec: frame.objctdec,
    total_exp_time: frame.exptime,
}
```

**Handling:**
```rust
let dates = vec![frame.date_obs];
date_obs_start = dates.min();  // = date_obs
date_obs_end = dates.max();    // = date_obs (same)
```

**Use Cases:**
- Single frame for testing
- Frame set with one survivor after split
- Isolated frame for quality review

---

### All Dates Missing

**Scenario:** No frames have date_obs.

**Behavior:**
```
Output: FrameSetMetadata {
    date_obs_start: None,
    date_obs_end: None,
    ...
}
```

**Handling:**
```rust
let dates: Vec<DateTime> = frames.iter()
    .filter_map(|f| f.date_obs)
    .collect();

let (start, end) = if !dates.is_empty() {
    (Some(dates.min()), Some(dates.max()))
} else {
    (None, None)
};
```

**Use Cases:**
- Imported frames without timestamp metadata
- Synthetic test data
- Legacy data

---

## Session Detection Edge Cases

### All Frames at Same Timestamp

**Scenario:** All frames have identical date_obs.

**Behavior:**
```
Input: 10 frames, all 2024-10-25 02:00:00
Output: 1 night, 1 session per instrument
```

**Handling:**
```rust
// Gap detection
for i in 0..frames.len()-1 {
    let gap = frames[i+1].date_obs - frames[i].date_obs;
    // gap = 0 (all same time)
    // 0 < threshold → all in same night
}
```

**Why This Works:**
- Zero gap < any threshold
- All frames grouped into single night
- Then grouped by instrument

---

### Frames Out of Order

**Scenario:** Frames not sorted by time in input.

**Behavior:**
- Session detection sorts frames first
- Produces correct nights regardless of input order

**Handling:**
```rust
pub fn detect_sessions(mut frames, gap_threshold) -> Result<Vec<Night>> {
    // Filter and sort
    frames.retain(|f| f.date_obs.is_some());
    frames.sort_by_key(|f| f.date_obs);
    // ... rest of algorithm
}
```

**Guaranteed:**
- Output always consistent
- Input order doesn't matter
- Idempotent operation

---

### Gap Exactly at Threshold

**Scenario:** Gap between frames exactly equals threshold.

**Example:**
```
Gap threshold: 6.0 hours
Frame 1: 00:00
Frame 2: 06:00  (gap = 6.0 hours exactly)
```

**Behavior:**
```rust
if gap > threshold {  // 6.0 > 6.0 is FALSE
    // New night
} else {
    // Same night  ← This branch
}
```

**Result:** Treated as same night (gap ≤ threshold).

**Rationale:**
- Inclusive boundary (≤ vs <)
- Conservative grouping
- Err on side of keeping together

---

## Deduplication Edge Cases

### Triple Duplicates

**Scenario:** Same frame appears 3+ times in a session.

**Example:**
```
session_members:
  (session=1, frame=42)
  (session=1, frame=42)
  (session=1, frame=42)
```

**Behavior:**
```sql
-- Delete all
DELETE FROM session_members WHERE session_id=1 AND frame_id=42
-- Insert one
INSERT INTO session_members (session_id, frame_id) VALUES (1, 42)
```

**Result:** Exactly one instance remains.

**Why Delete-All-Then-Insert:**
- Simple, guaranteed correctness
- Works for any N ≥ 1 duplicates
- Minimal code complexity

---

### No Duplicates

**Scenario:** Deduplication run on frame set with no duplicates.

**Behavior:**
```
Query finds 0 duplicates
0 iterations in dedup loop
Returns 0 duplicates removed
```

**Handling:**
```rust
let duplicates: Vec<(i64, i32)> = conn.prepare(
    "SELECT frame_id, COUNT(*) as count
     WHERE session_id = ?1
     GROUP BY frame_id
     HAVING count > 1"  // Empty result set
)?.query_map(...)?;

// duplicates.len() == 0
// for loop executes 0 times
```

**Performance:**
- Query still runs (small overhead)
- No actual modifications
- Idempotent and safe

---

## Transaction and Concurrency Edge Cases

### Concurrent Merges

**Scenario:** Two users merge same source into different targets simultaneously.

**Example:**
```
User A: merge(source=1, target=2)
User B: merge(source=1, target=3)
```

**Behavior:**
- One succeeds, one fails
- Winner deletes source
- Loser gets "Frame set not found" error

**Handling:**
- No explicit locking
- Database ACID guarantees
- Last delete wins

**Recommendation:**
- Show loading state during merge
- Retry on conflict (user decides)
- Not a critical issue (rare in practice)

---

### Mid-Operation Database Crash

**Scenario:** Application crashes during merge/split.

**Behavior:**
- SQLite transactions ensure atomicity
- Either fully completed or fully rolled back
- No partial states

**Recovery:**
- No manual intervention needed
- Frame set either merged or not
- Referential integrity maintained

---

## Validation Edge Cases

### Negative Frame IDs

**Scenario:** Invalid negative ID passed to commands.

**Behavior:**
```sql
SELECT * FROM frames WHERE id = -1
→ Returns empty result set
```

**Handling:**
- Query succeeds but returns no rows
- Treated as "not found"
- Error: "Frame set not found" or "No frames to split"

**Alternative Considered:** Input validation
- Rejected: Database already handles this
- Current approach: Let DB reject invalid IDs

---

### Very Large Frame Sets

**Scenario:** Frame set with 10,000+ frames.

**Performance:**
- Metadata calculation: O(F) ~ 100-200ms
- Deduplication: O(S × F) ~ 1-2 seconds
- UI rendering may be slow (pagination recommended)

**Handling:**
- No hard limits enforced
- Operations remain correct
- May need longer timeouts
- Consider pagination for UI

**Tested:**
- 10,000 frames: All operations work
- 50,000 frames: Slower but functional
- 100,000+ frames: Not tested

---

## Summary of Edge Case Strategies

### General Principles

1. **Graceful Degradation:** Handle missing data without errors
2. **Idempotency:** Operations produce same result when repeated
3. **Validation at Boundaries:** Check inputs, trust DB for constraints
4. **Delete-Insert Pattern:** Simple, guaranteed correctness
5. **Conservative Grouping:** Err on side of keeping together
6. **Let Database Enforce:** Use FK constraints, CASCADE, etc.

### When to Error vs. Degrade

**Error Cases:**
- Would violate logical invariant (empty frame set)
- User intent unclear (merge set into itself)
- Operation impossible (non-existent ID)

**Degradation Cases:**
- Missing metadata (use None/null)
- Partial data (use what's available)
- Non-critical failures (log and continue)

### Testing Strategy

All edge cases should have:
1. Unit test (algorithm level)
2. Integration test (database level)
3. UI test (user-facing behavior)
4. Documentation (this file!)
