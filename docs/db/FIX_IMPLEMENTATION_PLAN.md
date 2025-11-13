# Database and Coordinate Fixes - Implementation Plan

**Date:** 2025-11-13
**Version:** 1.0
**Status:** Ready for Implementation

## Executive Summary

This document provides detailed implementation steps for fixing all database consistency and coordinate handling issues identified in Athenaeum. Fixes are organized by priority with specific code changes, testing requirements, and expected impact.

**Total Issues:** 9 coordinate issues + 5 database issues = 14 issues
**Critical Fixes:** 2 (must fix immediately)
**High Priority:** 4 (should fix soon)
**Medium Priority:** 5 (nice to have)
**Low Priority:** 3 (future enhancements)

**Note:** This implementation plan assumes you will create a **new database from scratch** after applying fixes. No data migration is needed. The parser fixes will ensure all newly scanned files have correct coordinates.

---

## Table of Contents

1. [Implementation Phases](#implementation-phases)
2. [Phase 1: Critical Fixes](#phase-1-critical-fixes)
3. [Phase 2: High Priority Fixes](#phase-2-high-priority-fixes)
4. [Phase 3: Medium Priority Fixes](#phase-3-medium-priority-fixes)
5. [Phase 4: Low Priority Enhancements](#phase-4-low-priority-enhancements)
6. [Testing Strategy](#testing-strategy)
7. [Deployment Plan](#deployment-plan)
8. [Rollback Procedures](#rollback-procedures)

---

## Implementation Phases

### Overview

```
Phase 1: Critical Fixes (1-2 hours)
  ├─ Fix duplicate frame construction bug
  └─ Add coordinate unit detection

Phase 2: High Priority (3-4 hours)
  ├─ Add missing naxis1/naxis2 fields
  ├─ Add coordinate validation
  ├─ Convert OBJCTRA/OBJCTDEC to populate RA/DEC
  └─ Fix coordinate preference in clustering

Phase 3: Medium Priority (2-3 hours)
  ├─ Add YPIXSZ fallback to FITS parser
  ├─ Add PIXSZ as final fallback
  ├─ Remove duplicate angular_distance
  └─ Add coordinate normalization

Phase 4: Low Priority (1-2 hours)
  ├─ Standardize logging
  ├─ Add coordinate system metadata
  └─ Add precision tracking

Total Estimated Time: 7-11 hours
```

---

## Phase 1: Critical Fixes

**Priority:** CRITICAL - Fix Immediately
**Estimated Time:** 1-2 hours
**Risk Level:** Low (straightforward fixes)

### Fix #1.1: Delete Duplicate Frame Construction

**Issue:** Duplicate frame construction with wrong row indexes causes crashes
**File:** `src-tauri/src/db/operations.rs`
**Lines to Delete:** 828-862

#### Current Code (WRONG)

```rust
// Lines 828-862 - DELETE THIS ENTIRE BLOCK
let frame = crate::models::Frame {
    id: row.get(7)?,      // ❌ WRONG INDEX
    file_id: row.get(8)?,   // ❌ WRONG INDEX
    object: row.get(9)?,
    date_obs: date_obs_opt,
    telescop: row.get(11)?,
    instrume: row.get(12)?,
    exptime: row.get(13)?,
    filter: row.get(14)?,
    imagetyp: imagetyp_opt,
    is_master: row.get::<_, i64>(16)? != 0,
    gain: row.get(17)?,
    offset: row.get(18)?,
    binning: row.get(19)?,
    xbinning: row.get(20)?,
    ybinning: row.get(21)?,
    ccd_temp: row.get(22)?,
    set_temp: row.get(23)?,
    focallen: row.get(24)?,
    xpixsz: row.get(25)?,
    pixsz: row.get(26)?,
    naxis1: row.get(27)?,
    naxis2: row.get(28)?,
    ra: row.get(29)?,
    dec: row.get(30)?,
    sitelat: row.get(31)?,
    lat_obs: row.get(32)?,
    sitelong: row.get(33)?,
    long_obs: row.get(34)?,
    objctra: row.get(35)?,
    objctdec: row.get(36)?,
    override_: row.get::<_, i64>(37)? != 0,
};
```

#### Implementation Steps

1. **Locate the function** `get_frames_with_files_for_set()` in `src-tauri/src/db/operations.rs`

2. **Find lines 828-862** (the second frame construction block)

3. **Delete the entire block** from `let frame = crate::models::Frame {` to the closing `};`

4. **Verify** the first frame construction block (lines 782-816) remains intact

5. **Ensure** the function returns the correct frame construction

#### Expected Result

```rust
// Lines 782-816 - KEEP THIS (correct indexes)
let frame = crate::models::Frame {
    id: row.get(9)?,      // ✅ Correct
    file_id: row.get(10)?,  // ✅ Correct
    object: row.get(11)?,
    // ... rest of fields with correct indexes ...
};

// Lines 828-862 - DELETED (duplicate with wrong indexes)
// [Deleted block]

// Function returns the correct frame
Ok(frame_file)
```

#### Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_frames_with_files_for_set_correct_data() {
        // Setup test database
        let conn = setup_test_db();

        // Insert test frame with known values
        let test_frame = Frame {
            id: Some(1),
            file_id: 100,
            object: Some("M51".to_string()),
            exptime: Some(300.0),
            // ... other fields ...
        };
        insert_frame(&conn, &test_frame).unwrap();

        // Create frame set and add frame
        // ... setup code ...

        // Call function
        let result = get_frames_with_files_for_set(&conn, 1).unwrap();

        // Verify correct data
        assert_eq!(result.frames[0].frame.id, Some(1));
        assert_eq!(result.frames[0].frame.file_id, 100);
        assert_eq!(result.frames[0].frame.object, Some("M51".to_string()));
        assert_eq!(result.frames[0].frame.exptime, Some(300.0));
    }
}
```

---

### Fix #1.2: Add RA Unit Detection

**Issue:** RA in hours stored as degrees, causing position errors
**File:** `src-tauri/src/fits_parser/mod.rs`
**Lines to Modify:** 181-184 (FITS), 395-400 (XISF)

#### Add Helper Function

**Location:** `src-tauri/src/fits_parser/mod.rs` (after use statements, before parse_fits)

```rust
/// Detects if RA is in hours (0-24) or degrees (0-360) and normalizes to degrees
///
/// IMPROVED ALGORITHM: Uses OBJCTRA for verification when available to handle edge cases.
///
/// The FITS standard allows RA in both hours [0, 24) and degrees [0, 360).
/// For values in [0, 24), this is ambiguous without additional context.
///
/// This function uses these strategies:
/// 1. If OBJCTRA is available, parse it and compare with numeric RA to determine units
/// 2. If numeric RA matches OBJCTRA (within 0.1°), it's already in degrees
/// 3. If numeric RA * 15 matches OBJCTRA (within 0.1°), it's in hours → convert
/// 4. If no OBJCTRA, use heuristics: RA < 24 with valid DEC → assume hours
///
/// # Arguments
/// * `ra` - Raw RA value from FITS header
/// * `dec` - Optional DEC value for validation
/// * `objctra` - Optional OBJCTRA string for verification
///
/// # Returns
/// RA in decimal degrees, normalized to [0, 360)
///
/// # Edge Cases
/// - RA=0 works correctly in both hours and degrees (0h = 0°)
/// - RA in [1, 24) is verified against OBJCTRA if available
/// - Without OBJCTRA, assumes hours (common in astronomical FITS)
fn normalize_ra_from_fits(ra: f64, dec: Option<f64>, objctra: Option<&str>) -> f64 {
    // Handle RA >= 24: must be degrees
    if ra >= 24.0 {
        return crate::coordinates::normalize_ra(ra);
    }

    // Handle RA < 0: must be degrees, needs normalization
    if ra < 0.0 {
        return crate::coordinates::normalize_ra(ra);
    }

    // RA is in [0, 24): AMBIGUOUS - could be hours or degrees
    // Use OBJCTRA for verification if available
    if let Some(ra_str) = objctra {
        if let Ok(ra_from_objctra) = crate::coordinates::parse_ra_sexagesimal(ra_str) {
            // Compare numeric RA with parsed OBJCTRA
            let diff_as_degrees = (ra - ra_from_objctra).abs();
            let diff_as_hours = ((ra * 15.0) - ra_from_objctra).abs();

            // If numeric RA already matches OBJCTRA (within 0.1°), it's in degrees
            if diff_as_degrees < 0.1 {
                println!("  Verified RA already in degrees: {:.4}° (matches OBJCTRA)", ra);
                return crate::coordinates::normalize_ra(ra);
            }

            // If numeric RA * 15 matches OBJCTRA (within 0.1°), it's in hours
            if diff_as_hours < 0.1 {
                println!("  Detected RA in hours: {:.4}h → {:.4}° (verified with OBJCTRA)", ra, ra * 15.0);
                return crate::coordinates::normalize_ra(ra * 15.0);
            }

            // Neither match well - use OBJCTRA as ground truth
            println!("  WARNING: RA={:.4} doesn't match OBJCTRA. Using OBJCTRA value: {:.4}°", ra, ra_from_objctra);
            return ra_from_objctra;
        }
    }

    // No OBJCTRA available, use heuristics
    if let Some(d) = dec {
        if d >= -90.0 && d <= 90.0 {
            // Valid DEC suggests these are coordinates, assume hours
            println!("  RA={:.4} in ambiguous range [0,24). Assuming hours → {:.4}°", ra, ra * 15.0);
            return crate::coordinates::normalize_ra(ra * 15.0);
        }
    }

    // No context available, default to hours (astronomical convention)
    println!("  WARNING: RA={:.4} is ambiguous. No verification available. Assuming hours.", ra);
    crate::coordinates::normalize_ra(ra * 15.0)
}

/// Validates and normalizes DEC to [-90, 90] range
fn validate_dec(dec: f64) -> Result<f64, String> {
    if dec < -90.0 || dec > 90.0 {
        // Clamp to valid range and warn
        let clamped = crate::coordinates::normalize_dec(dec);
        println!("  WARNING: Invalid DEC={:.4}° (outside [-90, 90]). Clamped to {:.4}°", dec, clamped);
        Ok(clamped)
    } else {
        Ok(dec)
    }
}
```

#### Modify FITS Parser

**Location:** `src-tauri/src/fits_parser/mod.rs:181-184`

**Before:**
```rust
let ra = read_keyword_f64(&mut fitsfile, &hdu, "RA").ok();
let dec = read_keyword_f64(&mut fitsfile, &hdu, "DEC").ok();
let objctra = read_keyword_string(&mut fitsfile, &hdu, "OBJCTRA").ok();
let objctdec = read_keyword_string(&mut fitsfile, &hdu, "OBJCTDEC").ok();
```

**After:**
```rust
// Read raw values
let ra_raw = read_keyword_f64(&mut fitsfile, &hdu, "RA").ok();
let dec_raw = read_keyword_f64(&mut fitsfile, &hdu, "DEC").ok();
let objctra = read_keyword_string(&mut fitsfile, &hdu, "OBJCTRA").ok();
let objctdec = read_keyword_string(&mut fitsfile, &hdu, "OBJCTDEC").ok();

// Apply unit detection and validation
// Pass objctra for verification (handles RA=0 and [0,24) ambiguity correctly)
let ra = ra_raw.map(|r| normalize_ra_from_fits(r, dec_raw, objctra.as_deref()));
let dec = dec_raw.and_then(|d| validate_dec(d).ok());
```

#### Modify XISF Parser

**Location:** `src-tauri/src/fits_parser/mod.rs:395-400`

**Before:**
```rust
let ra = fits_keywords.get("RA")
    .and_then(|s| s.parse::<f64>().ok());
let dec = fits_keywords.get("DEC")
    .and_then(|s| s.parse::<f64>().ok());
let objctra = fits_keywords.get("OBJCTRA").cloned();
let objctdec = fits_keywords.get("OBJCTDEC").cloned();
```

**After:**
```rust
// Read raw values
let ra_raw = fits_keywords.get("RA")
    .and_then(|s| s.parse::<f64>().ok());
let dec_raw = fits_keywords.get("DEC")
    .and_then(|s| s.parse::<f64>().ok());
let objctra = fits_keywords.get("OBJCTRA").cloned();
let objctdec = fits_keywords.get("OBJCTDEC").cloned();

// Apply unit detection and validation
// Pass objctra for verification (handles RA=0 and [0,24) ambiguity correctly)
let ra = ra_raw.map(|r| normalize_ra_from_fits(r, dec_raw, objctra.as_deref()));
let dec = dec_raw.and_then(|d| validate_dec(d).ok());
```

#### Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ra_hours_to_degrees() {
        // RA in hours should be converted
        let ra = normalize_ra_from_fits(12.0, Some(45.0));
        assert_eq!(ra, 180.0);

        let ra = normalize_ra_from_fits(0.0, Some(0.0));
        assert_eq!(ra, 0.0);

        let ra = normalize_ra_from_fits(23.99, Some(-30.0));
        assert!((ra - 359.85).abs() < 0.01);
    }

    #[test]
    fn test_ra_degrees_passthrough() {
        // RA >= 24 should be treated as degrees
        let ra = normalize_ra_from_fits(180.0, Some(45.0));
        assert_eq!(ra, 180.0);

        let ra = normalize_ra_from_fits(270.5, Some(0.0));
        assert_eq!(ra, 270.5);
    }

    #[test]
    fn test_ra_negative_normalization() {
        // Negative RA should wrap to [0, 360)
        let ra = normalize_ra_from_fits(-10.0, Some(0.0));
        assert_eq!(ra, 350.0);
    }

    #[test]
    fn test_dec_validation() {
        assert_eq!(validate_dec(45.0).unwrap(), 45.0);
        assert_eq!(validate_dec(-90.0).unwrap(), -90.0);
        assert_eq!(validate_dec(90.0).unwrap(), 90.0);

        // Out of range should be clamped
        assert_eq!(validate_dec(100.0).unwrap(), 90.0);
        assert_eq!(validate_dec(-95.0).unwrap(), -90.0);
    }
}
```

---

## Phase 2: High Priority Fixes

**Priority:** HIGH - Fix Soon
**Estimated Time:** 3-4 hours
**Risk Level:** Low to Medium

### Fix #2.1: Add Missing naxis1/naxis2 Fields

**Issue:** Directory browse queries missing image dimensions
**File:** `src-tauri/src/db/operations.rs`
**Lines to Modify:** 306-307 (SELECT), 338-370 (row parsing)

#### Modify SELECT Statement

**Location:** `src-tauri/src/db/operations.rs:306-307`

**Before:**
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
ORDER BY f.path
```

**After:**
```sql
SELECT
    f.id, f.path, f.filename, f.size, f.modified_at, f.format,
    fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
    fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset,
    fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
    fr.focallen, fr.xpixsz, fr.pixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
    fr.long_obs, fr.objctra, fr.objctdec, fr.override
FROM files f
LEFT JOIN frames fr ON f.id = fr.file_id
WHERE f.path LIKE ?1 || '%'
ORDER BY f.path
```

**Change:** Added `fr.naxis1, fr.naxis2,` after `fr.pixsz,`

#### Update Row Parsing

**Location:** `src-tauri/src/db/operations.rs:338-370`

**Before (with indexes):**
```rust
let frame = crate::models::Frame {
    id: row.get(6)?,
    file_id: row.get(7)?,
    object: row.get(8)?,
    date_obs: date_obs_opt,
    telescop: row.get(10)?,
    instrume: row.get(11)?,
    exptime: row.get(12)?,
    filter: row.get(13)?,
    imagetyp: imagetyp_opt,
    is_master: row.get::<_, i64>(15)? != 0,
    gain: row.get(16)?,
    offset: row.get(17)?,
    binning: row.get(18)?,
    xbinning: row.get(19)?,
    ybinning: row.get(20)?,
    ccd_temp: row.get(21)?,
    set_temp: row.get(22)?,
    focallen: row.get(23)?,
    xpixsz: row.get(24)?,
    pixsz: row.get(25)?,
    ra: row.get(26)?,         // OLD: index 26
    dec: row.get(27)?,        // OLD: index 27
    sitelat: row.get(28)?,
    lat_obs: row.get(29)?,
    sitelong: row.get(30)?,
    long_obs: row.get(31)?,
    objctra: row.get(32)?,
    objctdec: row.get(33)?,
    override_: row.get::<_, i64>(34)? != 0,
};
```

**After (adjusted indexes):**
```rust
let frame = crate::models::Frame {
    id: row.get(6)?,
    file_id: row.get(7)?,
    object: row.get(8)?,
    date_obs: date_obs_opt,
    telescop: row.get(10)?,
    instrume: row.get(11)?,
    exptime: row.get(12)?,
    filter: row.get(13)?,
    imagetyp: imagetyp_opt,
    is_master: row.get::<_, i64>(15)? != 0,
    gain: row.get(16)?,
    offset: row.get(17)?,
    binning: row.get(18)?,
    xbinning: row.get(19)?,
    ybinning: row.get(20)?,
    ccd_temp: row.get(21)?,
    set_temp: row.get(22)?,
    focallen: row.get(23)?,
    xpixsz: row.get(24)?,
    pixsz: row.get(25)?,
    naxis1: row.get(26)?,     // NEW: index 26
    naxis2: row.get(27)?,     // NEW: index 27
    ra: row.get(28)?,         // SHIFTED: was 26, now 28
    dec: row.get(29)?,        // SHIFTED: was 27, now 29
    sitelat: row.get(30)?,    // SHIFTED: +2
    lat_obs: row.get(31)?,    // SHIFTED: +2
    sitelong: row.get(32)?,   // SHIFTED: +2
    long_obs: row.get(33)?,   // SHIFTED: +2
    objctra: row.get(34)?,    // SHIFTED: +2
    objctdec: row.get(35)?,   // SHIFTED: +2
    override_: row.get::<_, i64>(36)? != 0,  // SHIFTED: +2
};
```

**Change:** Added naxis1 and naxis2 at indexes 26-27, shifted all subsequent indexes by +2

#### Testing

```rust
#[test]
fn test_get_files_by_directory_includes_naxis() {
    let conn = setup_test_db();

    // Insert file with frame data including naxis1/naxis2
    let file_id = insert_test_file(&conn, "/test/dir/image.fits");
    let frame = Frame {
        file_id,
        naxis1: Some(4656),
        naxis2: Some(3520),
        // ... other fields ...
    };
    insert_frame(&conn, &frame).unwrap();

    // Query by directory
    let files = get_files_by_directory(&conn, "/test/dir").unwrap();

    // Verify naxis fields are populated
    assert_eq!(files.len(), 1);
    let frame_data = files[0].frames.as_ref().unwrap();
    assert_eq!(frame_data.naxis1, Some(4656));
    assert_eq!(frame_data.naxis2, Some(3520));
}
```

---

### Fix #2.2: Convert OBJCTRA/OBJCTDEC to Populate RA/DEC

**Issue:** Frames with only sexagesimal coordinates not queryable
**File:** `src-tauri/src/fits_parser/mod.rs`
**Lines to Add:** After reading all keywords in both parsers

#### Add Conversion Logic to FITS Parser

**Location:** `src-tauri/src/fits_parser/mod.rs` (after line 190, before Frame construction)

```rust
// After reading all keywords, convert sexagesimal if numeric RA/DEC missing
let (ra, dec) = match (ra, dec, &objctra, &objctdec) {
    // Case 1: Have numeric coordinates, use them (already normalized above)
    (Some(r), Some(d), _, _) => {
        println!("  Using numeric coordinates: RA={:.4}°, DEC={:.4}°", r, d);
        (Some(r), Some(d))
    },

    // Case 2: Missing numeric but have sexagesimal strings, convert them
    (None, None, Some(ra_str), Some(dec_str)) => {
        println!("  Converting sexagesimal coordinates...");
        match (
            crate::coordinates::parse_ra_sexagesimal(ra_str),
            crate::coordinates::parse_dec_sexagesimal(dec_str)
        ) {
            (Ok(r), Ok(d)) => {
                println!("  Successfully converted: RA={:.4}°, DEC={:.4}°", r, d);
                (Some(r), Some(d))
            },
            (Err(e1), Err(e2)) => {
                println!("  Failed to parse both: RA error: {}, DEC error: {}", e1, e2);
                (None, None)
            },
            (Err(e), _) => {
                println!("  Failed to parse RA: {}", e);
                (None, None)
            },
            (_, Err(e)) => {
                println!("  Failed to parse DEC: {}", e);
                (None, None)
            },
        }
    },

    // Case 3: Have one numeric, one sexagesimal - try to complete the pair
    (Some(r), None, _, Some(dec_str)) => {
        println!("  Have RA, converting DEC from sexagesimal...");
        match crate::coordinates::parse_dec_sexagesimal(dec_str) {
            Ok(d) => {
                println!("  Successfully converted DEC: {:.4}°", d);
                (Some(r), Some(d))
            },
            Err(e) => {
                println!("  Failed to parse DEC: {}", e);
                (Some(r), None)
            },
        }
    },
    (None, Some(d), Some(ra_str), _) => {
        println!("  Have DEC, converting RA from sexagesimal...");
        match crate::coordinates::parse_ra_sexagesimal(ra_str) {
            Ok(r) => {
                println!("  Successfully converted RA: {:.4}°", r);
                (Some(r), Some(d))
            },
            Err(e) => {
                println!("  Failed to parse RA: {}", e);
                (None, Some(d))
            },
        }
    },

    // Case 4: Have only one coordinate (partial data)
    (r, d, _, _) => {
        if r.is_some() || d.is_some() {
            println!("  WARNING: Partial coordinates (RA: {:?}, DEC: {:?})", r, d);
        }
        (r, d)
    },
};
```

#### Add Same Logic to XISF Parser

**Location:** `src-tauri/src/fits_parser/mod.rs` (after line 410, before Frame construction)

Same code as above, just placed in XISF parser section.

#### Testing

```rust
#[test]
fn test_objctra_objctdec_conversion() {
    // Create test FITS with only sexagesimal coordinates
    let fits = create_test_fits_with_objctra("12:30:00", "+45:00:00");

    let frame = parse_fits(&fits, 1).unwrap();

    // Should have converted to decimal degrees
    assert!(frame.ra.is_some());
    assert!(frame.dec.is_some());
    assert!((frame.ra.unwrap() - 187.5).abs() < 0.01);
    assert!((frame.dec.unwrap() - 45.0).abs() < 0.01);

    // Should still have original strings
    assert_eq!(frame.objctra, Some("12:30:00".to_string()));
    assert_eq!(frame.objctdec, Some("+45:00:00".to_string()));
}
```

---

### Fix #2.3: Fix Coordinate Preference in Clustering

**Issue:** Clustering prefers potentially wrong numeric RA/DEC over reliable OBJCTRA/OBJCTDEC
**File:** `src-tauri/src/clustering/mod.rs`
**Lines to Modify:** 162-170

#### Current Code (Wrong Priority)

```rust
let (ra_deg, dec_deg) = if let (Some(ra), Some(dec)) = (frame.ra, frame.dec) {
    (ra, dec)  // Uses this FIRST
} else if let (Some(objctra), Some(objctdec)) = (&frame.objctra, &frame.objctdec) {
    let ra = parse_ra_sexagesimal(objctra)?;
    let dec = parse_dec_sexagesimal(objctdec)?;
    (ra, dec)  // Uses this as FALLBACK
} else {
    return Err(anyhow!("Frame has no valid coordinates"));
};
```

#### Fixed Code (After Parser Fixes)

Since we're now populating RA/DEC from OBJCTRA/OBJCTDEC during parsing (Fix #2.2), the clustering code can be simplified:

```rust
// After parser fixes, RA/DEC should always be populated if any coordinates exist
let (ra_deg, dec_deg) = if let (Some(ra), Some(dec)) = (frame.ra, frame.dec) {
    // RA/DEC now guaranteed to be in correct units and normalized
    (ra, dec)
} else {
    return Err(anyhow!("Frame has no valid coordinates"));
};
```

**Note:** This fix depends on Fix #1.2 and Fix #2.2 being completed first.

---

### Fix #2.4: Add Coordinate Normalization to Database Operations

**Issue:** Coordinates stored without normalization
**File:** `src-tauri/src/db/operations.rs`
**Lines to Modify:** 37-87 (insert_frame function)

#### Add Validation Before Insert

**Location:** `src-tauri/src/db/operations.rs`, beginning of `insert_frame` function

**Before:**
```rust
pub fn insert_frame(conn: &Connection, frame: &Frame) -> Result<i64> {
    // Serialize DateTime to RFC3339 if present
    let date_obs_str = frame.date_obs.as_ref().map(|dt| dt.to_rfc3339());
    // ...
```

**After:**
```rust
pub fn insert_frame(conn: &Connection, frame: &Frame) -> Result<i64> {
    // Validate coordinates before insertion
    if let Some(ra) = frame.ra {
        if ra < 0.0 || ra >= 360.0 {
            return Err(anyhow!("Invalid RA: {:.4}° (must be in [0, 360))", ra));
        }
    }
    if let Some(dec) = frame.dec {
        if dec < -90.0 || dec > 90.0 {
            return Err(anyhow!("Invalid DEC: {:.4}° (must be in [-90, 90])", dec));
        }
    }

    // Serialize DateTime to RFC3339 if present
    let date_obs_str = frame.date_obs.as_ref().map(|dt| dt.to_rfc3339());
    // ...
```

**Note:** Since we're normalizing in the parser (Fix #1.2), this validation should never fail. It's a safety check.

---

## Phase 3: Medium Priority Fixes

**Priority:** MEDIUM - Nice to Have
**Estimated Time:** 2-3 hours
**Risk Level:** Low

### Fix #3.1: Add YPIXSZ Fallback to FITS Parser

**Issue:** FITS parser doesn't fall back to YPIXSZ like XISF parser does
**File:** `src-tauri/src/fits_parser/mod.rs`
**Lines to Modify:** 173

#### Current Code

```rust
let xpixsz = read_keyword_f64(&mut fitsfile, &hdu, "XPIXSZ").ok();
```

#### Fixed Code

```rust
let xpixsz = read_keyword_f64(&mut fitsfile, &hdu, "XPIXSZ")
    .or_else(|_| read_keyword_f64(&mut fitsfile, &hdu, "YPIXSZ"))
    .ok();
```

#### Testing

```rust
#[test]
fn test_xpixsz_fallback_to_ypixsz() {
    // FITS with only YPIXSZ
    let fits = create_test_fits_with_keyword("YPIXSZ", 3.76);
    let frame = parse_fits(&fits, 1).unwrap();
    assert_eq!(frame.xpixsz, Some(3.76));

    // FITS with both (should prefer XPIXSZ)
    let fits = create_test_fits_with_keywords(&[
        ("XPIXSZ", 3.76),
        ("YPIXSZ", 3.80),  // Different value
    ]);
    let frame = parse_fits(&fits, 1).unwrap();
    assert_eq!(frame.xpixsz, Some(3.76));  // Uses XPIXSZ
}
```

---

### Fix #3.2: Add PIXSZ as Final Fallback

**Issue:** PIXSZ keyword not used as fallback for square pixels
**File:** `src-tauri/src/fits_parser/mod.rs`
**Lines to Modify:** 173-174 (FITS), 382-386 (XISF)

#### FITS Parser

**Before:**
```rust
let xpixsz = read_keyword_f64(&mut fitsfile, &hdu, "XPIXSZ")
    .or_else(|_| read_keyword_f64(&mut fitsfile, &hdu, "YPIXSZ"))
    .ok();
let pixsz = read_keyword_f64(&mut fitsfile, &hdu, "PIXSZ").ok();
```

**After:**
```rust
let pixsz = read_keyword_f64(&mut fitsfile, &hdu, "PIXSZ").ok();
let xpixsz = read_keyword_f64(&mut fitsfile, &hdu, "XPIXSZ")
    .or_else(|_| read_keyword_f64(&mut fitsfile, &hdu, "YPIXSZ"))
    .ok()
    .or(pixsz);  // Use PIXSZ as final fallback
```

**Note:** Read PIXSZ first so it's available for fallback.

#### XISF Parser

**Before:**
```rust
let xpixsz = fits_keywords.get("XPIXSZ")
    .and_then(|s| s.parse::<f64>().ok())
    .or_else(|| fits_keywords.get("YPIXSZ").and_then(|s| s.parse::<f64>().ok()));
let pixsz = fits_keywords.get("PIXSZ")
    .and_then(|s| s.parse::<f64>().ok());
```

**After:**
```rust
let pixsz = fits_keywords.get("PIXSZ")
    .and_then(|s| s.parse::<f64>().ok());
let xpixsz = fits_keywords.get("XPIXSZ")
    .and_then(|s| s.parse::<f64>().ok())
    .or_else(|| fits_keywords.get("YPIXSZ").and_then(|s| s.parse::<f64>().ok()))
    .or(pixsz);  // Use PIXSZ as final fallback
```

---

### Fix #3.3: Remove Duplicate angular_distance Implementation

**Issue:** Two different implementations cause inconsistency
**Files:**
- `src-tauri/src/coordinates/mod.rs:131-146` (to remove)
- `src-tauri/src/selection/algorithms.rs:13-31` (keep this one)
- `src-tauri/src/clustering/mod.rs:53` (update to use selection::algorithms)

#### Step 1: Update Clustering to Use Haversine

**File:** `src-tauri/src/clustering/mod.rs:53`

**Before:**
```rust
let distance = crate::coordinates::angular_distance(
    cf1.ra_deg, cf1.dec_deg,
    cf2.ra_deg, cf2.dec_deg
);
```

**After:**
```rust
let distance = crate::selection::algorithms::angular_distance(
    cf1.ra_deg, cf1.dec_deg,
    cf2.ra_deg, cf2.dec_deg
);
```

#### Step 2: Remove Function from coordinates/mod.rs

**File:** `src-tauri/src/coordinates/mod.rs:131-146`

**Delete this function:**
```rust
pub fn angular_distance(ra1_deg: f64, dec1_deg: f64, ra2_deg: f64, dec2_deg: f64) -> f64 {
    let ra1 = ra1_deg.to_radians();
    let dec1 = dec1_deg.to_radians();
    let ra2 = ra2_deg.to_radians();
    let dec2 = dec2_deg.to_radians();

    let cos_angle = dec1.sin() * dec2.sin() + dec1.cos() * dec2.cos() * (ra2 - ra1).cos();
    let cos_angle = cos_angle.clamp(-1.0, 1.0);
    cos_angle.acos().to_degrees()
}
```

#### Step 3: Update Documentation

Add comment to `selection/algorithms.rs`:

```rust
/// Calculates angular distance between two points on celestial sphere using Haversine formula
///
/// This is the standard implementation used throughout Athenaeum for:
/// - Spatial queries (circle, rectangle)
/// - Frame set clustering (DBSCAN)
/// - Distance calculations in sky atlas
///
/// The Haversine formula is numerically stable for small angles and provides
/// accurate results for astronomical coordinate distances.
pub fn angular_distance(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    // ... existing implementation ...
}
```

---

## Phase 4: Low Priority Enhancements

**Priority:** LOW - Future Enhancements
**Estimated Time:** 1-2 hours
**Risk Level:** Low

### Fix #4.1: Standardize Logging Verbosity

**Issue:** Inconsistent logging between FITS and XISF parsers
**Files:** `src-tauri/src/fits_parser/mod.rs`

#### Recommendation

Consider migrating from `println!` to proper logging framework:

```rust
// Add to Cargo.toml:
// log = "0.4"
// env_logger = "0.11"  // or tracing for more features

// In code:
use log::{debug, info, warn, error};

// Replace println! calls:
debug!("  DATE-OBS from FITS: {:?}", date_obs_str);
info!("  Parsed date_obs successfully: {}", dt.to_rfc3339());
warn!("  Failed to parse date_obs: {}", e);
error!("  No DATE-OBS found in FITS header!");
```

Benefits:
- Can control log levels at runtime
- Consistent format across application
- Can filter by module/file
- Production-ready logging

---

### Fix #4.2: Add Coordinate System Metadata

**Issue:** No tracking of EQUINOX, RADESYS, etc.
**Status:** Future enhancement, low priority for amateur astrophotography

#### Database Schema Changes

```sql
ALTER TABLE frames ADD COLUMN equinox TEXT;  -- 'B1950', 'J2000', etc.
ALTER TABLE frames ADD COLUMN radesys TEXT;  -- 'FK4', 'FK5', 'ICRS', etc.
```

#### Model Changes

```rust
pub struct Frame {
    // ... existing fields ...
    pub equinox: Option<String>,
    pub radesys: Option<String>,
}
```

#### Parser Changes

```rust
let equinox = read_keyword_string(&mut fitsfile, &hdu, "EQUINOX").ok();
let radesys = read_keyword_string(&mut fitsfile, &hdu, "RADESYS").ok();
```

---

## Testing Strategy

### Unit Tests

Create `src-tauri/src/fits_parser/tests.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Coordinate tests
    #[test] fn test_ra_hours_conversion() { /* ... */ }
    #[test] fn test_ra_degrees_passthrough() { /* ... */ }
    #[test] fn test_dec_validation() { /* ... */ }

    // Parser tests
    #[test] fn test_fits_with_ra_in_hours() { /* ... */ }
    #[test] fn test_xisf_with_objctra_only() { /* ... */ }
    #[test] fn test_pixel_size_fallbacks() { /* ... */ }

    // Integration tests
    #[test] fn test_complete_parsing_workflow() { /* ... */ }
}
```

### Integration Tests

Test with real FITS files:
1. Download sample FITS from various observatories
2. Test parsing and coordinate extraction
3. Verify consistency between FITS and XISF

### Regression Tests

```bash
# Before making changes
cargo test > tests_before.txt

# After each fix
cargo test > tests_after.txt
diff tests_before.txt tests_after.txt

# Ensure no new failures
```

---

## Deployment Plan

### Pre-Deployment

1. **Backup database**:
   ```bash
   cp ~/.local/share/com.athenaeum.app/athenaeum.db athenaeum_backup_$(date +%Y%m%d).db
   ```

2. **Run all tests**:
   ```bash
   cd src-tauri && cargo test --all
   ```

3. **Build release**:
   ```bash
   npm run tauri build
   ```

### Deployment Steps

1. Deploy Phase 1 fixes first (critical)
2. Monitor for issues
3. Deploy Phase 2 fixes (high priority)
4. Deploy Phase 3 fixes (medium priority)
5. Phase 4 can be deployed when convenient

### Post-Deployment

1. **Run data migration** (see `COORDINATE_MIGRATION_GUIDE.md`)
2. **Verify spatial queries** return expected results
3. **Check frame set clustering** positions are correct
4. **Monitor logs** for any parsing errors

---

## Rollback Procedures

### If Critical Issues Arise

1. **Stop application**
2. **Restore database backup**:
   ```bash
   cp athenaeum_backup_YYYYMMDD.db ~/.local/share/com.athenaeum.app/athenaeum.db
   ```
3. **Revert to previous application version**
4. **Investigate issues before retry**

### Incremental Rollback

If specific fix causes issues:
1. Use `git revert <commit>` to undo that specific fix
2. Rebuild and redeploy
3. Database migration may need to be re-run

---

## Summary Checklist

### Phase 1 (Critical)
- [ ] Delete duplicate frame construction (operations.rs:828-862)
- [ ] Add normalize_ra_from_fits() helper function
- [ ] Add validate_dec() helper function
- [ ] Update FITS parser to use helpers
- [ ] Update XISF parser to use helpers
- [ ] Add unit tests for coordinate conversion
- [ ] Verify clustering still works

### Phase 2 (High Priority)
- [ ] Add naxis1/naxis2 to get_files_by_directory SELECT
- [ ] Update row parsing indexes
- [ ] Add OBJCTRA/OBJCTDEC conversion in FITS parser
- [ ] Add OBJCTRA/OBJCTDEC conversion in XISF parser
- [ ] Simplify clustering coordinate extraction
- [ ] Add coordinate validation in insert_frame
- [ ] Add integration tests

### Phase 3 (Medium Priority)
- [ ] Add YPIXSZ fallback to FITS parser
- [ ] Add PIXSZ final fallback to both parsers
- [ ] Update clustering to use haversine
- [ ] Remove duplicate angular_distance from coordinates module
- [ ] Update documentation

### Phase 4 (Low Priority)
- [ ] Migrate to proper logging framework
- [ ] Add coordinate system metadata (optional)
- [ ] Add precision tracking (optional)

---

## Related Documents

- `COORDINATE_ISSUES.md` - Detailed analysis of coordinate problems
- `DATABASE_CONSISTENCY_ISSUES.md` - Database and parser inconsistencies
- `COORDINATE_MIGRATION_GUIDE.md` - How to fix existing data

---

## Conclusion

This implementation plan provides step-by-step instructions for fixing all identified issues. The fixes are organized by priority and risk level, with comprehensive testing requirements for each change.

**Estimated Total Time:** 7-11 hours for all phases
**Recommended Approach:** Complete Phase 1 and Phase 2 before deploying to production
**Success Criteria:** All tests pass, spatial queries accurate, no data corruption

The most critical fixes (Phase 1) are straightforward deletions and additions that should take 1-2 hours to implement and test thoroughly.
