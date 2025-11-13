# Coordinate Handling Issues in Athenaeum

**Date:** 2025-11-13
**Status:** Critical Issues FIXED (as of 2025-11-13)
**Priority:** RESOLVED - Parser fixes implemented

## Executive Summary

**RESOLUTION STATUS (2025-11-13):** The critical coordinate handling issues identified in this document have been **FIXED**. The parser now correctly detects RA units using OBJCTRA verification and validates DEC ranges. See `IMPLEMENTATION_STATUS.md` for details.

This document details critical issues that were found in how astronomical coordinates (RA/DEC) are read, converted, validated, and stored in the Athenaeum database. The analysis revealed that **coordinates may be stored in incorrect units**, leading to:

- Wrong spatial query results
- Incorrect frame set clustering
- Missing frames in searches
- Data corruption for existing files

**Key Finding (FIXED):** FITS files can contain RA in either **hours (0-24)** or **degrees (0-360)**, but the original parser didn't distinguish between them, potentially storing hours as if they were degrees (e.g., 12h stored as 12° instead of 180°). This has been resolved by implementing `normalize_ra_from_fits()` with OBJCTRA verification.

---

## Table of Contents

1. [Coordinate Data Flow](#coordinate-data-flow)
2. [Critical Issues](#critical-issues)
3. [Issue Details with Code Analysis](#issue-details-with-code-analysis)
4. [Impact Assessment](#impact-assessment)
5. [Example Scenarios](#example-scenarios)
6. [Recommendations](#recommendations)

---

## Coordinate Data Flow

### Overview

```
FITS/XISF File → Parser → Frame Model → Database → Queries/Clustering
```

### Detailed Flow

#### Step 1: File Reading

**FITS Files** (`src-tauri/src/fits_parser/mod.rs:181-184`):
```rust
let ra = read_keyword_f64(&mut fitsfile, &hdu, "RA").ok();
let dec = read_keyword_f64(&mut fitsfile, &hdu, "DEC").ok();
let objctra = read_keyword_string(&mut fitsfile, &hdu, "OBJCTRA").ok();
let objctdec = read_keyword_string(&mut fitsfile, &hdu, "OBJCTDEC").ok();
```

**XISF Files** (`src-tauri/src/fits_parser/mod.rs:395-400`):
```rust
let ra = fits_keywords.get("RA")
    .and_then(|s| s.parse::<f64>().ok());
let dec = fits_keywords.get("DEC")
    .and_then(|s| s.parse::<f64>().ok());
let objctra = fits_keywords.get("OBJCTRA").cloned();
let objctdec = fits_keywords.get("OBJCTDEC").cloned();
```

**Issues at this stage:**
- ✅ Reads RA/DEC as f64
- ✅ Reads OBJCTRA/OBJCTDEC as strings
- ❌ **No unit detection** (hours vs degrees)
- ❌ **No validation** of value ranges
- ❌ **No conversion** from sexagesimal to decimal

#### Step 2: Database Storage

**Schema** (`src-tauri/src/db/schema.rs:44-51`):
```sql
ra REAL,              -- Assumed to be decimal degrees [0, 360)
dec REAL,             -- Assumed to be decimal degrees [-90, 90]
objctra TEXT,         -- Sexagesimal string (e.g., "12:30:00")
objctdec TEXT,        -- Sexagesimal string (e.g., "+45:00:00")
```

**Insertion** (`src-tauri/src/db/operations.rs:74-82`):
```rust
frame.ra,             // Inserted as-is, NO normalization
frame.dec,            // Inserted as-is, NO clamping
frame.objctra,        // String stored directly
frame.objctdec,       // String stored directly
```

**Issues at this stage:**
- ❌ **No constraints** on valid ranges
- ❌ **No documentation** of expected units
- ❌ **No normalization** to [0, 360) for RA
- ❌ **No conversion** from OBJCTRA/OBJCTDEC to populate RA/DEC

#### Step 3: Data Usage

**Clustering** (`src-tauri/src/clustering/mod.rs:159-177`):
```rust
pub fn normalize_frame_coordinates(frame: &Frame) -> Result<ClusterableFrame> {
    let (ra_deg, dec_deg) = if let (Some(ra), Some(dec)) = (frame.ra, frame.dec) {
        (ra, dec)  // ⚠️ ASSUMES already in degrees!
    } else if let (Some(objctra), Some(objctdec)) = (&frame.objctra, &frame.objctdec) {
        let ra = parse_ra_sexagesimal(objctra)?;  // ✅ Converts HMS to degrees
        let dec = parse_dec_sexagesimal(objctdec)?;  // ✅ Converts DMS to degrees
        (ra, dec)
    } else {
        return Err(anyhow!("Frame has no valid coordinates"));
    };
    // ...
}
```

**Spatial Queries** (`src-tauri/src/commands.rs:1628-1693`):
```rust
pub async fn query_frames_in_circle(
    state: State<'_, AppState>,
    ra: f64,      // Assumes degrees
    dec: f64,     // Assumes degrees
    radius_degrees: f64,
) -> Result<SelectionResult, String> {
    // Query: SELECT id, ra, dec FROM frames WHERE ra IS NOT NULL
    // ⚠️ Excludes frames with only OBJCTRA/OBJCTDEC
}
```

**Issues at this stage:**
- ❌ **Wrong assumption** that RA/DEC are in degrees
- ❌ **No validation** of coordinate units
- ❌ **Spatial queries miss frames** with only sexagesimal coordinates
- ✅ Clustering handles conversion (but inconsistent with queries)

---

## Critical Issues

### Issue #1: No Unit Detection During FITS Parsing

**Severity:** CRITICAL
**File:** `src-tauri/src/fits_parser/mod.rs:181-184` (FITS), `mod.rs:395-400` (XISF)
**Status:** Data Corruption Risk

#### Problem

The FITS standard allows RA to be expressed in either:
- **Hours**: Range [0, 24), often written as HH:MM:SS
- **Degrees**: Range [0, 360), often written as decimal degrees

When stored as a numeric value (not sexagesimal string), both are valid. The parser reads the numeric value without checking which unit it represents.

#### Current Behavior

```rust
// Both formats read identically
let ra = read_keyword_f64(&mut fitsfile, &hdu, "RA").ok();
```

If FITS header contains:
```
RA = 12.5
```

Is this 12.5 hours (should be 187.5°) or 12.5 degrees?

**The parser assumes degrees**, but it could be hours!

#### Impact

- **Data corruption**: RA values stored in wrong units
- **Wrong clustering**: Frame sets grouped at incorrect sky positions
- **Wrong queries**: Spatial searches return incorrect results
- **Silent failure**: No error or warning generated

#### Example

```
FITS Header:
  RA = 12.5 (actually hours, valid FITS format)
  DEC = 45.0 (degrees)

Current Behavior:
  Database: RA=12.5, DEC=45.0
  Clustering: Groups at 12.5°, 45° ❌ (should be 187.5°, 45°)

Correct Behavior:
  Detect RA < 24 with DEC in valid range → Convert to degrees
  Database: RA=187.5, DEC=45.0
  Clustering: Groups at 187.5°, 45° ✅
```

---

### Issue #2: No Validation of Coordinate Ranges

**Severity:** CRITICAL
**File:** `src-tauri/src/fits_parser/mod.rs:181-184`
**Status:** Data Corruption Risk

#### Problem

No validation that coordinates are in physically valid ranges:
- RA should be in [0, 360) degrees
- DEC should be in [-90, 90] degrees

Invalid coordinates are accepted and stored in the database.

#### Examples of Invalid Data Accepted

```rust
// These would all be accepted without error:
RA = -45.0        // Invalid (negative)
RA = 400.0        // Invalid (> 360)
DEC = 120.0       // Invalid (> 90)
DEC = -95.0       // Invalid (< -90)
```

#### Impact

- **Data corruption**: Invalid coordinates stored
- **Query failures**: May cause unexpected behavior in spatial queries
- **Calculation errors**: Angular distance calculations may give wrong results
- **No detection**: Invalid data can't be identified later

---

### Issue #3: Wrong Coordinate Preference in Clustering

**Severity:** HIGH
**File:** `src-tauri/src/clustering/mod.rs:162-170`
**Status:** Logic Error

#### Problem

The clustering algorithm prefers numeric RA/DEC over OBJCTRA/OBJCTDEC without validating that RA/DEC are in correct units:

```rust
let (ra_deg, dec_deg) = if let (Some(ra), Some(dec)) = (frame.ra, frame.dec) {
    (ra, dec)  // ⚠️ Uses this FIRST, assumes correct
} else if let (Some(objctra), Some(objctdec)) = (&frame.objctra, &frame.objctdec) {
    let ra = parse_ra_sexagesimal(objctra)?;  // ✅ Only used as fallback
    let dec = parse_dec_sexagesimal(objctdec)?;
    (ra, dec)
} else {
    return Err(anyhow!("Frame has no valid coordinates"));
};
```

#### Why This Is Wrong

If `frame.ra` contains hours (not degrees) due to Issue #1, the clustering will use the wrong value. OBJCTRA/OBJCTDEC, if present, would likely be more reliable since they explicitly encode hours in the string format (HH:MM:SS).

#### Correct Logic

Should either:
1. **Validate RA/DEC** before using them, or
2. **Prefer OBJCTRA/OBJCTDEC** and convert to populate RA/DEC

---

### Issue #4: No Coordinate Normalization

**Severity:** HIGH
**File:** `src-tauri/src/db/operations.rs:74-82`
**Status:** Data Inconsistency

#### Problem

Coordinates are stored without normalization:
- RA is not normalized to [0, 360)
- DEC is not clamped to [-90, 90]

This means mathematically equivalent coordinates might be stored differently:
- RA = 370° and RA = 10° represent the same position
- RA = -10° and RA = 350° represent the same position

#### Impact

- **Query failures**: Wrap-around queries near 0°/360° may miss frames
- **Clustering errors**: Same position might be treated as different
- **Data inconsistency**: Multiple representations of same coordinate

#### Example

```
Frame 1: RA = 359.5°
Frame 2: RA = -0.5°  (stored as-is, not normalized to 359.5°)

Spatial query: RA ∈ [358°, 2°]
Result: May miss Frame 2 depending on query logic
```

---

### Issue #5: Duplicate Angular Distance Implementations

**Severity:** MEDIUM
**Files:**
- `src-tauri/src/coordinates/mod.rs:131-146`
- `src-tauri/src/selection/algorithms.rs:13-31`
**Status:** Inconsistency

#### Problem

Two different algorithms for calculating angular distance between coordinates:

**Implementation 1: Spherical Law of Cosines** (`coordinates/mod.rs`):
```rust
pub fn angular_distance(ra1_deg: f64, dec1_deg: f64, ra2_deg: f64, dec2_deg: f64) -> f64 {
    let cos_angle = dec1.sin() * dec2.sin()
                    + dec1.cos() * dec2.cos() * (ra2 - ra1).cos();
    let cos_angle = cos_angle.clamp(-1.0, 1.0);
    cos_angle.acos().to_degrees()
}
```

**Implementation 2: Haversine Formula** (`selection/algorithms.rs`):
```rust
pub fn angular_distance(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    let a = (ddec / 2.0).sin().powi(2)
        + dec1_rad.cos() * dec2_rad.cos() * (dra / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    c.to_degrees()
}
```

#### Which Is Used Where?

- **Clustering** (`clustering/mod.rs:53`): Uses `coordinates::angular_distance` (Spherical Law)
- **Spatial Queries** (`commands.rs:1661`): Uses `selection::angular_distance` (Haversine)

#### Impact

- **Inconsistent results**: Same coordinate pair may have different distances
- **Maintenance burden**: Two implementations to maintain
- **Confusion**: Which one is correct?

**Note:** For most astronomical purposes, both give similar results, but haversine is more numerically stable for small angles.

---

### Issue #6: Frames with Only OBJCTRA/OBJCTDEC Not Queryable

**Severity:** HIGH
**File:** `src-tauri/src/commands.rs:1634-1639`
**Status:** Missing Data in Queries

#### Problem

Spatial query commands filter with `WHERE ra IS NOT NULL`:

```rust
let mut stmt = conn.prepare(
    "SELECT id, ra, dec, exptime FROM frames
     WHERE ra IS NOT NULL AND dec IS NOT NULL"
)?;
```

This excludes frames that have only OBJCTRA/OBJCTDEC but not numeric RA/DEC.

#### Impact

- **Missing data**: Frames with valid coordinates are excluded from searches
- **Inconsistency**: Clustering can use these frames (converts OBJCTRA/OBJCTDEC), but queries can't
- **User confusion**: Frames visible in frame sets but not in spatial selections

#### Example

```
Frame with:
  RA = NULL
  DEC = NULL
  OBJCTRA = "12:30:00"  (valid, = 187.5°)
  OBJCTDEC = "+45:00:00" (valid)

Clustering: ✅ Converts OBJCTRA/OBJCTDEC, includes frame in sets
Spatial Query: ❌ Excluded because RA IS NULL
```

---

### Issue #7: No HMS/DMS Format Detection for Numeric RA/DEC

**Severity:** MEDIUM
**File:** `src-tauri/src/fits_parser/mod.rs:181-184`
**Status:** Limited Format Support

#### Problem

If RA/DEC are stored as numeric values but actually represent hours (for RA), there's no heuristic to detect this.

The only reliable way to know is:
1. Check if value is in [0, 24) → likely hours
2. Check FITS header comments (not currently parsed)
3. Check if OBJCTRA/OBJCTDEC are present and consistent

#### Current State

- ✅ Parses OBJCTRA/OBJCTDEC strings in HMS/DMS format
- ❌ Doesn't use these to validate numeric RA/DEC
- ❌ Doesn't detect if numeric RA is in hours

---

### Issue #8: No Coordinate Precision Tracking

**Severity:** LOW
**File:** All coordinate handling
**Status:** Feature Missing

#### Problem

Original coordinate precision is not tracked:
- OBJCTRA = "12:30:00" has precision to 15 arc-seconds
- OBJCTRA = "12:30:00.123" has precision to 0.015 arc-seconds

After conversion to decimal degrees, this precision information is lost.

#### Impact

- **Precision loss**: Can't determine original measurement precision
- **Quality tracking**: Can't assess coordinate quality
- **Rounding issues**: May round differently than original

**Note:** This is LOW priority as it doesn't affect correctness, just metadata quality.

---

### Issue #9: No Coordinate System Metadata

**Severity:** LOW
**File:** `src-tauri/src/db/schema.rs`, `src-tauri/src/models.rs`
**Status:** Missing Metadata

#### Problem

No tracking of coordinate system metadata:
- **EQUINOX** (B1950, J2000, etc.)
- **RADESYS** (FK4, FK5, ICRS, etc.)
- **CTYPE1/CTYPE2** (Projection type)

Currently assumes all coordinates are in the same system (likely J2000/ICRS).

#### Impact

- **Epoch differences**: Coordinates from different epochs can't be distinguished
- **Conversion needed**: Can't convert between systems without metadata
- **Precision loss**: Different epochs have ~1 arcmin differences

**Note:** For most amateur astrophotography, this is acceptable as J2000 is standard. But for scientific applications or archival data, this could be problematic.

---

## Impact Assessment

### High Impact (Affects Data Integrity)

| Issue | Severity | Affected Component | Impact |
|-------|----------|-------------------|--------|
| #1: No unit detection | CRITICAL | Parser, Database | **RA in hours stored as degrees** → Wrong positions |
| #2: No validation | CRITICAL | Parser, Database | **Invalid coordinates accepted** → Corrupted data |
| #3: Wrong coordinate preference | HIGH | Clustering | **Wrong coordinates used** → Incorrect frame sets |
| #6: OBJCTRA/OBJCTDEC not queryable | HIGH | Spatial Queries | **Missing frames in results** → Incomplete searches |

### Medium Impact (Affects Functionality)

| Issue | Severity | Affected Component | Impact |
|-------|----------|-------------------|--------|
| #4: No normalization | HIGH | Database, Queries | **Boundary queries may fail** → Edge case errors |
| #5: Duplicate implementations | MEDIUM | Clustering, Queries | **Inconsistent distances** → Maintenance burden |

### Low Impact (Missing Features)

| Issue | Severity | Affected Component | Impact |
|-------|----------|-------------------|--------|
| #7: No format detection | MEDIUM | Parser | **Limited format support** → Some files may fail |
| #8: No precision tracking | LOW | Metadata | **Lost precision info** → Quality assessment difficult |
| #9: No coordinate system | LOW | Metadata | **Epoch differences** → Scientific use limited |

---

## Example Scenarios

### Scenario 1: RA in Hours (Most Critical)

**FITS File:**
```
SIMPLE  =                    T
NAXIS   =                    2
NAXIS1  =                 4656
NAXIS2  =                 3520
RA      =              12.5000  / Right Ascension in hours
DEC     =              45.0000  / Declination in degrees
OBJECT  = 'M51     '
```

**Current Behavior:**

1. Parser reads: `RA=12.5`, `DEC=45.0`
2. Database stores: `RA=12.5`, `DEC=45.0`
3. Clustering calculates: "M51 is at 12.5°, 45°" ❌
4. Spatial query for M51 at (187.5°, 45°): **Frame NOT found** ❌

**Actual M51 position:** RA = 13h 29m 52s = 202.5°, DEC = 47° 11'

**Problem:** The frame would be clustered at (12.5°, 45°) instead of (187.5°, 45°), and searches for M51 would miss it.

**Correct Behavior:**

1. Parser detects: RA < 24, DEC valid → RA is in hours
2. Parser converts: RA = 12.5 * 15 = 187.5°
3. Database stores: `RA=187.5`, `DEC=45.0`
4. Clustering: Frame correctly positioned at (187.5°, 45°) ✅
5. Spatial queries work correctly ✅

---

### Scenario 2: Only Sexagesimal Coordinates

**FITS File:**
```
SIMPLE  =                    T
NAXIS   =                    2
OBJCTRA = '12:30:00.00'      / Object Right Ascension
OBJCTDEC= '+45:00:00.0'      / Object Declination
OBJECT  = 'NGC4567'
# No numeric RA/DEC keywords
```

**Current Behavior:**

1. Parser reads: `RA=NULL`, `DEC=NULL`, `OBJCTRA='12:30:00.00'`, `OBJCTDEC='+45:00:00.0'`
2. Database stores: `RA=NULL`, `DEC=NULL`, `OBJCTRA='12:30:00.00'`, `OBJCTDEC='+45:00:00.0'`
3. Clustering: ✅ Converts strings, includes frame (187.5°, 45°)
4. Spatial query: ❌ Excluded because `WHERE ra IS NOT NULL` fails

**Result:** Frame appears in frame sets but not in spatial selections!

**Correct Behavior:**

1. Parser converts: `OBJCTRA='12:30:00.00'` → `RA=187.5°`
2. Parser converts: `OBJCTDEC='+45:00:00.0'` → `DEC=45.0°`
3. Database stores: `RA=187.5`, `DEC=45.0`, `OBJCTRA='12:30:00.00'`, `OBJCTDEC='+45:00:00.0'`
4. Both clustering and queries work ✅

---

### Scenario 3: RA Wrap-Around

**Frames:**
```
Frame 1: RA = 359.5°, DEC = 0°
Frame 2: RA = 0.5°, DEC = 0°
```

**Spatial Query:** Select frames in rectangle RA ∈ [358°, 2°], DEC ∈ [-1°, 1°]

**Current Behavior:**

- Rectangle query has wrap-around logic ✅
- **BUT** if Frame 2 was stored as `RA = -0.5°` (not normalized), query logic breaks ❌

**With Normalization:**

- Frame 2: Normalized to `RA = 359.5°` during storage
- Rectangle query works correctly ✅

---

### Scenario 4: Invalid Coordinates Accepted

**FITS File (corrupted or unusual telescope):**
```
RA      =              -45.0  / Invalid negative RA
DEC     =              120.0  / Invalid DEC > 90
```

**Current Behavior:**

1. Parser accepts: `RA=-45.0`, `DEC=120.0` (no validation)
2. Database stores: `RA=-45.0`, `DEC=120.0`
3. Clustering: May fail or produce nonsense results
4. Queries: May return frame for searches it shouldn't match

**With Validation:**

1. Parser detects: RA < 0 → normalize to `RA = 315.0°`
2. Parser detects: DEC > 90 → **reject or warn**
3. Database stores valid coordinates only

---

## Recommendations

### Phase 1: Critical Fixes (Prevent Further Data Corruption)

#### Fix #1: Add Unit Detection and Conversion

**File:** `src-tauri/src/fits_parser/mod.rs`

Add helper function:
```rust
/// Detects if RA is in hours (0-24) or degrees (0-360) and converts to degrees
fn normalize_ra_from_fits(ra: f64, dec: Option<f64>) -> f64 {
    // If RA is clearly in [0, 24) range and we have a valid DEC, assume hours
    if ra >= 0.0 && ra < 24.0 {
        // Check if DEC is in valid range to confirm this is coordinate data
        if let Some(d) = dec {
            if d >= -90.0 && d <= 90.0 {
                // High confidence RA is in hours, convert to degrees
                return ra * 15.0;
            }
        }
        // Ambiguous: could be hours or degrees in [0, 24) range
        // Log warning and assume hours (safer for most FITS files)
        eprintln!("WARNING: RA={} is ambiguous (hours or degrees?). Assuming hours, converting to degrees.", ra);
        return ra * 15.0;
    }

    // RA >= 24, must be degrees
    crate::coordinates::normalize_ra(ra)
}
```

**Update parser (line 181):**
```rust
let ra_raw = read_keyword_f64(&mut fitsfile, &hdu, "RA").ok();
let dec_raw = read_keyword_f64(&mut fitsfile, &hdu, "DEC").ok();

// Apply unit detection and normalization
let ra = ra_raw.map(|r| normalize_ra_from_fits(r, dec_raw));
let dec = dec_raw.map(|d| crate::coordinates::normalize_dec(d));
```

#### Fix #2: Add Coordinate Validation

**File:** `src-tauri/src/coordinates/mod.rs` (functions already exist, just use them!)

```rust
pub fn normalize_ra(ra: f64) -> f64 {
    let mut result = ra % 360.0;
    if result < 0.0 {
        result += 360.0;
    }
    result
}

pub fn normalize_dec(dec: f64) -> f64 {
    dec.clamp(-90.0, 90.0)
}
```

**These functions exist but aren't used during parsing!**

#### Fix #3: Populate RA/DEC from OBJCTRA/OBJCTDEC

**File:** `src-tauri/src/fits_parser/mod.rs`

Add after reading all keywords:
```rust
// If numeric RA/DEC are missing but we have sexagesimal strings, convert them
let (ra, dec) = match (ra, dec, &objctra, &objctdec) {
    // Have numeric coordinates, use them (already normalized above)
    (Some(r), Some(d), _, _) => (Some(r), Some(d)),

    // Missing numeric but have sexagesimal, convert
    (None, None, Some(ra_str), Some(dec_str)) => {
        match (parse_ra_sexagesimal(ra_str), parse_dec_sexagesimal(dec_str)) {
            (Ok(r), Ok(d)) => {
                println!("  Converted OBJCTRA/OBJCTDEC to numeric: RA={:.4}°, DEC={:.4}°", r, d);
                (Some(r), Some(d))
            },
            _ => {
                println!("  Failed to parse OBJCTRA/OBJCTDEC");
                (None, None)
            }
        }
    },

    // Mixed or neither, keep what we have
    _ => (ra, dec),
};
```

### Phase 2: Consistency Fixes

#### Fix #4: Remove Duplicate angular_distance

Keep haversine version in `selection/algorithms.rs`, update clustering to use it:

**File:** `src-tauri/src/clustering/mod.rs:53`

```rust
// Change from:
let distance = crate::coordinates::angular_distance(cf1.ra_deg, cf1.dec_deg, cf2.ra_deg, cf2.dec_deg);

// To:
let distance = crate::selection::algorithms::angular_distance(cf1.ra_deg, cf1.dec_deg, cf2.ra_deg, cf2.dec_deg);
```

#### Fix #5: Add Database Constraints

**File:** `src-tauri/src/db/schema.rs`

Add validation (note: SQLite CHECK constraints are limited, may need application-level checks):

```sql
-- Add after table creation
CREATE INDEX idx_frames_coordinates ON frames(ra, dec) WHERE ra IS NOT NULL AND dec IS NOT NULL;
```

Application-level validation in `db/operations.rs`:
```rust
pub fn insert_frame(conn: &Connection, frame: &Frame) -> Result<i64> {
    // Validate coordinates before insertion
    if let Some(ra) = frame.ra {
        if ra < 0.0 || ra >= 360.0 {
            return Err(anyhow!("Invalid RA: {} (must be in [0, 360))", ra));
        }
    }
    if let Some(dec) = frame.dec {
        if dec < -90.0 || dec > 90.0 {
            return Err(anyhow!("Invalid DEC: {} (must be in [-90, 90])", dec));
        }
    }

    // ... existing insertion code ...
}
```

### Phase 3: Data Migration

See `COORDINATE_MIGRATION_GUIDE.md` for detailed procedures to fix existing database.

---

## Testing Strategy

### Unit Tests

Create `src-tauri/src/fits_parser/tests.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ra_in_hours_conversion() {
        let ra_hours = 12.5;
        let dec = 45.0;
        let ra_degrees = normalize_ra_from_fits(ra_hours, Some(dec));
        assert_eq!(ra_degrees, 187.5);
    }

    #[test]
    fn test_ra_in_degrees_passthrough() {
        let ra_degrees = 187.5;
        let dec = 45.0;
        let result = normalize_ra_from_fits(ra_degrees, Some(dec));
        assert_eq!(result, 187.5);
    }

    #[test]
    fn test_ra_normalization() {
        assert_eq!(normalize_ra(-10.0), 350.0);
        assert_eq!(normalize_ra(370.0), 10.0);
        assert_eq!(normalize_ra(180.0), 180.0);
    }

    #[test]
    fn test_dec_clamping() {
        assert_eq!(normalize_dec(100.0), 90.0);
        assert_eq!(normalize_dec(-100.0), -90.0);
        assert_eq!(normalize_dec(45.0), 45.0);
    }
}
```

### Integration Tests

Test with real FITS files covering:
1. RA in hours (< 24)
2. RA in degrees (>= 24)
3. Only OBJCTRA/OBJCTDEC present
4. Both numeric and sexagesimal present
5. Invalid/missing coordinates

---

## Related Documents

- `DATABASE_CONSISTENCY_ISSUES.md` - Other database and parser issues
- `FIX_IMPLEMENTATION_PLAN.md` - Detailed implementation steps for all fixes
- `COORDINATE_MIGRATION_GUIDE.md` - How to fix existing corrupted data in database

---

## Conclusion

The coordinate handling issues represent **critical data integrity problems** that can lead to:
- Incorrectly positioned frames in frame sets
- Missing frames in spatial searches
- Corrupted coordinate data in the database

The root cause is **lack of unit detection and validation** during FITS/XISF parsing. The fix requires:
1. Detecting if RA is in hours or degrees
2. Validating coordinate ranges
3. Converting sexagesimal coordinates to populate numeric fields
4. Normalizing all coordinates to standard ranges

All necessary conversion functions already exist in `coordinates/mod.rs` but are **not used during parsing**. The implementation is straightforward but requires careful testing with various FITS file formats.
