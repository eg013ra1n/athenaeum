# Coordinate Data Migration Guide

**Date:** 2025-11-13
**Version:** 1.0
**Status:** Reference Only - Not Needed for Fresh Database

---

**IMPORTANT NOTE:** This document is provided for reference only. If you are creating a **new database from scratch** after implementing the parser fixes, you do **NOT** need to perform any data migration. Simply scan your FITS/XISF files with the fixed parser and all coordinates will be correctly extracted and stored.

This guide is only relevant if you have an **existing database** with potentially corrupted coordinate data that you want to preserve and fix.

---

## Executive Summary

This guide provides procedures for detecting and fixing corrupted coordinate data in existing Athenaeum databases. After implementing the parser fixes documented in `FIX_IMPLEMENTATION_PLAN.md`, existing data may still contain:

- RA values in hours stored as degrees (e.g., 12h stored as 12° instead of 180°)
- Missing RA/DEC for frames that have OBJCTRA/OBJCTDEC
- Invalid coordinates outside valid ranges
- Un-normalized coordinates (negative RA, RA >= 360°, etc.)

This guide explains how to:
1. Detect corrupted data
2. Create backups before migration
3. Fix coordinate values
4. Validate the corrections
5. Handle edge cases

---

## Table of Contents

1. [Pre-Migration Checklist](#pre-migration-checklist)
2. [Detection Queries](#detection-queries)
3. [Migration Procedures](#migration-procedures)
4. [Validation Queries](#validation-queries)
5. [Rollback Procedures](#rollback-procedures)
6. [Troubleshooting](#troubleshooting)

---

## Pre-Migration Checklist

### Before You Begin

- [ ] **Backup your database** (CRITICAL - do not skip)
- [ ] **Close Athenaeum application** (prevent concurrent access)
- [ ] **Install sqlite3** command-line tool (for manual queries)
- [ ] **Read this entire guide** before executing any commands
- [ ] **Have the FITS files** available (needed for re-parsing)

### Backup Procedure

```bash
# Find your database location
# macOS: ~/.local/share/com.athenaeum.app/athenaeum.db
# Linux: ~/.local/share/com.athenaeum.app/athenaeum.db
# Windows: %APPDATA%/com.athenaeum.app/athenaeum.db

# Create backup with timestamp
cp ~/.local/share/com.athenaeum.app/athenaeum.db \
   ~/athenaeum_backup_$(date +%Y%m%d_%H%M%S).db

# Verify backup
ls -lh ~/athenaeum_backup_*.db
```

### Verify Database Structure

```bash
# Open database
sqlite3 ~/.local/share/com.athenaeum.app/athenaeum.db

# Check frames table structure
.schema frames

# Should show: ra REAL, dec REAL, objctra TEXT, objctdec TEXT
# If different, consult documentation
```

---

## Detection Queries

These queries identify frames with potentially corrupted coordinate data.

### Query #1: Detect RA in Hours (Most Common Issue)

**Problem:** RA < 24 with valid DEC likely means RA is in hours, not degrees

```sql
-- Find frames with RA in suspected hours format
SELECT
    f.id,
    f.file_id,
    fi.path,
    fi.filename,
    f.ra,
    f.dec,
    f.objctra,
    f.objctdec,
    f.object,
    CASE
        WHEN f.ra >= 0 AND f.ra < 24 AND f.dec >= -90 AND f.dec <= 90 THEN 'LIKELY_HOURS'
        ELSE 'UNKNOWN'
    END as issue_type
FROM frames f
JOIN files fi ON f.file_id = fi.id
WHERE f.ra IS NOT NULL
  AND f.ra >= 0
  AND f.ra < 24
  AND f.dec IS NOT NULL
  AND f.dec >= -90
  AND f.dec <= 90
ORDER BY f.ra;
```

**Interpretation:**
- If you have frames with RA in [0, 24) and valid DEC, they're likely in hours
- Check a few by looking at the `object` field (e.g., M51 should be ~202°, not ~13h)
- Save results: `.output ra_in_hours.txt` then run query

**Example Output:**
```
id|file_id|path|filename|ra|dec|objctra|objctdec|object|issue_type
123|45|/data/m51|M51_300s.fits|13.4|47.2|13:29:52|+47:11:43|M51|LIKELY_HOURS
124|46|/data/m51|M51_300s_002.fits|13.4|47.2|13:29:52|+47:11:43|M51|LIKELY_HOURS
```

**Verification:** M51 is at RA = 13h 29m 52s = 202.47°, but database shows 13.4°

---

### Query #2: Missing RA/DEC with Valid OBJCTRA/OBJCTDEC

**Problem:** Frames have sexagesimal coordinates but not decimal

```sql
-- Find frames with OBJCTRA/OBJCTDEC but missing numeric RA/DEC
SELECT
    f.id,
    f.file_id,
    fi.path,
    fi.filename,
    f.ra,
    f.dec,
    f.objctra,
    f.objctdec,
    f.object
FROM frames f
JOIN files fi ON f.file_id = fi.id
WHERE (f.ra IS NULL OR f.dec IS NULL)
  AND f.objctra IS NOT NULL
  AND f.objctdec IS NOT NULL
ORDER BY f.object;
```

**Impact:** These frames are excluded from spatial queries

---

### Query #3: Invalid Coordinates

**Problem:** Coordinates outside valid ranges

```sql
-- Find frames with invalid coordinates
SELECT
    f.id,
    f.file_id,
    fi.filename,
    f.ra,
    f.dec,
    CASE
        WHEN f.ra < 0 THEN 'RA_NEGATIVE'
        WHEN f.ra >= 360 THEN 'RA_TOO_LARGE'
        WHEN f.dec < -90 THEN 'DEC_TOO_LOW'
        WHEN f.dec > 90 THEN 'DEC_TOO_HIGH'
        ELSE 'UNKNOWN'
    END as issue_type
FROM frames f
JOIN files fi ON f.file_id = fi.id
WHERE (f.ra IS NOT NULL AND (f.ra < 0 OR f.ra >= 360))
   OR (f.dec IS NOT NULL AND (f.dec < -90 OR f.dec > 90))
ORDER BY issue_type;
```

---

### Query #4: Coordinate Inconsistencies

**Problem:** Numeric RA/DEC don't match OBJCTRA/OBJCTDEC

```sql
-- This requires a custom function, can't be done purely in SQL
-- Instead, export data and check with Python/Rust script
-- See "Advanced Detection" section below
```

---

## Migration Procedures

### Option 1: Full Re-Scan (Recommended)

**Best approach:** Re-parse all FITS/XISF files with fixed parser

**Steps:**

1. **Backup database** (already done above)

2. **Note current scan roots:**
   ```sql
   SELECT * FROM scan_roots;
   ```

3. **Delete existing frame data** (keeps files table):
   ```sql
   BEGIN TRANSACTION;

   -- Remove frames from frame sets
   DELETE FROM frames_set_members;

   -- Remove frames
   DELETE FROM frames;

   -- Remove FITS headers
   DELETE FROM fits_header;

   COMMIT;
   ```

4. **Re-scan directories** using Athenaeum with fixed parser:
   - Open Athenaeum
   - Go to File Manager
   - Re-scan your image directories
   - Parser will extract coordinates with correct unit conversion

**Pros:**
- Cleanest solution
- Ensures all metadata is re-extracted with fixes
- No risk of partial corrections

**Cons:**
- Takes time for large collections
- Loses any manual corrections/tags

---

### Option 2: In-Place Correction (For RA in Hours)

**For specific issue:** RA in hours stored as degrees

**Steps:**

1. **Identify affected frames** (using Query #1 above)

2. **Create temporary table for tracking:**
   ```sql
   CREATE TEMP TABLE frames_to_fix AS
   SELECT
       id,
       ra,
       dec,
       ra * 15.0 as corrected_ra  -- Convert hours to degrees
   FROM frames
   WHERE ra IS NOT NULL
     AND ra >= 0
     AND ra < 24
     AND dec IS NOT NULL
     AND dec >= -90
     AND dec <= 90;

   -- Review what will be changed
   SELECT * FROM frames_to_fix LIMIT 10;
   ```

3. **Apply correction:**
   ```sql
   BEGIN TRANSACTION;

   -- Update RA values (hours to degrees)
   UPDATE frames
   SET ra = ra * 15.0
   WHERE id IN (SELECT id FROM frames_to_fix);

   -- Verify count
   SELECT COUNT(*) as 'Frames Corrected' FROM frames_to_fix;

   COMMIT;
   ```

4. **Verify corrections:**
   ```sql
   -- Check a known object (e.g., M51 should be ~202°)
   SELECT id, object, ra, dec, objctra, objctdec
   FROM frames
   WHERE object LIKE '%M51%'
   LIMIT 5;
   ```

**Warning:** This assumes ALL frames with RA < 24 are in hours. If you have legitimate frames near RA=0° to 24° (which should be rare), this will incorrectly multiply them.

---

### Option 3: Convert OBJCTRA/OBJCTDEC to Populate RA/DEC

**For:** Frames with missing numeric coordinates

**This requires a Tauri command or external script since SQLite can't parse sexagesimal strings natively**

#### Create Migration Command

Add to `src-tauri/src/commands.rs`:

```rust
#[tauri::command]
pub async fn migrate_convert_sexagesimal_coordinates(
    state: State<'_, AppState>,
) -> Result<MigrationResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Find frames with OBJCTRA/OBJCTDEC but missing RA/DEC
    let mut stmt = conn.prepare(
        "SELECT id, objctra, objctdec
         FROM frames
         WHERE (ra IS NULL OR dec IS NULL)
           AND objctra IS NOT NULL
           AND objctdec IS NOT NULL"
    ).map_err(|e| e.to_string())?;

    let mut frames_to_convert: Vec<(i64, String, String)> = vec![];
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    }).map_err(|e| e.to_string())?;

    for row in rows {
        frames_to_convert.push(row.map_err(|e| e.to_string())?);
    }

    // Convert each frame
    let mut converted = 0;
    let mut failed = 0;

    for (id, objctra, objctdec) in frames_to_convert {
        match (
            crate::coordinates::parse_ra_sexagesimal(&objctra),
            crate::coordinates::parse_dec_sexagesimal(&objctdec),
        ) {
            (Ok(ra), Ok(dec)) => {
                conn.execute(
                    "UPDATE frames SET ra = ?1, dec = ?2 WHERE id = ?3",
                    rusqlite::params![ra, dec, id],
                ).map_err(|e| e.to_string())?;
                converted += 1;
            },
            _ => {
                eprintln!("Failed to convert frame {}: '{}', '{}'", id, objctra, objctdec);
                failed += 1;
            }
        }
    }

    Ok(MigrationResult {
        converted,
        failed,
        message: format!("Converted {} frames, {} failures", converted, failed),
    })
}

#[derive(serde::Serialize)]
pub struct MigrationResult {
    pub converted: usize,
    pub failed: usize,
    pub message: String,
}
```

**Usage:**
```typescript
// From frontend or test
const result = await invoke('migrate_convert_sexagesimal_coordinates');
console.log(result.message);
```

---

### Option 4: External Python Script

If you prefer not to add Tauri commands, use this Python script:

```python
#!/usr/bin/env python3
"""
Coordinate migration script for Athenaeum database
Converts OBJCTRA/OBJCTDEC to populate missing RA/DEC fields
"""

import sqlite3
import re
import sys
from pathlib import Path

def parse_ra_sexagesimal(ra_str):
    """Convert RA string (HH:MM:SS.ss) to decimal degrees"""
    # Handle formats: "12:30:45.67", "12h30m45.67s"
    ra_str = ra_str.strip().replace('h', ':').replace('m', ':').replace('s', '')
    parts = re.split(r'[:]\s*', ra_str)

    if len(parts) < 2:
        raise ValueError(f"Invalid RA format: {ra_str}")

    h = float(parts[0])
    m = float(parts[1]) if len(parts) > 1 else 0
    s = float(parts[2]) if len(parts) > 2 else 0

    # Convert to degrees
    degrees = (h + m/60.0 + s/3600.0) * 15.0
    return degrees

def parse_dec_sexagesimal(dec_str):
    """Convert DEC string (±DD:MM:SS.s) to decimal degrees"""
    # Handle formats: "+45:30:00", "-12:15:45.3", "+45d30m00s"
    dec_str = dec_str.strip().replace('d', ':').replace('m', ':').replace('s', '')

    sign = -1 if dec_str.startswith('-') else 1
    dec_str = dec_str.lstrip('+-')

    parts = re.split(r'[:]\s*', dec_str)

    if len(parts) < 1:
        raise ValueError(f"Invalid DEC format: {dec_str}")

    d = float(parts[0])
    m = float(parts[1]) if len(parts) > 1 else 0
    s = float(parts[2]) if len(parts) > 2 else 0

    # Convert to degrees
    degrees = sign * (d + m/60.0 + s/3600.0)
    return degrees

def migrate_coordinates(db_path):
    """Migrate coordinates in database"""

    # Backup check
    backup_path = Path(db_path).with_suffix('.db.backup')
    if not backup_path.exists():
        print(f"ERROR: No backup found at {backup_path}")
        print("Create a backup first: cp {db_path} {backup_path}")
        return 1

    conn = sqlite3.connect(db_path)
    cursor = conn.cursor()

    # Find frames to convert
    cursor.execute("""
        SELECT id, objctra, objctdec
        FROM frames
        WHERE (ra IS NULL OR dec IS NULL)
          AND objctra IS NOT NULL
          AND objctdec IS NOT NULL
    """)

    frames = cursor.fetchall()
    print(f"Found {len(frames)} frames to convert")

    converted = 0
    failed = 0

    for frame_id, objctra, objctdec in frames:
        try:
            ra = parse_ra_sexagesimal(objctra)
            dec = parse_dec_sexagesimal(objctdec)

            cursor.execute(
                "UPDATE frames SET ra = ?, dec = ? WHERE id = ?",
                (ra, dec, frame_id)
            )
            converted += 1

            if converted % 100 == 0:
                print(f"Converted {converted}/{len(frames)}...")

        except Exception as e:
            print(f"Failed frame {frame_id}: {objctra}, {objctdec} - {e}")
            failed += 1

    conn.commit()
    conn.close()

    print(f"\nMigration complete:")
    print(f"  Converted: {converted}")
    print(f"  Failed: {failed}")

    return 0

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python migrate_coordinates.py <path_to_athenaeum.db>")
        sys.exit(1)

    db_path = sys.argv[1]
    sys.exit(migrate_coordinates(db_path))
```

**Usage:**
```bash
# Backup first!
cp ~/.local/share/com.athenaeum.app/athenaeum.db \
   ~/.local/share/com.athenaeum.app/athenaeum.db.backup

# Run migration
python3 migrate_coordinates.py ~/.local/share/com.athenaeum.app/athenaeum.db
```

---

## Validation Queries

After migration, run these queries to verify corrections:

### Validation #1: No RA in Hours Range

```sql
-- Should return 0 rows (or very few legitimate frames near RA=0-24°)
SELECT COUNT(*) as suspicious_count
FROM frames
WHERE ra IS NOT NULL
  AND ra >= 0
  AND ra < 24
  AND dec IS NOT NULL
  AND dec >= -90
  AND dec <= 90;
```

**Expected:** 0 or very few rows (only frames legitimately near RA=0-24°, which is uncommon)

---

### Validation #2: All Coordinates in Valid Ranges

```sql
-- Should return 0 rows
SELECT
    id,
    ra,
    dec,
    CASE
        WHEN ra < 0 THEN 'RA_NEGATIVE'
        WHEN ra >= 360 THEN 'RA_TOO_LARGE'
        WHEN dec < -90 THEN 'DEC_TOO_LOW'
        WHEN dec > 90 THEN 'DEC_TOO_HIGH'
    END as issue
FROM frames
WHERE (ra IS NOT NULL AND (ra < 0 OR ra >= 360))
   OR (dec IS NOT NULL AND (dec < -90 OR dec > 90));
```

**Expected:** 0 rows

---

### Validation #3: Populated RA/DEC Where Possible

```sql
-- Count frames with coordinates
SELECT
    COUNT(*) as total_frames,
    COUNT(ra) as frames_with_ra,
    COUNT(dec) as frames_with_dec,
    COUNT(CASE WHEN ra IS NOT NULL AND dec IS NOT NULL THEN 1 END) as frames_with_both,
    ROUND(100.0 * COUNT(ra) / COUNT(*), 2) as pct_with_ra
FROM frames;
```

**Expected:** High percentage of frames with coordinates (depends on your files)

---

### Validation #4: Spot Check Known Objects

```sql
-- Check M51 (should be near RA=202.5°, DEC=47.2°)
SELECT
    id,
    object,
    ROUND(ra, 2) as ra,
    ROUND(dec, 2) as dec,
    objctra,
    objctdec
FROM frames
WHERE object LIKE '%M51%'
LIMIT 5;
```

**Expected:** M51 frames near RA=202.5°, DEC=47.2°

**Other test objects:**
- M31 (Andromeda): RA≈10.7°, DEC≈41.3°
- M42 (Orion): RA≈83.8°, DEC≈-5.4°
- M27 (Dumbbell): RA≈299.9°, DEC≈22.7°

---

### Validation #5: Test Spatial Queries

```sql
-- Query frames near M51 position (RA=202.5°, DEC=47.2°)
-- Should find M51 frames
SELECT
    COUNT(*) as frames_found,
    GROUP_CONCAT(DISTINCT object) as objects
FROM frames
WHERE ra BETWEEN 200 AND 205
  AND dec BETWEEN 45 AND 50;
```

**Expected:** Should find M51 frames if you have them

---

## Rollback Procedures

### If Migration Fails or Produces Wrong Results

1. **Stop immediately** - Don't make more changes

2. **Restore from backup:**
   ```bash
   # Close Athenaeum first!

   # Restore backup (adjust date/time to your backup)
   cp ~/athenaeum_backup_20251113_143000.db \
      ~/.local/share/com.athenaeum.app/athenaeum.db

   # Verify restoration
   sqlite3 ~/.local/share/com.athenaeum.app/athenaeum.db \
      "SELECT COUNT(*) FROM frames;"
   ```

3. **Restart Athenaeum** - Should show data from before migration

4. **Review what went wrong** before retrying

---

## Troubleshooting

### Issue: "No frames found with RA < 24"

**Possible causes:**
- Migration already completed
- Files legitimately don't have this issue
- RA values stored in different format

**Solution:** Check a few frames manually to verify coordinates are correct

---

### Issue: "Failed to parse OBJCTRA/OBJCTDEC"

**Possible causes:**
- Non-standard sexagesimal format
- Corrupted header data
- Empty/NULL strings

**Solution:**
```sql
-- Find problematic formats
SELECT DISTINCT objctra, objctdec
FROM frames
WHERE objctra IS NOT NULL
  AND objctra NOT LIKE '%:%'  -- Not in HH:MM:SS format
LIMIT 20;
```

Handle manually or update parser to support format

---

### Issue: "Conversion multiplied degrees by 15"

**Problem:** Frames with RA already in degrees (20-24° range) were incorrectly multiplied

**Solution:**
1. Restore from backup
2. Use more sophisticated detection:
   ```sql
   -- Only convert if objctra is clearly in HMS format
   WHERE objctra LIKE '%:%'
     AND CAST(SUBSTR(objctra, 1, 2) AS INTEGER) < 24
   ```

---

### Issue: "Spatial queries still missing frames"

**Possible causes:**
- Frames still have NULL RA/DEC
- Query range too narrow
- Frames excluded by other filters (imagetyp, etc.)

**Debug:**
```sql
-- Check why frame not in results
SELECT
    id,
    object,
    ra,
    dec,
    imagetyp,
    CASE
        WHEN ra IS NULL THEN 'RA_IS_NULL'
        WHEN dec IS NULL THEN 'DEC_IS_NULL'
        WHEN ra < 200 OR ra > 205 THEN 'RA_OUT_OF_RANGE'
        WHEN dec < 45 OR dec > 50 THEN 'DEC_OUT_OF_RANGE'
        ELSE 'SHOULD_MATCH'
    END as reason
FROM frames
WHERE object LIKE '%M51%';
```

---

## Advanced Detection

### Python Script to Verify Coordinate Consistency

```python
#!/usr/bin/env python3
"""
Verify that numeric RA/DEC match sexagesimal OBJCTRA/OBJCTDEC
"""

import sqlite3
import sys
from migrate_coordinates import parse_ra_sexagesimal, parse_dec_sexagesimal

def verify_coordinates(db_path):
    conn = sqlite3.connect(db_path)
    cursor = conn.cursor()

    cursor.execute("""
        SELECT id, ra, dec, objctra, objctdec, object
        FROM frames
        WHERE ra IS NOT NULL
          AND dec IS NOT NULL
          AND objctra IS NOT NULL
          AND objctdec IS NOT NULL
        LIMIT 1000
    """)

    mismatches = 0

    for frame_id, ra, dec, objctra, objctdec, obj in cursor.fetchall():
        try:
            ra_from_sex = parse_ra_sexagesimal(objctra)
            dec_from_sex = parse_dec_sexagesimal(objctdec)

            # Allow small differences (0.01° = 36 arcsec, reasonable for rounding)
            ra_diff = abs(ra - ra_from_sex)
            dec_diff = abs(dec - dec_from_sex)

            if ra_diff > 0.01 or dec_diff > 0.01:
                print(f"Mismatch frame {frame_id} ({obj}):")
                print(f"  Numeric:    RA={ra:8.4f}° DEC={dec:8.4f}°")
                print(f"  Sexagesimal: RA={ra_from_sex:8.4f}° DEC={dec_from_sex:8.4f}°")
                print(f"  Difference:  ΔRA={ra_diff:8.4f}° ΔDEC={dec_diff:8.4f}°")
                print()
                mismatches += 1

        except Exception as e:
            print(f"Error parsing frame {frame_id}: {e}")

    conn.close()
    print(f"Found {mismatches} mismatches")
    return 0 if mismatches == 0 else 1

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python verify_coordinates.py <path_to_athenaeum.db>")
        sys.exit(1)

    db_path = sys.argv[1]
    sys.exit(verify_coordinates(db_path))
```

---

## Summary Checklist

### Before Migration
- [ ] Close Athenaeum application
- [ ] Create database backup with timestamp
- [ ] Run detection queries to identify issues
- [ ] Choose migration strategy (re-scan vs in-place)
- [ ] Read entire guide

### During Migration
- [ ] Use transactions for SQL updates
- [ ] Monitor progress and errors
- [ ] Keep logs of changes made

### After Migration
- [ ] Run all validation queries
- [ ] Spot-check known objects
- [ ] Test spatial queries in application
- [ ] Verify frame set clustering positions
- [ ] Keep backup for at least 1 week

### If Problems
- [ ] Stop immediately
- [ ] Restore from backup
- [ ] Document what went wrong
- [ ] Seek help before retrying

---

## Related Documents

- `COORDINATE_ISSUES.md` - Analysis of coordinate problems
- `DATABASE_CONSISTENCY_ISSUES.md` - Other database issues
- `FIX_IMPLEMENTATION_PLAN.md` - Parser fixes to implement first

---

## Conclusion

Data migration for coordinate fixes ranges from simple (re-scan with fixed parser) to complex (in-place SQL updates). The safest approach is **full re-scan** if you have time and the original FITS files.

For large databases where re-scanning is impractical, the in-place corrections can be effective, but require careful validation to ensure coordinates are truly in hours and not legitimate low-RA positions.

**Always backup before migration** - database corruption can result in permanent data loss.

After migration, spatial queries should work correctly and frame set clustering should group frames at accurate sky positions.
