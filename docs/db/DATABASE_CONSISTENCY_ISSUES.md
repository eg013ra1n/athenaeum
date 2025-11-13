# Database Consistency Issues in Athenaeum

**Date:** 2025-11-13
**Status:** Critical Bugs Identified
**Priority:** CRITICAL - Immediate Fix Required

## Executive Summary

This document details database consistency issues, query inconsistencies, and parser differences between FITS and XISF file handling in Athenaeum. The analysis identified:

- **1 CRITICAL bug** causing data corruption (duplicate frame construction with wrong indexes)
- **1 HIGH priority issue** causing missing data (naxis1/naxis2 fields missing in queries)
- **2 MEDIUM priority inconsistencies** between FITS and XISF parsers
- **1 LOW priority issue** with logging verbosity

**URGENT:** The duplicate frame construction bug in `get_frames_with_files_for_set()` will cause crashes or data corruption when loading frame set details.

---

## Table of Contents

1. [Critical Issues](#critical-issues)
2. [High Priority Issues](#high-priority-issues)
3. [Medium Priority Issues](#medium-priority-issues)
4. [Low Priority Issues](#low-priority-issues)
5. [Database Schema Analysis](#database-schema-analysis)
6. [Model Consistency Verification](#model-consistency-verification)
7. [Recommendations](#recommendations)

---

## Critical Issues

### Issue #1: Duplicate Frame Construction with Wrong Row Indexes

**Severity:** CRITICAL - Data Corruption / Crash Risk
**File:** `src-tauri/src/db/operations.rs`
**Lines:** 817-862
**Status:** MUST FIX IMMEDIATELY

#### Problem

The function `get_frames_with_files_for_set()` contains **duplicate frame construction code** with the second block using **incorrect row indexes**:

**First construction (lines 782-816) - CORRECT:**
```rust
let frame = crate::models::Frame {
    id: row.get(9)?,      // ✅ Correct index for frame.id
    file_id: row.get(10)?,  // ✅ Correct index for frame.file_id
    object: row.get(11)?,
    date_obs: date_obs_opt,
    telescop: row.get(13)?,
    instrume: row.get(14)?,
    exptime: row.get(15)?,
    filter: row.get(16)?,
    imagetyp: imagetyp_opt,
    is_master: row.get::<_, i64>(18)? != 0,
    gain: row.get(19)?,
    offset: row.get(20)?,
    binning: row.get(21)?,
    xbinning: row.get(22)?,
    ybinning: row.get(23)?,
    ccd_temp: row.get(24)?,
    set_temp: row.get(25)?,
    focallen: row.get(26)?,
    xpixsz: row.get(27)?,
    pixsz: row.get(28)?,
    naxis1: row.get(29)?,
    naxis2: row.get(30)?,
    ra: row.get(31)?,
    dec: row.get(32)?,
    sitelat: row.get(33)?,
    lat_obs: row.get(34)?,
    sitelong: row.get(35)?,
    long_obs: row.get(36)?,
    objctra: row.get(37)?,
    objctdec: row.get(38)?,
    override_: row.get::<_, i64>(39)? != 0,
};
```

**Second construction (lines 828-862) - WRONG INDEXES:**
```rust
let frame = crate::models::Frame {
    id: row.get(7)?,      // ❌ WRONG! Should be 9
    file_id: row.get(8)?,   // ❌ WRONG! Should be 10
    object: row.get(9)?,    // ❌ All subsequent indexes off by 2
    // ... all fields shifted by 2 positions ...
};
```

**The function returns the SECOND (buggy) construction!**

#### Impact

**When loading frame set details, this will cause:**

1. **Data corruption**: Field values read from wrong columns
   - `id` reads from column 7 (might be `file.path` or `file.filename`)
   - `file_id` reads from column 8 (might be `file.size`)
   - All frame metadata completely wrong

2. **Type mismatch crashes**:
   - Trying to parse TEXT as INTEGER will panic
   - Trying to parse INTEGER as f64 will panic
   - Application crashes when viewing frame set details

3. **Silent failures**:
   - Some type conversions might succeed but with wrong values
   - Frame data displayed in UI will be completely incorrect

#### Example Failure Scenario

```rust
// SQL returns row with columns:
// 0: file.id
// 1: file.path
// 2: file.filename
// 3: file.size
// 4: file.modified_at
// 5: file.format
// 6: file.created_at
// 7: file.metadata_hash
// 8: file.content_hash
// 9: frame.id
// 10: frame.file_id
// ...

// Code tries to do:
let id = row.get::<_, i64>(7)?;  // Gets TEXT (metadata_hash) as i64 -> CRASH!
```

#### Fix

**Delete lines 828-862** (the duplicate construction with wrong indexes).

The correct frame construction (lines 782-816) should be the only one used.

---

## High Priority Issues

### Issue #2: Missing naxis1/naxis2 in Directory Browse Queries

**Severity:** HIGH - Missing Data
**File:** `src-tauri/src/db/operations.rs`
**Lines:** 289-385 (`get_files_by_directory` function)
**Status:** Should Fix Soon

#### Problem

The `get_files_by_directory()` function's SELECT statement is **missing `naxis1` and `naxis2` fields**:

**Affected SQL (lines 306-307):**
```sql
SELECT
    f.id, f.path, f.filename, f.size, f.modified_at, f.format,
    fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
    fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset,
    fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
    fr.focallen, fr.xpixsz, fr.pixsz, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
    fr.long_obs, fr.objctra, fr.objctdec, fr.override
FROM files f
LEFT JOIN frames fr ON f.id = fr.file_id
WHERE f.path LIKE ?1 || '%'
```

**Missing:** `fr.naxis1, fr.naxis2` (between `fr.pixsz` and `fr.ra`)

**Compare with `get_files()` (lines 208-212):**
```sql
-- Same SELECT but includes:
fr.focallen, fr.xpixsz, fr.pixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, ...
```

#### Impact

**When browsing files by directory:**

1. **Missing image dimensions**:
   - `naxis1` and `naxis2` will always be `None`
   - Image width/height not displayed in UI

2. **Broken FOV calculations**:
   - `commands.rs:27-51` calculate Field of View using `naxis1`, `naxis2`, `xpixsz`, `focallen`
   - If `naxis1`/`naxis2` are `None`, FOV calculation fails
   - FOV not shown for directory-browsed files

3. **Inconsistent UI**:
   - Files from `get_files()` show dimensions
   - Files from `get_files_by_directory()` don't show dimensions
   - Same file appears different depending on how it's loaded

#### Example

```rust
// User browses directory "/path/to/images"
let files = get_files_by_directory(&conn, "/path/to/images")?;

// File data returned:
File {
    frames: Some(Frame {
        naxis1: None,  // ❌ Should be Some(4656)
        naxis2: None,  // ❌ Should be Some(3520)
        xpixsz: Some(3.76),
        focallen: Some(600.0),
        // ...
    })
}

// FOV calculation in commands.rs:27-51:
if let (Some(naxis1), Some(naxis2), ...) = (frame.naxis1, frame.naxis2, ...) {
    // Calculate FOV
} else {
    // ❌ Fails - no dimensions available
}
```

#### Fix

**Add `fr.naxis1, fr.naxis2` to SELECT statement** (line 306):

```sql
fr.focallen, fr.xpixsz, fr.pixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, ...
```

**Update row parsing** (lines 338-370) to read from correct indexes:
```rust
// After pixsz (index 22):
naxis1: row.get(23)?,     // New index
naxis2: row.get(24)?,     // New index
ra: row.get(25)?,         // Shifted from 23
dec: row.get(26)?,        // Shifted from 24
// ... all subsequent fields shift by +2
```

---

## Medium Priority Issues

### Issue #3: XPIXSZ Fallback Inconsistency Between FITS and XISF

**Severity:** MEDIUM - Parser Inconsistency
**Files:** `src-tauri/src/fits_parser/mod.rs`
**Lines:** 173 (FITS), 382-384 (XISF)
**Status:** Should Fix for Consistency

#### Problem

The two parsers handle pixel size differently:

**FITS Parser (line 173):**
```rust
let xpixsz = read_keyword_f64(&mut fitsfile, &hdu, "XPIXSZ").ok();
```
- Only reads `XPIXSZ` keyword
- No fallback to `YPIXSZ`
- If `XPIXSZ` is missing, pixel size is `None`

**XISF Parser (lines 382-384):**
```rust
let xpixsz = fits_keywords.get("XPIXSZ")
    .and_then(|s| s.parse::<f64>().ok())
    .or_else(|| fits_keywords.get("YPIXSZ").and_then(|s| s.parse::<f64>().ok()));
```
- Reads `XPIXSZ` first
- **Falls back to `YPIXSZ`** if `XPIXSZ` is missing
- More robust handling

#### Why This Matters

Many cameras report square pixels, so:
- `XPIXSZ = YPIXSZ` (same value)
- Some software only writes `YPIXSZ`
- Some software only writes `XPIXSZ`
- Some write both

**Result:** XISF files have better pixel size detection than FITS files for the same camera.

#### Impact

**FITS file without XPIXSZ:**
```
YPIXSZ  =                 3.76 / Pixel height in microns
# XPIXSZ not present (but pixels are square)
```

- FITS parser: `xpixsz = None` ❌
- XISF parser (same header): `xpixsz = Some(3.76)` ✅

**Consequences:**
- FOV calculation fails for FITS files
- Same camera/telescope setup has different metadata depending on file format
- Inconsistent user experience

#### Fix

**Add YPIXSZ fallback to FITS parser** (line 173):

```rust
let xpixsz = read_keyword_f64(&mut fitsfile, &hdu, "XPIXSZ")
    .or_else(|_| read_keyword_f64(&mut fitsfile, &hdu, "YPIXSZ"))
    .ok();
```

---

### Issue #4: PIXSZ Not Used as Fallback

**Severity:** MEDIUM - Missing Fallback Logic
**Files:** `src-tauri/src/fits_parser/mod.rs`
**Lines:** 173-174 (FITS), 382-386 (XISF)
**Status:** Enhancement Opportunity

#### Problem

Both parsers read `PIXSZ` keyword but don't use it as a fallback for `XPIXSZ`:

**Current code (FITS):**
```rust
let xpixsz = read_keyword_f64(&mut fitsfile, &hdu, "XPIXSZ").ok();
let pixsz = read_keyword_f64(&mut fitsfile, &hdu, "PIXSZ").ok();
// Both stored separately, PIXSZ not used as fallback
```

**Suggested improvement:**
```rust
let pixsz = read_keyword_f64(&mut fitsfile, &hdu, "PIXSZ").ok();
let xpixsz = read_keyword_f64(&mut fitsfile, &hdu, "XPIXSZ")
    .or_else(|_| read_keyword_f64(&mut fitsfile, &hdu, "YPIXSZ"))
    .ok()
    .or(pixsz);  // Use PIXSZ as final fallback
```

#### Impact

Some older FITS files use `PIXSZ` for square pixels instead of separate `XPIXSZ`/`YPIXSZ`. Without fallback, these files would have:
- `xpixsz = None`
- `pixsz = Some(3.76)`
- FOV calculation fails

With fallback:
- `xpixsz = Some(3.76)` (from PIXSZ)
- `pixsz = Some(3.76)`
- FOV calculation succeeds

---

## Low Priority Issues

### Issue #5: Logging Verbosity Inconsistency

**Severity:** LOW - Developer Experience
**Files:** `src-tauri/src/fits_parser/mod.rs`
**Lines:** 155-210 (FITS), 413-426 (XISF)
**Status:** Nice to Have

#### Problem

FITS parser has much more verbose logging than XISF parser for DATE-OBS parsing:

**FITS Parser (lines 155-210):**
```rust
println!("  DATE-OBS from FITS: {:?}", date_obs_str);
println!("  TIME-OBS from FITS: {:?}", time_obs);
// ... later ...
println!("  Parsed date_obs successfully: {}", dt.to_rfc3339());
println!("  Failed to parse date_obs: {}", e);
println!("  No DATE-OBS found in FITS header!");
```
- 6 different log messages
- Detailed debugging info
- Logs raw values and parse results

**XISF Parser (lines 413-426):**
```rust
println!("  Parsed DATE-OBS successfully: {}", dt.to_rfc3339());
println!("  Failed to parse DATE-OBS '{}': {}", date_str, e);
```
- 2 log messages
- Less verbose
- No logging of raw keyword values

#### Impact

- **Debugging**: FITS parsing issues easier to diagnose than XISF issues
- **Log noise**: FITS parsing produces more console output
- **Maintenance**: Inconsistent logging patterns

#### Recommendation

Standardize logging levels:
- Use same log messages for both parsers
- Consider using proper logging crate (`log`, `tracing`) instead of `println!`
- Add log levels (DEBUG, INFO, WARN, ERROR)

---

## Database Schema Analysis

### Schema Consistency Check

**File:** `src-tauri/src/db/schema.rs`

#### Files Table (lines 6-19)

```sql
CREATE TABLE files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT NOT NULL UNIQUE,
    filename TEXT NOT NULL,
    size INTEGER NOT NULL,
    modified_at TEXT NOT NULL,
    format TEXT NOT NULL CHECK(format IN ('FITS', 'XISF')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    metadata_hash TEXT,
    content_hash TEXT
)
```

**Status:** ✅ Consistent
- All fields used in File model
- Proper constraints (UNIQUE on path, CHECK on format)
- Timestamps as TEXT (RFC3339 format)

#### Frames Table (lines 22-58)

```sql
CREATE TABLE frames (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id INTEGER NOT NULL,
    -- 28 metadata fields --
    FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
)
```

**Status:** ✅ Consistent
- All 28 fields match Frame model
- Foreign key with CASCADE delete
- Proper indexes on search fields

#### Indexes (lines 60-90)

```sql
CREATE INDEX idx_files_filename ON files(filename);
CREATE INDEX idx_files_metadata_hash ON files(metadata_hash);
CREATE INDEX idx_frames_date_obs ON frames(date_obs);
CREATE INDEX idx_frames_object ON frames(object);
CREATE INDEX idx_frames_ra ON frames(ra);
CREATE INDEX idx_frames_dec ON frames(dec);
-- ... 12 total indexes
```

**Status:** ✅ Well-Designed
- Indexes on all frequently-queried fields
- Supports fast spatial queries (ra, dec)
- Supports fast metadata searches (object, filter, exptime)

---

## Model Consistency Verification

### Frame Model vs Database Schema

**File:** `src-tauri/src/models.rs` (lines 24-58)

**Verification:** ALL fields match exactly ✅

| Model Field | Type | DB Column | DB Type | Match |
|------------|------|-----------|---------|-------|
| id | Option<i64> | id | INTEGER | ✅ |
| file_id | i64 | file_id | INTEGER | ✅ |
| object | Option<String> | object | TEXT | ✅ |
| date_obs | Option<DateTime<Utc>> | date_obs | TEXT | ✅ |
| telescop | Option<String> | telescop | TEXT | ✅ |
| instrume | Option<String> | instrume | TEXT | ✅ |
| exptime | Option<f64> | exptime | REAL | ✅ |
| filter | Option<String> | filter | TEXT | ✅ |
| imagetyp | Option<ImageType> | imagetyp | TEXT | ✅ |
| is_master | bool | is_master | INTEGER | ✅ |
| gain | Option<f64> | gain | REAL | ✅ |
| offset | Option<f64> | offset | REAL | ✅ |
| binning | Option<String> | binning | TEXT | ✅ |
| xbinning | Option<i32> | xbinning | INTEGER | ✅ |
| ybinning | Option<i32> | ybinning | INTEGER | ✅ |
| ccd_temp | Option<f64> | ccd_temp | REAL | ✅ |
| set_temp | Option<f64> | set_temp | REAL | ✅ |
| focallen | Option<f64> | focallen | REAL | ✅ |
| xpixsz | Option<f64> | xpixsz | REAL | ✅ |
| pixsz | Option<f64> | pixsz | REAL | ✅ |
| naxis1 | Option<i32> | naxis1 | INTEGER | ✅ |
| naxis2 | Option<i32> | naxis2 | INTEGER | ✅ |
| ra | Option<f64> | ra | REAL | ✅ |
| dec | Option<f64> | dec | REAL | ✅ |
| sitelat | Option<f64> | sitelat | REAL | ✅ |
| lat_obs | Option<f64> | lat_obs | REAL | ✅ |
| sitelong | Option<f64> | sitelong | REAL | ✅ |
| long_obs | Option<f64> | long_obs | REAL | ✅ |
| objctra | Option<String> | objctra | TEXT | ✅ |
| objctdec | Option<String> | objctdec | TEXT | ✅ |
| override_ | bool | override | INTEGER | ✅ |

**Total:** 31 fields, all consistent

---

## Recommendations

### Priority 1: CRITICAL - Fix Immediately

1. **Delete duplicate frame construction** in `get_frames_with_files_for_set()`
   - File: `src-tauri/src/db/operations.rs`
   - Lines: 828-862
   - Action: Delete entire second construction block
   - Reason: Will cause crashes or data corruption

### Priority 2: HIGH - Fix Soon

2. **Add naxis1/naxis2 to get_files_by_directory()**
   - File: `src-tauri/src/db/operations.rs`
   - Line: 306 (SELECT statement)
   - Lines: 338-370 (row parsing)
   - Action: Add fields and adjust indexes
   - Reason: Missing image dimensions breaks FOV calculations

### Priority 3: MEDIUM - Fix for Consistency

3. **Add YPIXSZ fallback to FITS parser**
   - File: `src-tauri/src/fits_parser/mod.rs`
   - Line: 173
   - Action: Add `.or_else(|_| read_keyword_f64(&mut fitsfile, &hdu, "YPIXSZ"))`
   - Reason: Match XISF parser behavior

4. **Add PIXSZ as final fallback for both parsers**
   - Files: `src-tauri/src/fits_parser/mod.rs`
   - Lines: 173 (FITS), 382 (XISF)
   - Action: Use PIXSZ if XPIXSZ/YPIXSZ missing
   - Reason: Support older FITS files with only PIXSZ

### Priority 4: LOW - Nice to Have

5. **Standardize logging verbosity**
   - File: `src-tauri/src/fits_parser/mod.rs`
   - Lines: 155-210 (FITS), 413-426 (XISF)
   - Action: Use same log messages, consider proper logging crate
   - Reason: Consistency and maintainability

---

## Testing Requirements

### Unit Tests

**Test duplicate frame construction fix:**
```rust
#[test]
fn test_get_frames_with_files_for_set() {
    // Create test database with frame set
    // Call get_frames_with_files_for_set()
    // Verify frame.id matches expected value
    // Verify all fields populated correctly
}
```

**Test naxis1/naxis2 in directory queries:**
```rust
#[test]
fn test_get_files_by_directory_includes_dimensions() {
    // Insert file with naxis1=4656, naxis2=3520
    // Call get_files_by_directory()
    // Assert frame.naxis1 == Some(4656)
    // Assert frame.naxis2 == Some(3520)
}
```

### Integration Tests

**Test FITS parser pixel size fallbacks:**
1. FITS with only XPIXSZ → should populate xpixsz
2. FITS with only YPIXSZ → should populate xpixsz (after fix)
3. FITS with only PIXSZ → should populate xpixsz (after enhancement)
4. FITS with all three → should prefer XPIXSZ

**Test query consistency:**
1. Insert same frame via directory and via get_files()
2. Verify all fields match including naxis1/naxis2
3. Verify FOV calculation works for both

---

## Related Documents

- `COORDINATE_ISSUES.md` - Coordinate handling problems (RA/DEC unit detection, validation)
- `FIX_IMPLEMENTATION_PLAN.md` - Detailed implementation steps for all fixes
- `COORDINATE_MIGRATION_GUIDE.md` - How to fix existing corrupted data

---

## Conclusion

The database consistency analysis revealed one **critical bug** that must be fixed immediately (duplicate frame construction), and one **high-priority issue** causing missing data (naxis1/naxis2).

The FITS and XISF parsers are mostly consistent, with the exception of pixel size handling. The database schema and models are well-designed and fully consistent.

**Action Items:**
1. Fix critical bug (delete lines 828-862 in operations.rs)
2. Add missing fields to directory query
3. Add pixel size fallbacks for consistency
4. Add comprehensive tests for all fixes

All fixes are straightforward and low-risk except for the frame construction bug which is **urgent**.
