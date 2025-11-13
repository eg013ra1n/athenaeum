# SkyAtlas SQL Queries

**Date:** 2025-11-13
**Version:** 1.0
**Status:** Documentation

---

## Table of Contents

1. [Overview](#overview)
2. [Complete Query Structure](#complete-query-structure)
3. [Branch 1: Frame Set Locations](#branch-1-frame-set-locations)
4. [Branch 2: Cluster Locations](#branch-2-cluster-locations)
5. [UNION Logic](#union-logic)
6. [FOV Calculation](#fov-calculation)
7. [Query Optimization](#query-optimization)
8. [Example Results](#example-results)

---

## Overview

The `get_imaging_locations` command executes a SQL query with UNION to fetch imaging locations from two sources:
1. **Frame Sets**: Organized sessions grouped into frame sets
2. **Clusters**: Unorganized frames grouped by sky coordinates

**Location:** `src-tauri/src/commands.rs`, lines 1454-1600

**Return Type:** `Vec<ImagingLocation>`

**Purpose:** Provide marker data for SkyAtlas visualization

---

## Complete Query Structure

```sql
-- Branch 1: Organized Frame Sets
SELECT
    fs.id as frame_set_id,
    fs.name as object_name,
    AVG(fr.ra) as avg_ra,
    AVG(fr.dec) as avg_dec,
    COUNT(DISTINCT fr.id) as frame_count,
    SUM(fr.exptime) as total_exposure,
    GROUP_CONCAT(DISTINCT fr.filter) as filters,
    MIN(fr.date_obs) as min_date,
    MAX(fr.date_obs) as max_date,
    'frameset' as location_type
FROM frames_set fs
JOIN imaging_nights ino ON ino.frames_set_id = fs.id
JOIN sessions s ON s.imaging_night_id = ino.id
JOIN session_members sm ON sm.session_id = s.id
JOIN frames fr ON fr.id = sm.frame_id
WHERE fr.ra IS NOT NULL
  AND fr.dec IS NOT NULL
  AND fr.imagetyp = 'Light'
GROUP BY fs.id
HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL

UNION ALL

-- Branch 2: Unorganized Clusters
SELECT
    NULL as frame_set_id,
    COALESCE(fr.object, 'Unknown') as object_name,
    AVG(fr.ra) as avg_ra,
    AVG(fr.dec) as avg_dec,
    COUNT(fr.id) as frame_count,
    SUM(fr.exptime) as total_exposure,
    GROUP_CONCAT(DISTINCT fr.filter) as filters,
    MIN(fr.date_obs) as min_date,
    MAX(fr.date_obs) as max_date,
    'cluster' as location_type
FROM frames fr
WHERE fr.ra IS NOT NULL
  AND fr.dec IS NOT NULL
  AND fr.imagetyp = 'Light'
  AND NOT EXISTS (
      SELECT 1 FROM session_members sm WHERE sm.frame_id = fr.id
  )
GROUP BY COALESCE(fr.object, 'Unknown'), ROUND(fr.ra, 1), ROUND(fr.dec, 1)
HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL
```

---

## Branch 1: Frame Set Locations

### Purpose

Fetch locations for **organized imaging sessions** that have been grouped into frame sets.

### Database Schema Hierarchy

Frame sets require this complete hierarchy:

```
frames_set (id, name, project_id)
  ↓
imaging_nights (id, frames_set_id, night_date)
  ↓
sessions (id, imaging_night_id, instrume, setup)
  ↓
session_members (session_id, frame_id)
  ↓
frames (id, ra, dec, object, exptime, filter, date_obs, imagetyp)
```

### Query Breakdown

#### SELECT Clause

```sql
SELECT
    fs.id as frame_set_id,           -- Frame set ID (used for navigation)
    fs.name as object_name,          -- Frame set name (e.g., "M31 Andromeda")
    AVG(fr.ra) as avg_ra,            -- Average RA of all frames
    AVG(fr.dec) as avg_dec,          -- Average DEC of all frames
    COUNT(DISTINCT fr.id) as frame_count,  -- Total frames in set
    SUM(fr.exptime) as total_exposure,     -- Total exposure time (seconds)
    GROUP_CONCAT(DISTINCT fr.filter) as filters,  -- Comma-separated filter list
    MIN(fr.date_obs) as min_date,    -- Earliest observation date
    MAX(fr.date_obs) as max_date,    -- Latest observation date
    'frameset' as location_type      -- Marker type discriminator
```

**Key Points:**
- `AVG(fr.ra)` and `AVG(fr.dec)`: Arithmetic average of frame coordinates
  - For proper spherical mean, see `coordinates::spherical_mean()`
  - Acceptable for frames within same target (small angular separation)
- `COUNT(DISTINCT fr.id)`: Ensures frames counted once even if in multiple sessions
- `GROUP_CONCAT(DISTINCT fr.filter)`: SQLite function, filters separated by comma

#### FROM and JOIN Clauses

```sql
FROM frames_set fs
JOIN imaging_nights ino ON ino.frames_set_id = fs.id
JOIN sessions s ON s.imaging_night_id = ino.id
JOIN session_members sm ON sm.session_id = s.id
JOIN frames fr ON fr.id = sm.frame_id
```

**Join Chain:**
1. Start with `frames_set` table
2. Join to `imaging_nights` via `frames_set_id`
3. Join to `sessions` via `imaging_night_id`
4. Join to `session_members` junction table
5. Join to `frames` via `frame_id`

**CRITICAL:** All joins are INNER joins (default). If ANY link is missing, the frame set will NOT appear in results.

**Example Missing Links:**
- Frame set exists but has no `imaging_nights` records → Not in results
- Imaging night exists but has no `sessions` → Not in results
- Session exists but has no `session_members` → Not in results

#### WHERE Clause

```sql
WHERE fr.ra IS NOT NULL
  AND fr.dec IS NOT NULL
  AND fr.imagetyp = 'Light'
```

**Filter Conditions:**
1. `fr.ra IS NOT NULL`: Frame must have RA coordinate
2. `fr.dec IS NOT NULL`: Frame must have DEC coordinate
3. `fr.imagetyp = 'Light'`: Only LIGHT frames (exclude calibration frames)

**Excluded Frame Types:**
- Dark frames (`imagetyp = 'Dark'`)
- Flat frames (`imagetyp = 'Flat'`)
- Bias frames (`imagetyp = 'Bias'`)
- Dark flat frames (`imagetyp = 'DarkFlat'`)

**Rationale:** Calibration frames don't have meaningful sky coordinates.

#### GROUP BY Clause

```sql
GROUP BY fs.id
```

**Aggregation Level:** One result row per frame set

**Effect:**
- All frames in all sessions in all nights for a frame set are aggregated
- Average coordinates computed across entire frame set
- Total frame count and exposure summed

#### HAVING Clause

```sql
HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL
```

**Post-Aggregation Filter:**
- Ensures averaged coordinates are valid
- Filters out frame sets where ALL frames had NULL coordinates
- Should always be true if WHERE clause succeeded, but acts as safety check

### Example Result

```rust
ImagingLocation {
    id: 1,
    frame_set_id: Some(5),
    ra: 10.684792,              // RA in degrees
    dec: 41.268750,             // DEC in degrees
    object_name: Some("M31 Andromeda"),
    frame_count: 120,
    total_exposure: 37800.0,    // 10.5 hours in seconds
    filters: vec!["Ha".to_string(), "OIII".to_string(), "SII".to_string()],
    date_range: ("2024-09-15T22:30:00Z".to_string(), "2024-10-20T23:45:00Z".to_string()),
    fov_width: Some(1.487),
    fov_height: Some(1.122),
    location_type: "frameset".to_string(),
}
```

---

## Branch 2: Cluster Locations

### Purpose

Fetch locations for **unorganized frames** that are NOT part of any imaging session.

### Query Breakdown

#### SELECT Clause

```sql
SELECT
    NULL as frame_set_id,            -- No frame set (cluster marker)
    COALESCE(fr.object, 'Unknown') as object_name,  -- Object name or 'Unknown'
    AVG(fr.ra) as avg_ra,            -- Average RA of clustered frames
    AVG(fr.dec) as avg_dec,          -- Average DEC of clustered frames
    COUNT(fr.id) as frame_count,     -- Total frames in cluster
    SUM(fr.exptime) as total_exposure,     -- Total exposure time (seconds)
    GROUP_CONCAT(DISTINCT fr.filter) as filters,  -- Comma-separated filter list
    MIN(fr.date_obs) as min_date,    -- Earliest observation date
    MAX(fr.date_obs) as max_date,    -- Latest observation date
    'cluster' as location_type       -- Marker type discriminator
```

**Key Differences from Branch 1:**
- `frame_set_id` is always NULL
- `COALESCE(fr.object, 'Unknown')`: Defaults to 'Unknown' if object name missing
- `COUNT(fr.id)` instead of `COUNT(DISTINCT fr.id)`: No risk of duplicates in this query

#### FROM Clause

```sql
FROM frames fr
```

**Simple Structure:** Direct query of frames table, no joins needed.

#### WHERE Clause

```sql
WHERE fr.ra IS NOT NULL
  AND fr.dec IS NOT NULL
  AND fr.imagetyp = 'Light'
  AND NOT EXISTS (
      SELECT 1 FROM session_members sm WHERE sm.frame_id = fr.id
  )
```

**Filter Conditions:**
1. `fr.ra IS NOT NULL`: Frame must have RA coordinate
2. `fr.dec IS NOT NULL`: Frame must have DEC coordinate
3. `fr.imagetyp = 'Light'`: Only LIGHT frames
4. **`NOT EXISTS` subquery**: Frame is NOT in any session

**NOT EXISTS Explanation:**
- Checks if frame ID appears in `session_members` table
- If YES → frame is organized, excluded from clusters
- If NO → frame is unorganized, included in clusters

This ensures **mutual exclusivity**: frames appear either in Branch 1 (frame sets) OR Branch 2 (clusters), never both.

#### GROUP BY Clause

```sql
GROUP BY COALESCE(fr.object, 'Unknown'), ROUND(fr.ra, 1), ROUND(fr.dec, 1)
```

**Clustering Strategy:**
- Group by **object name** (or 'Unknown')
- Group by **rounded RA** to nearest 0.1 degree (6 arcminutes)
- Group by **rounded DEC** to nearest 0.1 degree (6 arcminutes)

**Effect:**
- Frames of same object at similar coordinates are clustered
- Coordinate rounding creates ~6 arcmin grid cells
- Multiple clusters can exist for same object at different sky positions

**Example:**
- NGC 7000 frames at RA=312.5°, DEC=44.3° → Cluster A
- NGC 7000 frames at RA=312.8°, DEC=44.3° → Cluster B (different RA bin)

**Rationale:** Groups frames taken at approximately the same target position.

#### HAVING Clause

```sql
HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL
```

Same as Branch 1: ensures valid averaged coordinates.

### Example Result

```rust
ImagingLocation {
    id: 23,
    frame_set_id: None,
    ra: 312.534,                // RA in degrees
    dec: 44.321,                // DEC in degrees
    object_name: Some("NGC 7000"),
    frame_count: 45,
    total_exposure: 13500.0,    // 3.75 hours in seconds
    filters: vec!["Ha".to_string(), "OIII".to_string()],
    date_range: ("2024-08-10T21:15:00Z".to_string(), "2024-08-25T22:30:00Z".to_string()),
    fov_width: Some(2.456),
    fov_height: Some(1.852),
    location_type: "cluster".to_string(),
}
```

---

## UNION Logic

### UNION ALL vs UNION

**Query uses:** `UNION ALL`

**Reason:** Frame sets and clusters are mutually exclusive (ensured by NOT EXISTS clause), so no risk of duplicates. `UNION ALL` is more efficient than `UNION` (which deduplicates).

### Result Ordering

**No ORDER BY clause specified.**

**Default behavior:** Results returned in database engine order:
1. All frame set results
2. Then all cluster results

**Frontend sorting:** If needed, sorting happens in TypeScript after data is received.

### Performance Characteristics

**Expected Performance:**
- Fast for small to medium datasets (<1000 locations)
- Both branches use appropriate indexes
- NOT EXISTS subquery is efficient with proper indexing

**Potential Bottlenecks:**
- `GROUP_CONCAT` on large result sets (many filters)
- Multiple joins in Branch 1 (4 joins total)
- NOT EXISTS subquery in Branch 2 (depends on session_members size)

---

## FOV Calculation

### When FOV is Calculated

**Location:** `commands.rs`, lines 1547-1560

FOV is calculated **after** the UNION query executes, for each location with complete sensor metadata.

### Required Metadata

For each location, the query attempts to fetch:
- `XPIXSZ`: Pixel size in micrometers
- `FOCALLEN`: Focal length in millimeters
- `NAXIS1`: Sensor width in pixels
- `NAXIS2`: Sensor height in pixels
- `XBINNING`: Horizontal binning factor
- `YBINNING`: Vertical binning factor

**Source:** Aggregates from frames in the location (either set or cluster)

### Calculation Function

```rust
fn calculate_fov(
    pixel_size_um: f64,
    focal_len_mm: f64,
    sensor_pixels: f64,
    binning: f64
) -> f64 {
    let pixel_size_mm = pixel_size_um / 1000.0;
    let sensor_mm = pixel_size_mm * sensor_pixels * binning;
    let fov_radians = 2.0 * (sensor_mm / (2.0 * focal_len)).atan();
    fov_radians.to_degrees()
}
```

**Formula Explanation:**
1. Convert pixel size from μm to mm
2. Calculate sensor dimension in mm: `pixel_size * pixels * binning`
3. Calculate FOV using small angle approximation: `2 * arctan(sensor / (2 * focal_length))`
4. Convert from radians to degrees

### FOV in Query Results

**If metadata incomplete:**
- `fov_width: None`
- `fov_height: None`

**If metadata complete:**
- `fov_width: Some(width_degrees)`
- `fov_height: Some(height_degrees)`

**Frontend behavior:**
- No FOV → simple cross marker
- Has FOV → rectangle overlay (when zoomed in)

---

## Query Optimization

### Index Recommendations

**Critical indexes for performance:**

```sql
-- Frame set branch
CREATE INDEX idx_imaging_nights_fs ON imaging_nights(frames_set_id);
CREATE INDEX idx_sessions_night ON sessions(imaging_night_id);
CREATE INDEX idx_session_members_session ON session_members(session_id);
CREATE INDEX idx_session_members_frame ON session_members(frame_id);

-- Both branches
CREATE INDEX idx_frames_coords ON frames(ra, dec) WHERE ra IS NOT NULL AND dec IS NOT NULL;
CREATE INDEX idx_frames_imagetyp ON frames(imagetyp);

-- Cluster branch
CREATE INDEX idx_frames_object_coords ON frames(object, ra, dec);
```

**Rationale:**
- Join indexes ensure fast traversal through hierarchy
- Composite index on (ra, dec) supports WHERE clause filtering
- Partial index on imagetyp speeds up Light frame filtering

### Query Execution Plan

**Typical execution order:**

**Branch 1:**
1. Scan `frames_set` table
2. Index lookup on `imaging_nights` by `frames_set_id`
3. Index lookup on `sessions` by `imaging_night_id`
4. Index lookup on `session_members` by `session_id`
5. Index lookup on `frames` by `frame_id`
6. Filter by WHERE clause (imagetyp, coordinates)
7. Aggregate by frame set ID

**Branch 2:**
1. Scan `frames` table
2. Filter by WHERE clause (imagetyp, coordinates)
3. For each frame, check NOT EXISTS subquery:
   - Index lookup on `session_members` by `frame_id`
   - If found → exclude frame
4. Group by object, rounded coordinates
5. Aggregate

**Optimization opportunities:**
- Ensure `frames_set` table is not too large (limit by project)
- Consider materialized view for frequently accessed locations
- Add covering indexes if aggregation columns are accessed frequently

### Performance Benchmarks

**Expected query time:**
- Small dataset (<100 frame sets, <1000 frames): <50ms
- Medium dataset (<500 frame sets, <10,000 frames): <500ms
- Large dataset (<1000 frame sets, <50,000 frames): <2000ms

**If slower:**
- Check EXPLAIN QUERY PLAN output
- Verify indexes exist and are being used
- Consider query optimization or caching

---

## Example Results

### Sample Dataset

**Database state:**
- 3 frame sets (M31, M33, M42)
- 500 total frames
- 400 frames organized into sets
- 100 frames unorganized

### Query Results

```rust
vec![
    // Frame Set 1: M31 Andromeda
    ImagingLocation {
        id: 1,
        frame_set_id: Some(1),
        ra: 10.684792,
        dec: 41.268750,
        object_name: Some("M31 Andromeda"),
        frame_count: 120,
        total_exposure: 37800.0,
        filters: vec!["Ha", "OIII", "SII"],
        date_range: ("2024-09-15T22:30:00Z", "2024-10-20T23:45:00Z"),
        fov_width: Some(1.487),
        fov_height: Some(1.122),
        location_type: "frameset",
    },

    // Frame Set 2: M33 Triangulum
    ImagingLocation {
        id: 2,
        frame_set_id: Some(2),
        ra: 23.462083,
        dec: 30.660194,
        object_name: Some("M33 Triangulum"),
        frame_count: 180,
        total_exposure: 54000.0,
        filters: vec!["Ha", "OIII", "SII", "RGB"],
        date_range: ("2024-10-01T20:00:00Z", "2024-11-10T23:00:00Z"),
        fov_width: Some(1.487),
        fov_height: Some(1.122),
        location_type: "frameset",
    },

    // Frame Set 3: M42 Orion Nebula
    ImagingLocation {
        id: 3,
        frame_set_id: Some(3),
        ra: 83.822458,
        dec: -5.391111,
        object_name: Some("M42 Orion Nebula"),
        frame_count: 100,
        total_exposure: 15000.0,
        filters: vec!["Ha", "OIII"],
        date_range: ("2024-12-01T18:30:00Z", "2024-12-20T21:00:00Z"),
        fov_width: Some(1.487),
        fov_height: Some(1.122),
        location_type: "frameset",
    },

    // Cluster 1: NGC 7000 (unorganized)
    ImagingLocation {
        id: 4,
        frame_set_id: None,
        ra: 312.534,
        dec: 44.321,
        object_name: Some("NGC 7000"),
        frame_count: 45,
        total_exposure: 13500.0,
        filters: vec!["Ha", "OIII"],
        date_range: ("2024-08-10T21:15:00Z", "2024-08-25T22:30:00Z"),
        fov_width: Some(2.456),
        fov_height: Some(1.852),
        location_type: "cluster",
    },

    // Cluster 2: IC 1396 (unorganized)
    ImagingLocation {
        id: 5,
        frame_set_id: None,
        ra: 326.245,
        dec: 57.501,
        object_name: Some("IC 1396"),
        frame_count: 55,
        total_exposure: 16500.0,
        filters: vec!["Ha", "SII"],
        date_range: ("2024-07-15T23:00:00Z", "2024-07-30T01:30:00Z"),
        fov_width: None,  // No FOV data
        fov_height: None,
        location_type: "cluster",
    },
]
```

**Terminal Output:**
```
Found 5 imaging locations (3 framesets, 2 clusters)
```

**Map Display:**
- 3 green markers labeled "[Frame Set]" (clickable)
- 2 green markers labeled "[Unorganized]" (not clickable)
- 4 markers with FOV rectangles (when zoomed in)
- 1 marker without FOV rectangle (IC 1396)

---

## Related Documentation

- [MARKER_DISPLAY_SYSTEM.md](./MARKER_DISPLAY_SYSTEM.md) - Frontend rendering pipeline
- [TROUBLESHOOTING_FRAMESETS.md](./TROUBLESHOOTING_FRAMESETS.md) - Diagnostic queries
- [DATA_MODEL.md](./DATA_MODEL.md) - Data structure reference
- `../db/schema.rs` - Complete database schema

---

## Version History

**v1.0 (2025-11-13):**
- Initial documentation
- Complete query breakdown for both branches
- FOV calculation explanation
- Performance optimization notes
