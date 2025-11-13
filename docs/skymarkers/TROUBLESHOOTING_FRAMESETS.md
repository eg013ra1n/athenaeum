# Troubleshooting Frame Set Display Issues

**Date:** 2025-11-13
**Version:** 1.0
**Status:** Diagnostic Guide

---

## Table of Contents

1. [Overview](#overview)
2. [Quick Diagnostic Checklist](#quick-diagnostic-checklist)
3. [Root Cause Analysis](#root-cause-analysis)
4. [Common Failure Modes](#common-failure-modes)
5. [Diagnostic SQL Queries](#diagnostic-sql-queries)
6. [Debug Logging Guide](#debug-logging-guide)
7. [Step-by-Step Troubleshooting](#step-by-step-troubleshooting)
8. [Solutions](#solutions)

---

## Overview

Frame sets may not appear on the SkyAtlas map even though they exist in the database. This guide provides comprehensive diagnostics to identify and fix the root cause.

**Symptom:** Frame set exists in database but is not visible on SkyAtlas map

**Possible Causes:**
1. Incomplete database hierarchy (missing nights, sessions, or members)
2. Frames missing coordinate data (NULL RA or DEC)
3. Only calibration frames in set (no LIGHT frames)
4. Session detection failed during creation
5. Coordinate validation failures

---

## Quick Diagnostic Checklist

Run these checks in order:

### ✅ 1. Verify Frame Set Exists

```sql
SELECT id, name FROM frames_set;
```

**Expected:** One or more rows
**If empty:** No frame sets created yet, create via auto-generate or custom commands

### ✅ 2. Check Backend Output

**Terminal log should show:**
```
Found X imaging locations (Y framesets, Z clusters)
```

**If Y = 0:** Frame sets aren't being returned by query (continue diagnostics)
**If Y > 0:** Frame sets returned but not displaying (frontend issue)

### ✅ 3. Check Hierarchy Completeness

```sql
SELECT
    fs.id,
    fs.name,
    COUNT(DISTINCT ino.id) as night_count,
    COUNT(DISTINCT s.id) as session_count,
    COUNT(DISTINCT sm.frame_id) as member_count
FROM frames_set fs
LEFT JOIN imaging_nights ino ON ino.frames_set_id = fs.id
LEFT JOIN sessions s ON s.imaging_night_id = ino.id
LEFT JOIN session_members sm ON sm.session_id = s.id
GROUP BY fs.id;
```

**Expected:** All counts > 0
**If any count = 0:** Hierarchy incomplete (see [Common Failure Modes](#common-failure-modes))

### ✅ 4. Check Frame Coordinates

```sql
SELECT
    fs.id,
    fs.name,
    COUNT(*) as total_frames,
    SUM(CASE WHEN fr.ra IS NULL OR fr.dec IS NULL THEN 1 ELSE 0 END) as missing_coords,
    SUM(CASE WHEN fr.imagetyp = 'Light' THEN 1 ELSE 0 END) as light_frames
FROM frames_set fs
JOIN imaging_nights ino ON ino.frames_set_id = fs.id
JOIN sessions s ON s.imaging_night_id = ino.id
JOIN session_members sm ON sm.session_id = s.id
JOIN frames fr ON fr.id = sm.frame_id
GROUP BY fs.id;
```

**Expected:**
- `missing_coords = 0`
- `light_frames > 0`

**If missing_coords > 0:** Frames lack coordinate data
**If light_frames = 0:** Only calibration frames in set

### ✅ 5. Test Query Directly

Run the exact query from `get_imaging_locations`:

```sql
SELECT
    fs.id as frame_set_id,
    fs.name as object_name,
    AVG(fr.ra) as avg_ra,
    AVG(fr.dec) as avg_dec,
    COUNT(DISTINCT fr.id) as frame_count
FROM frames_set fs
JOIN imaging_nights ino ON ino.frames_set_id = fs.id
JOIN sessions s ON s.imaging_night_id = ino.id
JOIN session_members sm ON sm.session_id = s.id
JOIN frames fr ON fr.id = sm.frame_id
WHERE fr.ra IS NOT NULL
  AND fr.dec IS NOT NULL
  AND fr.imagetyp = 'Light'
GROUP BY fs.id
HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL;
```

**Expected:** One row per visible frame set
**If empty:** Frame set doesn't meet query conditions

---

## Root Cause Analysis

### The Complete Hierarchy Requirement

For a frame set to appear on the map, the following **complete chain** must exist:

```
frames_set (id, name, project_id)
    ↓ [JOIN on frames_set_id]
imaging_nights (id, frames_set_id, night_date)
    ↓ [JOIN on imaging_night_id]
sessions (id, imaging_night_id, instrume, ...)
    ↓ [JOIN on session_id]
session_members (session_id, frame_id)
    ↓ [JOIN on frame_id]
frames (id, ra, dec, imagetyp='Light')
```

**Critical:** All four JOINs are **INNER joins**. If ANY link is missing, the entire chain breaks and the frame set becomes invisible.

### Why Sessions Matter

The `get_imaging_locations` query specifically requires:
- `imaging_nights` table entries for the frame set
- `sessions` table entries for those nights
- `session_members` junction table entries linking frames to sessions

**Session Detection Process:**

When a frame set is created (auto or custom), the system calls `detect_sessions()`:

**Location:** `commands.rs`, function `detect_sessions`, lines 194-346

**Process:**
1. Groups frames by `imaging_nights` (using date_obs)
2. Within each night, groups frames by instrument/setup into sessions
3. Creates `imaging_nights` records
4. Creates `sessions` records
5. Creates `session_members` records

**If this fails:** Frame set exists but has no nights/sessions/members → invisible on map

---

## Common Failure Modes

### Mode 1: Frame Set Created But Empty Hierarchy

**Symptoms:**
```sql
-- Frame set exists
SELECT COUNT(*) FROM frames_set;  -- Returns: 1

-- But no imaging nights
SELECT COUNT(*) FROM imaging_nights WHERE frames_set_id = 1;  -- Returns: 0
```

**Cause:** Session detection failed or was skipped during creation

**Impact:** Frame set invisible (0 rows in query result)

**How It Happens:**
- Custom frame set created without calling `detect_sessions()`
- All frames have NULL or invalid `date_obs` (can't group by night)
- Error during session detection was silently ignored

**Solution:** Re-run session detection (see [Solutions](#solutions))

---

### Mode 2: Nights Exist But No Sessions

**Symptoms:**
```sql
-- Frame set and nights exist
SELECT COUNT(*) FROM imaging_nights WHERE frames_set_id = 1;  -- Returns: 3

-- But no sessions
SELECT COUNT(*) FROM sessions s
JOIN imaging_nights ino ON s.imaging_night_id = ino.id
WHERE ino.frames_set_id = 1;  -- Returns: 0
```

**Cause:** Session creation failed during detection

**Impact:** Frame set invisible (JOIN chain breaks at sessions)

**How It Happens:**
- All frames in night have NULL `instrume` or other session grouping fields
- Error during session INSERT
- Transaction rolled back partway through

**Solution:** Re-run session detection or manually create sessions

---

### Mode 3: Sessions Exist But No Members

**Symptoms:**
```sql
-- Frame set, nights, and sessions exist
SELECT COUNT(*) FROM sessions s
JOIN imaging_nights ino ON s.imaging_night_id = ino.id
WHERE ino.frames_set_id = 1;  -- Returns: 5

-- But no session members
SELECT COUNT(*) FROM session_members sm
JOIN sessions s ON sm.session_id = s.id
JOIN imaging_nights ino ON s.imaging_night_id = ino.id
WHERE ino.frames_set_id = 1;  -- Returns: 0
```

**Cause:** Frame-to-session linking failed

**Impact:** Frame set invisible (no frames to aggregate)

**How It Happens:**
- `session_members` table not populated during creation
- Frame IDs changed after sessions were created
- Manual session creation without member insertion

**Solution:** Repopulate session_members table

---

### Mode 4: Frames Have NULL Coordinates

**Symptoms:**
```sql
-- Complete hierarchy exists
-- But frames missing coordinates
SELECT COUNT(*) FROM session_members sm
JOIN frames fr ON sm.frame_id = fr.id
WHERE fr.ra IS NULL OR fr.dec IS NULL;  -- Returns: 120 (all frames)
```

**Cause:** FITS files lack coordinate metadata or parsing failed

**Impact:** Frame set invisible (WHERE clause filters out all frames)

**How It Happens:**
- FITS files don't have RA/DEC or OBJCTRA/OBJCTDEC keywords
- Coordinate parsing errors during file scan
- Coordinates in wrong format (not recognized by parser)
- Database created before coordinate parsing fixes

**Solution:**
1. Re-scan FITS files with fixed parser
2. Verify FITS headers contain coordinate keywords
3. Check coordinate normalization (see `docs/db/COORDINATE_ISSUES.md`)

---

### Mode 5: Only Calibration Frames

**Symptoms:**
```sql
-- Complete hierarchy exists
-- Frames have coordinates
-- But all frames are calibration
SELECT imagetyp, COUNT(*) FROM session_members sm
JOIN frames fr ON sm.frame_id = fr.id
JOIN sessions s ON sm.session_id = s.id
JOIN imaging_nights ino ON s.imaging_night_id = ino.id
WHERE ino.frames_set_id = 1
GROUP BY imagetyp;

-- Returns:
-- Dark   | 50
-- Flat   | 30
-- Bias   | 20
-- Light  | 0  ← Problem!
```

**Cause:** Frame set contains only Dark, Flat, Bias frames

**Impact:** Frame set invisible (WHERE clause: `imagetyp = 'Light'`)

**How It Happens:**
- Incorrect frame set creation (added calibration frames by mistake)
- Frame type misclassified in FITS headers
- User created calibration-only frame set

**Solution:**
- Frame sets should only contain LIGHT frames (this is by design)
- Remove calibration frames from set or create proper LIGHT frame set

---

### Mode 6: Custom Frame Set Without Session Detection

**Symptoms:**
```sql
-- Custom frame set created via manual command
-- Direct frame IDs provided
-- But hierarchy never created
```

**Cause:** Custom frame set creation didn't call `detect_sessions()`

**Impact:** Frame set invisible

**How It Happens:**
- Old implementation of custom frame set command
- Manual database manipulation (bypassing Rust code)
- Error during `detect_sessions()` call

**Solution:** Update custom frame set command to call `detect_sessions()`

---

## Diagnostic SQL Queries

### Query 1: List All Frame Sets with Completeness Status

```sql
SELECT
    fs.id,
    fs.name,
    fs.project_id,
    COUNT(DISTINCT ino.id) as imaging_nights,
    COUNT(DISTINCT s.id) as sessions,
    COUNT(DISTINCT sm.frame_id) as total_frames,
    SUM(CASE WHEN fr.imagetyp = 'Light' THEN 1 ELSE 0 END) as light_frames,
    SUM(CASE WHEN fr.ra IS NOT NULL AND fr.dec IS NOT NULL THEN 1 ELSE 0 END) as frames_with_coords,
    CASE
        WHEN COUNT(DISTINCT ino.id) = 0 THEN 'Missing imaging_nights'
        WHEN COUNT(DISTINCT s.id) = 0 THEN 'Missing sessions'
        WHEN COUNT(DISTINCT sm.frame_id) = 0 THEN 'Missing session_members'
        WHEN SUM(CASE WHEN fr.imagetyp = 'Light' THEN 1 ELSE 0 END) = 0 THEN 'No LIGHT frames'
        WHEN SUM(CASE WHEN fr.ra IS NOT NULL AND fr.dec IS NOT NULL THEN 1 ELSE 0 END) = 0 THEN 'No coordinates'
        ELSE 'OK - Should be visible'
    END as status
FROM frames_set fs
LEFT JOIN imaging_nights ino ON ino.frames_set_id = fs.id
LEFT JOIN sessions s ON s.imaging_night_id = ino.id
LEFT JOIN session_members sm ON sm.session_id = s.id
LEFT JOIN frames fr ON fr.id = sm.frame_id
GROUP BY fs.id
ORDER BY fs.id;
```

**Interpretation:**
- **Status = 'OK - Should be visible'**: Frame set should appear on map
- **Status = anything else**: Indicates specific problem

### Query 2: Find Frames Outside Sessions

```sql
-- Frames with coordinates not in any session
SELECT
    fr.id,
    fr.object,
    fr.ra,
    fr.dec,
    fr.date_obs,
    fr.imagetyp
FROM frames fr
WHERE fr.imagetyp = 'Light'
  AND fr.ra IS NOT NULL
  AND fr.dec IS NOT NULL
  AND NOT EXISTS (
      SELECT 1 FROM session_members sm WHERE sm.frame_id = fr.id
  )
ORDER BY fr.date_obs;
```

**Use Case:** Find frames that should be organized but aren't

### Query 3: Validate Average Coordinates

```sql
-- For each frame set, show coordinate range
SELECT
    fs.id,
    fs.name,
    MIN(fr.ra) as min_ra,
    MAX(fr.ra) as max_ra,
    AVG(fr.ra) as avg_ra,
    MIN(fr.dec) as min_dec,
    MAX(fr.dec) as max_dec,
    AVG(fr.dec) as avg_dec,
    MAX(fr.ra) - MIN(fr.ra) as ra_spread,
    MAX(fr.dec) - MIN(fr.dec) as dec_spread
FROM frames_set fs
JOIN imaging_nights ino ON ino.frames_set_id = fs.id
JOIN sessions s ON s.imaging_night_id = ino.id
JOIN session_members sm ON sm.session_id = s.id
JOIN frames fr ON fr.id = sm.frame_id
WHERE fr.imagetyp = 'Light'
GROUP BY fs.id;
```

**Check:**
- Large `ra_spread` or `dec_spread` (>1°) may indicate mixed targets
- Verify `avg_ra` and `avg_dec` are reasonable values

### Query 4: Session Detection Simulation

```sql
-- Simulate what detect_sessions() would create
-- Groups frames by date and instrument
SELECT
    DATE(fr.date_obs) as night,
    fr.instrume,
    COUNT(*) as frame_count,
    GROUP_CONCAT(fr.id) as frame_ids
FROM frames fr
WHERE fr.id IN (
    -- Replace with your frame IDs
    SELECT frame_id FROM some_frame_set_source
)
GROUP BY DATE(fr.date_obs), fr.instrume
ORDER BY night, instrume;
```

**Use Case:** Preview how frames will be grouped before creating frame set

### Query 5: Compare Query Result vs Database

```sql
-- What query WOULD return vs what database HAS
WITH query_result AS (
    SELECT
        fs.id as frame_set_id,
        fs.name as object_name,
        COUNT(DISTINCT fr.id) as frame_count
    FROM frames_set fs
    JOIN imaging_nights ino ON ino.frames_set_id = fs.id
    JOIN sessions s ON s.imaging_night_id = ino.id
    JOIN session_members sm ON sm.session_id = s.id
    JOIN frames fr ON fr.id = sm.frame_id
    WHERE fr.ra IS NOT NULL
      AND fr.dec IS NOT NULL
      AND fr.imagetyp = 'Light'
    GROUP BY fs.id
    HAVING COUNT(DISTINCT fr.id) > 0
),
database_sets AS (
    SELECT id, name FROM frames_set
)
SELECT
    ds.id,
    ds.name,
    qr.frame_count,
    CASE
        WHEN qr.frame_set_id IS NULL THEN 'NOT IN QUERY RESULT'
        ELSE 'OK'
    END as status
FROM database_sets ds
LEFT JOIN query_result qr ON ds.id = qr.frame_set_id;
```

**Interpretation:**
- Frame sets with 'NOT IN QUERY RESULT' won't appear on map

---

## Debug Logging Guide

### Backend Logs (Terminal)

**Location:** Terminal where `npm run tauri dev` is running

**Key Log Lines:**

1. **Query result summary:**
   ```
   Found X imaging locations (Y framesets, Z clusters)
   ```
   - Check if Y > 0 (frame sets returned)

2. **Session detection:**
   ```
   Detected N sessions across M nights
   ```
   - Logged during frame set creation
   - If missing or N=0, session detection failed

3. **Frame set creation:**
   ```
   Created frame set with ID X
   ```
   - Confirms frame set record inserted

4. **SQL errors:**
   ```
   Error executing query: ...
   ```
   - Indicates database operation failure

### Frontend Logs (Browser Console)

**Key Log Lines:**

1. **Locations fetched:**
   ```javascript
   console.log('Received locations:', locations.length);
   ```
   - Check if > 0

2. **Validation results:**
   ```javascript
   console.log('Adding', validLocs.length, 'imaging location markers');
   ```
   - Check if locations passed coordinate validation

3. **Feature creation:**
   ```javascript
   console.log('Pre-transformed data features:', features.length);
   ```
   - Check if GeoJSON features created

4. **Marker rendering:**
   ```javascript
   console.log('Created', markers.size(), 'marker elements');
   ```
   - Confirms SVG markers rendered

### Enable Verbose Logging

**Backend:** Add debug prints in `get_imaging_locations`:

```rust
for location in &result {
    println!("Location: id={}, type={}, ra={:.4}, dec={:.4}, name={:?}",
        location.id,
        location.location_type,
        location.ra,
        location.dec,
        location.object_name
    );
}
```

**Frontend:** Add console logs in `addImagingMarkers`:

```typescript
console.log('Location data:', locations);
console.log('Valid locations:', validLocs);
console.log('Features:', features);
```

---

## Step-by-Step Troubleshooting

### Step 1: Verify Frame Set Exists

```sql
sqlite3 athenaeum.db "SELECT id, name FROM frames_set;"
```

**If empty:** Create frame set first
**If exists:** Continue to Step 2

### Step 2: Check Hierarchy

Run diagnostic Query 1 (Completeness Status).

**If status ≠ 'OK':** Follow specific failure mode instructions
**If status = 'OK':** Continue to Step 3

### Step 3: Check Backend Output

Look for terminal log: `Found X imaging locations (Y framesets, Z clusters)`

**If Y = 0 and frame set status is OK:** Run Query 5 (Compare Query vs Database) to find discrepancy
**If Y > 0:** Backend is working, continue to Step 4

### Step 4: Check Frontend

Open browser console and look for:
```
Adding X imaging location markers
```

**If X = 0:** Check network tab for Tauri IPC call response
**If X > 0:** Markers should be visible on map

### Step 5: Inspect Network

In browser DevTools → Network tab:
- Look for `get_imaging_locations` IPC call
- Check response JSON
- Verify `location_type: "frameset"` items exist

### Step 6: Check Coordinates

Run diagnostic Query 3 (Validate Average Coordinates).

Verify:
- avg_ra in range [0, 360)
- avg_dec in range [-90, 90]
- Coordinates match expected target position

### Step 7: Manual Query Test

Run the exact `get_imaging_locations` query (see Quick Diagnostic Checklist #5).

**If returns rows:** Backend query works, issue is elsewhere
**If empty:** Identify which condition filters out the frame set

---

## Solutions

### Solution 1: Re-run Session Detection

**For existing frame set without sessions:**

```rust
// Call from Tauri command or add to migration
let frame_ids: Vec<i64> = /* get frame IDs for frame set */;
let (nights, sessions) = detect_sessions(&conn, &frame_ids)?;

// Link sessions to frame set
for night in nights {
    // Update imaging_night.frames_set_id = your_frame_set_id
}
```

**Note:** This requires code modification. Alternatively, delete and recreate the frame set.

### Solution 2: Recreate Frame Set

**Safest option if hierarchy is broken:**

1. Note the frame set details (name, frames)
2. Delete the frame set:
   ```sql
   DELETE FROM frames_set WHERE id = X;
   -- CASCADE will delete nights, sessions, members
   ```
3. Recreate using auto-generate or custom command
4. Verify new frame set appears on map

### Solution 3: Re-scan Files with Fixed Parser

**If coordinates are NULL:**

1. Ensure coordinate parsing fixes are applied (see `docs/db/COORDINATE_ISSUES.md`)
2. Delete frames and re-scan:
   ```sql
   DELETE FROM frames WHERE file_id IN (
       SELECT id FROM files WHERE path LIKE '/your/path/%'
   );
   ```
3. Re-run scan from UI or command
4. Verify frames now have RA/DEC values
5. Recreate frame sets

### Solution 4: Manual Hierarchy Creation

**Advanced: Manual database repair**

```sql
-- 1. Create imaging night
INSERT INTO imaging_nights (frames_set_id, night_date)
VALUES (1, '2024-11-13');

-- 2. Create session
INSERT INTO sessions (imaging_night_id, instrume, focallen, xpixsz)
VALUES (1, 'ZWO ASI533MC Pro', 600, 3.76);

-- 3. Link frames to session
INSERT INTO session_members (session_id, frame_id)
SELECT 1, id FROM frames WHERE /* your criteria */;
```

**Warning:** Only use if you understand the data model. Prefer recreating frame sets.

### Solution 5: Update Custom Frame Set Command

**If using old custom frame set code:**

Ensure `create_custom_frames_set` or `create_frame_set_from_selection` calls `detect_sessions()`:

```rust
// After creating frames_set record
let (nights, sessions) = detect_sessions(&conn, &frame_ids)?;

// Create imaging_nights records
for (night_date, night_sessions) in nights {
    let night_id = /* insert imaging_night */;

    // Create sessions and link to night
    for session in night_sessions {
        let session_id = /* insert session */;

        // Create session_members
        for frame_id in session.frame_ids {
            /* insert session_member */;
        }
    }
}
```

---

## Prevention

### Best Practices

1. **Always use provided commands** (auto-generate, custom) instead of manual SQL
2. **Verify coordinates** before creating frame sets (check frames table)
3. **Test with small dataset** first before large batch operations
4. **Check backend logs** during frame set creation
5. **Run diagnostic queries** periodically to detect issues early

### Validation Checks

Add to frame set creation workflow:

```rust
// After creating frame set
let validation_query = "
    SELECT COUNT(*) FROM frames_set fs
    JOIN imaging_nights ino ON ino.frames_set_id = fs.id
    JOIN sessions s ON s.imaging_night_id = ino.id
    WHERE fs.id = ?1
";
let count: i64 = conn.query_row(validation_query, [frame_set_id], |row| row.get(0))?;

if count == 0 {
    return Err(anyhow!("Frame set created but has no sessions - hierarchy incomplete"));
}
```

---

## Related Documentation

- [MARKER_DISPLAY_SYSTEM.md](./MARKER_DISPLAY_SYSTEM.md) - How markers are rendered
- [SQL_QUERIES.md](./SQL_QUERIES.md) - Query breakdown
- [DATA_MODEL.md](./DATA_MODEL.md) - Database schema
- `../db/COORDINATE_ISSUES.md` - Coordinate parsing fixes

---

## Version History

**v1.0 (2025-11-13):**
- Initial diagnostic guide
- 6 common failure modes documented
- Complete SQL diagnostic queries
- Step-by-step troubleshooting workflow
