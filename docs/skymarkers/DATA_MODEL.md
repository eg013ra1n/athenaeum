# SkyAtlas Data Model

**Date:** 2025-11-13
**Version:** 1.0
**Status:** Documentation

---

## Table of Contents

1. [Overview](#overview)
2. [ImagingLocation Interface](#imaginglocation-interface)
3. [Database Schema Relationships](#database-schema-relationships)
4. [Property Descriptions](#property-descriptions)
5. [Data Type Mappings](#data-type-mappings)
6. [Validation Rules](#validation-rules)
7. [Coordinate Conventions](#coordinate-conventions)
8. [Example Data](#example-data)

---

## Overview

The SkyAtlas feature uses the `ImagingLocation` data structure to represent both organized frame sets and unorganized frame clusters on the celestial map.

**Key Concept:** A single unified model represents two distinct entity types:
- **Frame Sets**: Organized imaging sessions with explicit user grouping
- **Clusters**: Automatic groupings of unorganized frames by sky position

The `location_type` property acts as the **discriminator** between these types.

---

## ImagingLocation Interface

### TypeScript Definition

**Location:** `src/types/models.ts`, lines 323-336

```typescript
export interface ImagingLocation {
  id: number;                         // Unique location identifier
  ra: number;                          // Right Ascension in decimal degrees [0, 360)
  dec: number;                         // Declination in decimal degrees [-90, 90]
  object_name: string | null;          // Target object name (e.g., "M31 Andromeda")
  frame_count: number;                 // Total number of frames in location
  total_exposure: number;              // Total exposure time in seconds
  filters: string[];                   // List of filters used (e.g., ["Ha", "OIII"])
  date_range: [string, string];        // [earliest, latest] ISO 8601 timestamps
  frame_set_id: number | null;         // Frame set ID (NULL for clusters)
  fov_width: number | null;            // Field of view width in degrees
  fov_height: number | null;           // Field of view height in degrees
  location_type: 'frameset' | 'cluster';  // Discriminator: type of location
}
```

### Rust Definition

**Location:** `src-tauri/src/models.rs`, lines 396-412

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagingLocation {
    pub id: i64,
    pub ra: f64,
    pub dec: f64,
    pub object_name: Option<String>,
    pub frame_count: i32,
    pub total_exposure: f64,
    pub filters: Vec<String>,
    pub date_range: (String, String),
    pub frame_set_id: Option<i64>,
    pub fov_width: Option<f64>,
    pub fov_height: Option<f64>,
    pub location_type: String,  // "frameset" or "cluster"
}
```

**Serde Attributes:**
- `#[derive(Serialize, Deserialize)]`: Enables JSON serialization for Tauri IPC
- Fields serialize/deserialize automatically between Rust and TypeScript

---

## Database Schema Relationships

### Frame Set Hierarchy

```
projects
    ↓
frames_set ────────────┐
    ↓                  │
imaging_nights         │
    ↓                  │  Aggregated into
sessions               │  ImagingLocation
    ↓                  │  (location_type = 'frameset')
session_members        │
    ↓                  │
frames ────────────────┘
```

**Table Definitions:**

#### frames_set
```sql
CREATE TABLE frames_set (
    id INTEGER PRIMARY KEY,
    project_id INTEGER,
    name TEXT NOT NULL,
    objctra TEXT,
    objctdec TEXT,
    FOREIGN KEY (project_id) REFERENCES projects(id)
);
```

**Maps to:**
- `ImagingLocation.frame_set_id` = `frames_set.id`
- `ImagingLocation.object_name` = `frames_set.name`

#### imaging_nights
```sql
CREATE TABLE imaging_nights (
    id INTEGER PRIMARY KEY,
    frames_set_id INTEGER NOT NULL,
    night_date TEXT NOT NULL,
    FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE
);
```

**Purpose:** Groups sessions by observing night within a frame set

#### sessions
```sql
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    imaging_night_id INTEGER NOT NULL,
    instrume TEXT,
    telescop TEXT,
    focallen REAL,
    xpixsz REAL,
    setup_hash TEXT,
    FOREIGN KEY (imaging_night_id) REFERENCES imaging_nights(id) ON DELETE CASCADE
);
```

**Purpose:** Groups frames by instrument/setup within a night

**Maps to:** FOV calculation uses `focallen`, `xpixsz` from sessions

#### session_members
```sql
CREATE TABLE session_members (
    session_id INTEGER NOT NULL,
    frame_id INTEGER NOT NULL,
    PRIMARY KEY (session_id, frame_id),
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE,
    FOREIGN KEY (frame_id) REFERENCES frames(id) ON DELETE CASCADE
);
```

**Purpose:** Junction table linking frames to sessions

#### frames
```sql
CREATE TABLE frames (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL,
    object TEXT,
    date_obs TEXT,
    ra REAL,
    dec REAL,
    exptime REAL,
    filter TEXT,
    imagetyp TEXT,
    instrume TEXT,
    focallen REAL,
    xpixsz REAL,
    naxis1 INTEGER,
    naxis2 INTEGER,
    xbinning INTEGER,
    ybinning INTEGER,
    objctra TEXT,
    objctdec TEXT,
    -- ... many other fields
    FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
);
```

**Maps to:**
- `ImagingLocation.ra` = `AVG(frames.ra)`
- `ImagingLocation.dec` = `AVG(frames.dec)`
- `ImagingLocation.frame_count` = `COUNT(frames.id)`
- `ImagingLocation.total_exposure` = `SUM(frames.exptime)`
- `ImagingLocation.filters` = `GROUP_CONCAT(DISTINCT frames.filter)`
- `ImagingLocation.date_range` = `[MIN(frames.date_obs), MAX(frames.date_obs)]`

### Cluster Structure

Clusters do NOT have a persistent database representation. They are computed on-the-fly by the query:

```sql
SELECT
    NULL as frame_set_id,  -- No persistent ID
    COALESCE(fr.object, 'Unknown') as object_name,
    AVG(fr.ra) as avg_ra,
    AVG(fr.dec) as avg_dec,
    -- ... aggregations
FROM frames fr
WHERE NOT EXISTS (SELECT 1 FROM session_members sm WHERE sm.frame_id = fr.id)
GROUP BY COALESCE(fr.object, 'Unknown'), ROUND(fr.ra, 1), ROUND(fr.dec, 1);
```

**Key Point:** Clusters are ephemeral groupings. The `id` field for clusters is generated sequentially in the query result, not from a database table.

---

## Property Descriptions

### id

**Type:** `number` (TypeScript) / `i64` (Rust)

**Description:** Unique identifier for this imaging location

**Source:**
- For frame sets: `frames_set.id`
- For clusters: Sequential number assigned during query execution (not persistent)

**Usage:**
- Unique key for React rendering
- Not used for navigation (use `frame_set_id` instead)

**Validation:** Always > 0

---

### ra

**Type:** `number` (TypeScript) / `f64` (Rust)

**Description:** Right Ascension in decimal degrees

**Range:** [0, 360)

**Source:** Averaged from all LIGHT frames in the location
- SQL: `AVG(frames.ra)`

**Precision:** Typically 6 decimal places (≈0.04 arcsec)

**Coordinate System:** ICRS (International Celestial Reference System)
- Equinox: J2000.0
- Frame: ICRS (aligned with J2000 within measurement error)

**Important:** RA is stored in **degrees**, not hours. To convert:
- Degrees to hours: `ra_hours = ra_degrees / 15`
- Hours to degrees: `ra_degrees = ra_hours * 15`

**Example:**
- RA = 83.822458° (degrees)
- RA = 5h 35m 17.4s (hours:minutes:seconds)

---

### dec

**Type:** `number` (TypeScript) / `f64` (Rust)

**Description:** Declination in decimal degrees

**Range:** [-90, 90]

**Source:** Averaged from all LIGHT frames in the location
- SQL: `AVG(frames.dec)`

**Precision:** Typically 6 decimal places (≈0.04 arcsec)

**Sign Convention:**
- Positive: North of celestial equator
- Negative: South of celestial equator
- 0: On celestial equator

**Example:**
- DEC = -5.391111° (degrees)
- DEC = -5° 23' 28" (degrees:arcmin:arcsec)

---

### object_name

**Type:** `string | null` (TypeScript) / `Option<String>` (Rust)

**Description:** Name of the target celestial object

**Source:**
- For frame sets: `frames_set.name`
- For clusters: `COALESCE(frames.object, 'Unknown')`

**Null Handling:**
- Can be null in TypeScript (though rare)
- Clusters default to 'Unknown' if frames lack object names

**Common Values:**
- Messier objects: "M31", "M42", "M45"
- NGC/IC objects: "NGC 7000", "IC 1396"
- Common names: "Andromeda Galaxy", "Orion Nebula"
- Unknown: "Unknown" (for unidentified targets)

**Usage:** Displayed in map tooltips and marker labels

---

### frame_count

**Type:** `number` (TypeScript) / `i32` (Rust)

**Description:** Total number of LIGHT frames in this location

**Source:** `COUNT(DISTINCT frames.id)` or `COUNT(frames.id)`

**Range:** Typically 1 to 1000+

**Excludes:** Calibration frames (Dark, Flat, Bias)

**Usage:**
- Displayed in tooltip: "120 frames"
- Indicates data completeness

---

### total_exposure

**Type:** `number` (TypeScript) / `f64` (Rust)

**Description:** Cumulative exposure time in **seconds**

**Source:** `SUM(frames.exptime)`

**Range:** Typically 0 to 100,000+ seconds

**Conversion:**
- To hours: `hours = total_exposure / 3600`
- To minutes: `minutes = total_exposure / 60`

**Example:**
- 37800 seconds = 10.5 hours = 630 minutes

**Usage:**
- Displayed in tooltip: "10.5 hours"
- Indicates total integration time

---

### filters

**Type:** `string[]` (TypeScript) / `Vec<String>` (Rust)

**Description:** List of unique filter names used

**Source:** `GROUP_CONCAT(DISTINCT frames.filter)` (comma-separated)
- Rust splits string into Vec
- TypeScript receives as array

**Common Values:**
- Narrowband: "Ha", "OIII", "SII", "NII"
- Broadband RGB: "Red", "Green", "Blue"
- Luminance: "Lum", "L"
- Clear/no filter: "None", "" (empty string)

**Null Handling:** Empty array `[]` if no frames have filter data

**Usage:**
- Displayed in tooltip: "Filters: Ha, OIII, SII"
- Comma-joined for display

---

### date_range

**Type:** `[string, string]` (TypeScript) / `(String, String)` (Rust)

**Description:** Tuple of [earliest, latest] observation timestamps

**Format:** ISO 8601 with timezone: `YYYY-MM-DDTHH:MM:SSZ`

**Source:**
- `[0]` = `MIN(frames.date_obs)`
- `[1]` = `MAX(frames.date_obs)`

**Example:**
```typescript
date_range: ["2024-09-15T22:30:00Z", "2024-10-20T23:45:00Z"]
```

**Duration Calculation:**
```typescript
const start = new Date(date_range[0]);
const end = new Date(date_range[1]);
const days = (end - start) / (1000 * 60 * 60 * 24);
```

**Usage:**
- Displayed in tooltip: "2024-09-15 to 2024-10-20"
- Indicates observing campaign duration

---

### frame_set_id

**Type:** `number | null` (TypeScript) / `Option<i64>` (Rust)

**Description:** Foreign key to `frames_set` table

**Values:**
- **Non-null:** Location represents a frame set (organized)
- **Null:** Location represents a cluster (unorganized)

**Usage:**
- **Navigation:** Click handler uses this to navigate to frame set detail page
  ```typescript
  if (frame_set_id) {
    navigate(`/objects/${frame_set_id}`);
  }
  ```
- **Click behavior:** Only frame set markers are clickable
- **Tooltip label:** Determines "[Frame Set]" vs "[Unorganized]"

**Important:** This is the **discriminator** between clickable and non-clickable markers.

---

### fov_width

**Type:** `number | null` (TypeScript) / `Option<f64>` (Rust)

**Description:** Field of view width in decimal degrees

**Source:** Calculated from sensor metadata
```rust
fov_width = calculate_fov(xpixsz, focallen, naxis1, xbinning)
```

**Formula:**
```
FOV = 2 * arctan((pixel_size_mm * sensor_pixels * binning) / (2 * focal_length_mm))
```

**Range:** Typically 0.1° to 10° for amateur equipment

**Null Conditions:**
- Missing XPIXSZ (pixel size)
- Missing FOCALLEN (focal length)
- Missing NAXIS1 (sensor width)
- Missing XBINNING (binning factor)

**Usage:**
- When zoomed in, draws FOV rectangle
- Width dimension aligned with RA axis

**Example:**
- 1.487° = 1° 29' 13" = 89.2 arcmin

---

### fov_height

**Type:** `number | null` (TypeScript) / `Option<f64>` (Rust)

**Description:** Field of view height in decimal degrees

**Source:** Calculated from sensor metadata
```rust
fov_height = calculate_fov(xpixsz, focallen, naxis2, ybinning)
```

**Same formula as fov_width**, but using:
- NAXIS2 (sensor height in pixels)
- YBINNING (vertical binning factor)

**Usage:**
- When zoomed in, draws FOV rectangle
- Height dimension aligned with DEC axis

**Note:** For square pixels and 1:1 binning, fov_height often differs from fov_width due to non-square sensor (e.g., 4656×3520 pixels).

---

### location_type

**Type:** `'frameset' | 'cluster'` (TypeScript) / `String` (Rust)

**Description:** Discriminator indicating location type

**Values:**
- `"frameset"`: Organized frame set
- `"cluster"`: Unorganized frame cluster

**Source:** Hardcoded in SQL query
- Branch 1: `'frameset' as location_type`
- Branch 2: `'cluster' as location_type`

**Usage:**
- **Tooltip prefix:** Determines "[Frame Set]" vs "[Unorganized]"
- **Click behavior:** Frame sets navigate, clusters don't
- **Cursor style:** `pointer` for framesets, `default` for clusters
- **Future styling:** Could differentiate marker appearance

**TypeScript Type Guard:**
```typescript
function isFrameSet(location: ImagingLocation): boolean {
  return location.location_type === 'frameset';
}
```

**Important:** This is a string in Rust but has a union type in TypeScript for type safety.

---

## Data Type Mappings

### Rust → TypeScript Serialization

Tauri serializes Rust structs to JSON, which TypeScript deserializes:

| Rust Type | JSON Type | TypeScript Type | Example |
|-----------|-----------|-----------------|---------|
| `i64` | Number | `number` | `123` |
| `f64` | Number | `number` | `83.822458` |
| `String` | String | `string` | `"M31 Andromeda"` |
| `Option<String>` | String or Null | `string \| null` | `"Ha"` or `null` |
| `Option<i64>` | Number or Null | `number \| null` | `5` or `null` |
| `Option<f64>` | Number or Null | `number \| null` | `1.487` or `null` |
| `Vec<String>` | Array | `string[]` | `["Ha", "OIII"]` |
| `(String, String)` | Array | `[string, string]` | `["2024-01-01T00:00:00Z", "2024-12-31T23:59:59Z"]` |

### Special Considerations

**Floating Point Precision:**
- Rust `f64` has ~15-17 decimal digits precision
- JSON numbers preserve precision
- TypeScript `number` (IEEE 754 double) matches Rust `f64`

**Integer Overflow:**
- Rust `i64` range: -2^63 to 2^63-1
- JSON number can represent integers up to 2^53-1 safely
- Database IDs stay well within safe range

**Date/Time:**
- Rust stores as ISO 8601 strings
- TypeScript receives as strings
- Convert to Date objects on frontend:
  ```typescript
  const date = new Date(location.date_range[0]);
  ```

---

## Validation Rules

### Backend Validation (SQL Query)

**Enforced by WHERE clause:**

1. **Coordinates must exist:**
   ```sql
   WHERE fr.ra IS NOT NULL AND fr.dec IS NOT NULL
   ```

2. **Only LIGHT frames:**
   ```sql
   WHERE fr.imagetyp = 'Light'
   ```

3. **Averaged coordinates must be valid:**
   ```sql
   HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL
   ```

### Frontend Validation (TypeScript)

**Location:** `SkyAtlas.tsx`, lines 264-277

```typescript
const validLocs = locations.filter(loc => {
  const isValidRA = loc.ra !== null && !isNaN(loc.ra) && isFinite(loc.ra);
  const isValidDec = loc.dec !== null && !isNaN(loc.dec) && isFinite(loc.dec);
  return isValidRA && isValidDec;
});
```

**Checks:**
- Not null
- Not NaN (invalid float)
- Finite (not Infinity or -Infinity)

**Failed Validation:**
- Location excluded from map
- Warning logged: "No valid imaging locations to display"

### Data Integrity Constraints

**Assumed invariants:**

1. **RA range:** `0 ≤ ra < 360`
   - Ensured by coordinate normalization in parser
   - Frontend doesn't validate range (trusts backend)

2. **DEC range:** `-90 ≤ dec ≤ 90`
   - Ensured by coordinate validation in parser
   - Frontend doesn't validate range

3. **frame_count > 0:**
   - Ensured by SQL aggregation (COUNT always ≥ 1)
   - Empty groups filtered by HAVING clause

4. **total_exposure ≥ 0:**
   - Individual frame exposures should be positive
   - Sum cannot be negative

5. **date_range[0] ≤ date_range[1]:**
   - MIN ≤ MAX by definition
   - Could be equal for single-night observations

---

## Coordinate Conventions

### RA/DEC Storage Format

**Database:** Decimal degrees
- RA: [0, 360)
- DEC: [-90, 90]

**Display Format:** Sexagesimal (hours/degrees, minutes, seconds)
- RA: `HH:MM:SS.S` (hours)
- DEC: `±DD:MM:SS` (degrees)

**Conversion Functions:**

```typescript
// Degrees to HMS (for RA)
function raDegreesToHMS(degrees: number): string {
  const hours = degrees / 15;
  const h = Math.floor(hours);
  const m = Math.floor((hours - h) * 60);
  const s = ((hours - h) * 60 - m) * 60;
  return `${h}h ${m}m ${s.toFixed(1)}s`;
}

// Degrees to DMS (for DEC)
function decDegreesToDMS(degrees: number): string {
  const sign = degrees >= 0 ? '+' : '-';
  const absDeg = Math.abs(degrees);
  const d = Math.floor(absDeg);
  const m = Math.floor((absDeg - d) * 60);
  const s = ((absDeg - d) * 60 - m) * 60;
  return `${sign}${d}° ${m}' ${s.toFixed(0)}"`;
}
```

### GeoJSON Coordinate System

**Location:** `SkyAtlas.tsx`, lines 231-233

```typescript
const raToGeoJsonLongitude = (ra: number) => {
  return ra;  // Direct mapping: RA degrees → GeoJSON longitude
};
```

**Convention:**
- RA (0-360°) maps directly to GeoJSON longitude
- DEC (-90 to 90°) maps directly to GeoJSON latitude
- No offset or transformation needed (Option A)

**D3-Celestial Projection:**
- Receives GeoJSON coordinates
- Projects onto 2D canvas using selected projection (e.g., Aitoff, Mollweide)
- Handles RA wrapping at 0°/360° boundary

---

## Example Data

### Frame Set Example

```typescript
const frameSetLocation: ImagingLocation = {
  id: 5,
  ra: 10.684792,                    // 0h 42m 44.35s
  dec: 41.268750,                   // +41° 16' 7.5"
  object_name: "M31 Andromeda",
  frame_count: 120,
  total_exposure: 37800.0,          // 10.5 hours
  filters: ["Ha", "OIII", "SII"],
  date_range: [
    "2024-09-15T22:30:00Z",
    "2024-10-20T23:45:00Z"
  ],
  frame_set_id: 5,                  // Clickable (navigates to /objects/5)
  fov_width: 1.487,                 // 1.487° ≈ 89 arcmin
  fov_height: 1.122,                // 1.122° ≈ 67 arcmin
  location_type: "frameset"
};
```

**Map Display:**
- Green cross or FOV rectangle
- Tooltip: "[Frame Set] M31 Andromeda / 120 frames | 10.50 hours / Filters: Ha, OIII, SII / 2024-09-15 to 2024-10-20"
- Click navigates to frame set detail page

### Cluster Example

```typescript
const clusterLocation: ImagingLocation = {
  id: 23,
  ra: 312.534,                      // 20h 50m 8.16s
  dec: 44.321,                      // +44° 19' 15.6"
  object_name: "NGC 7000",
  frame_count: 45,
  total_exposure: 13500.0,          // 3.75 hours
  filters: ["Ha", "OIII"],
  date_range: [
    "2024-08-10T21:15:00Z",
    "2024-08-25T22:30:00Z"
  ],
  frame_set_id: null,               // Not clickable
  fov_width: 2.456,
  fov_height: 1.852,
  location_type: "cluster"
};
```

**Map Display:**
- Green cross or FOV rectangle
- Tooltip: "[Unorganized] NGC 7000 / 45 frames | 3.75 hours / Filters: Ha, OIII / 2024-08-10 to 2024-08-25"
- Click has no effect (cluster not clickable)

### Cluster Without FOV

```typescript
const clusterNoFOV: ImagingLocation = {
  id: 47,
  ra: 83.822458,                    // 5h 35m 17.39s
  dec: -5.391111,                   // -5° 23' 28"
  object_name: "M42 Orion Nebula",
  frame_count: 15,
  total_exposure: 3600.0,           // 1 hour
  filters: ["Ha"],
  date_range: [
    "2024-12-05T18:00:00Z",
    "2024-12-05T20:00:00Z"
  ],
  frame_set_id: null,
  fov_width: null,                  // No FOV data
  fov_height: null,
  location_type: "cluster"
};
```

**Map Display:**
- Green cross only (no FOV rectangle)
- Same tooltip format as other clusters

---

## Related Documentation

- [MARKER_DISPLAY_SYSTEM.md](./MARKER_DISPLAY_SYSTEM.md) - Frontend rendering
- [SQL_QUERIES.md](./SQL_QUERIES.md) - Backend queries
- [TROUBLESHOOTING_FRAMESETS.md](./TROUBLESHOOTING_FRAMESETS.md) - Diagnostics
- `../db/schema.rs` - Complete database schema

---

## Version History

**v1.0 (2025-11-13):**
- Initial data model documentation
- Complete property descriptions
- Validation rules
- Coordinate conventions
- Example data
