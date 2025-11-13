# SkyAtlas Marker Display System

**Date:** 2025-11-13
**Version:** 1.0
**Status:** Documentation

---

## Table of Contents

1. [Overview](#overview)
2. [Data Flow Pipeline](#data-flow-pipeline)
3. [Marker Types](#marker-types)
4. [Coordinate System](#coordinate-system)
5. [Rendering Pipeline](#rendering-pipeline)
6. [Styling Specifications](#styling-specifications)
7. [FOV Rectangle Display](#fov-rectangle-display)
8. [Tooltip System](#tooltip-system)

---

## Overview

The SkyAtlas marker display system visualizes imaging locations on an all-sky celestial map using D3.js and d3-celestial. Markers represent either:
- **Frame Sets**: Organized collections of frames grouped into imaging sessions
- **Clusters**: Unorganized frames grouped by sky coordinates and object name

The system handles coordinate transformation, data validation, dynamic rendering based on zoom level, and interactive tooltips.

**Key Files:**
- Frontend: `src/pages/SkyAtlas.tsx`
- Backend: `src-tauri/src/commands.rs` (function `get_imaging_locations`, lines 1454-1600)
- Types: `src/types/models.ts` (ImagingLocation interface)

---

## Data Flow Pipeline

### 1. Data Fetching (Frontend)

**Location:** `SkyAtlas.tsx`, lines 49-62

```typescript
useEffect(() => {
  async function loadLocations() {
    const data = await invoke<ImagingLocation[]>('get_imaging_locations');
    setLocations(data);
  }
  loadLocations();
}, []);
```

**Process:**
1. Component mounts
2. Invokes Tauri command `get_imaging_locations`
3. Receives array of `ImagingLocation` objects
4. Stores in React state

### 2. Backend Query Execution

**Location:** `src-tauri/src/commands.rs`, lines 1454-1600

**Process:**
1. Command receives request
2. Executes SQL query with UNION of two branches:
   - Branch 1: Organized frame sets (location_type = 'frameset')
   - Branch 2: Unorganized clusters (location_type = 'cluster')
3. Calculates FOV for frames with complete sensor data
4. Returns `Vec<ImagingLocation>`
5. Logs count summary: `"Found X imaging locations (Y framesets, Z clusters)"`

See [SQL_QUERIES.md](./SQL_QUERIES.md) for detailed query breakdown.

### 3. Data Validation

**Location:** `SkyAtlas.tsx`, lines 264-277

```typescript
const validLocs = locations.filter(loc => {
  const isValidRA = loc.ra !== null && !isNaN(loc.ra) && isFinite(loc.ra);
  const isValidDec = loc.dec !== null && !isNaN(loc.dec) && isFinite(loc.dec);
  return isValidRA && isValidDec;
});
```

**Validation Rules:**
- RA must be non-null, not NaN, and finite
- DEC must be non-null, not NaN, and finite
- Invalid locations are filtered out
- Warning logged if no valid locations: `"No valid imaging locations to display"`

### 4. GeoJSON Feature Creation

**Location:** `SkyAtlas.tsx`, lines 280-307

```typescript
const features = [
  ...validLocs.map(loc => ({
    type: 'Feature',
    id: loc.id,
    properties: {
      name: loc.object_name || 'Unknown',
      object_name: loc.object_name,
      frame_count: loc.frame_count,
      total_exposure: loc.total_exposure,
      filters: loc.filters.join(', '),
      date_range: loc.date_range,
      frame_set_id: loc.frame_set_id,
      location_type: loc.location_type,
      fov_width: loc.fov_width,
      fov_height: loc.fov_height,
      original_ra: loc.ra  // Keep original RA for FOV calculations
    },
    geometry: {
      type: 'Point',
      coordinates: [raToGeoJsonLongitude(loc.ra), loc.dec]
    }
  }))
];
```

**Feature Properties:**
- All metadata preserved for tooltip display
- `original_ra` stored for FOV rectangle calculations
- `location_type` used for styling differentiation

### 5. D3-Celestial Integration

**Location:** `SkyAtlas.tsx`, lines 304-318

```typescript
const imagingData = {
  type: 'FeatureCollection',
  features: features
};

// Transform data using d3-celestial's coordinate system
Celestial.display(imagingData);
```

**Process:**
1. Wraps features in GeoJSON FeatureCollection
2. Passes to d3-celestial for coordinate transformation
3. d3-celestial projects celestial coordinates onto 2D canvas
4. Triggers rendering callbacks for markers

---

## Marker Types

### Frame Set Markers

**Properties:**
- `location_type: 'frameset'`
- `frame_set_id: number` (non-null)
- Represents organized imaging sessions
- Aggregated from multiple frames in sessions

**Source Query:**
```sql
SELECT
    fs.id as frame_set_id,
    fs.name as object_name,
    AVG(fr.ra) as avg_ra,
    AVG(fr.dec) as avg_dec,
    -- ... aggregates ...
    'frameset' as location_type
FROM frames_set fs
JOIN imaging_nights ino ON ino.frames_set_id = fs.id
JOIN sessions s ON s.imaging_night_id = ino.id
JOIN session_members sm ON sm.session_id = s.id
JOIN frames fr ON fr.id = sm.frame_id
WHERE fr.imagetyp = 'Light'
  AND fr.ra IS NOT NULL
  AND fr.dec IS NOT NULL
GROUP BY fs.id
```

**Tooltip Example:**
```
[Frame Set] M31 Andromeda
120 frames | 10.5 hours
Filters: Ha, OIII, SII
2024-09-15 to 2024-10-20
```

### Cluster Markers

**Properties:**
- `location_type: 'cluster'`
- `frame_set_id: null`
- Represents unorganized frames
- Grouped by object name and approximate coordinates

**Source Query:**
```sql
SELECT
    NULL as frame_set_id,
    COALESCE(fr.object, 'Unknown') as object_name,
    AVG(fr.ra) as avg_ra,
    AVG(fr.dec) as avg_dec,
    -- ... aggregates ...
    'cluster' as location_type
FROM frames fr
WHERE fr.imagetyp = 'Light'
  AND fr.ra IS NOT NULL
  AND fr.dec IS NOT NULL
  AND NOT EXISTS (
      SELECT 1 FROM session_members sm WHERE sm.frame_id = fr.id
  )
GROUP BY COALESCE(fr.object, 'Unknown'), ROUND(fr.ra, 1), ROUND(fr.dec, 1)
```

**Tooltip Example:**
```
[Unorganized] NGC 7000
45 frames | 3.75 hours
Filters: Ha, OIII
2024-08-10 to 2024-08-25
```

**Key Difference:**
- Clusters group frames NOT in any session (NOT EXISTS clause)
- Grouped by rounded coordinates (ROUND to 0.1 degree = ~6 arcminutes)
- Multiple cluster markers may exist for same object at different positions

---

## Coordinate System

### RA to GeoJSON Longitude Conversion

**Location:** `SkyAtlas.tsx`, lines 231-233

```typescript
const raToGeoJsonLongitude = (ra: number) => {
  return ra;  // Option A: Direct use (0-360°)
};
```

**Coordinate Convention:**
- RA is stored in database as decimal degrees [0, 360)
- GeoJSON longitude uses same range [0, 360)
- No conversion needed (Option A from coordinate migration guide)
- DEC remains as-is (latitude in [-90, 90])

**Historical Note:**
The code includes commented-out Option B (subtract 180°) which was considered but not used. Direct mapping (Option A) was chosen for simplicity.

### Coordinate Validation

Valid coordinates must satisfy:
- RA: 0 ≤ ra < 360
- DEC: -90 ≤ dec ≤ 90
- Both non-null, not NaN, finite

Invalid coordinates are filtered out before rendering.

---

## Rendering Pipeline

The rendering system has two modes based on zoom level:

### Mode 1: Zoomed Out (Lines 454-521)

**Trigger:** Default view, showing full sky or large regions

**Rendering:**
- Simple cross markers (8px size)
- No FOV rectangles displayed
- Green color for all normal markers
- Red color for test markers (now removed)

**Implementation:**
```typescript
// Add cross markers for each location
markers.append('path')
  .attr('d', d3.symbol().type(d3.symbolCross).size(64))  // 8px cross
  .attr('class', 'imaging-marker')
  .style('fill', d =>
    d.properties.location_type === 'test' ? '#ef4444' : '#22c55e'
  )
  .style('stroke', d =>
    d.properties.location_type === 'test' ? '#dc2626' : '#16a34a'
  )
  .style('stroke-width', d =>
    d.properties.location_type === 'test' ? 3 : 2
  );
```

### Mode 2: Zoomed In (Lines 370-453)

**Trigger:** User zooms into specific region

**Rendering Logic:**
1. **If FOV data exists** (fov_width && fov_height):
   - Draw FOV rectangle aligned with celestial coordinates
   - Semi-transparent green fill (15% opacity)
   - Green stroke outline
   - Center cross marker

2. **If FOV data missing**:
   - Draw larger cross marker (10px)
   - Green color
   - No rectangle

**FOV Rectangle Calculation:**
```typescript
const width = d.properties.fov_width;
const height = d.properties.fov_height;
const originalRA = d.properties.original_ra;

// Calculate corners (RA wraps at 0/360)
const raMin = originalRA - width / 2;
const raMax = originalRA + width / 2;
const decMin = d.coordinates[1] - height / 2;
const decMax = d.coordinates[1] + height / 2;

// Create rectangle path
const rect = createFOVRectangle(raMin, raMax, decMin, decMax);
```

**RA Wrapping Handling:**
If FOV rectangle crosses RA = 0° / 360° boundary:
- Rectangle is split into two segments
- Left segment: [raMin, 360]
- Right segment: [0, raMax - 360]

---

## Styling Specifications

### Color Palette

| Element | Color | Hex | Usage |
|---------|-------|-----|-------|
| Normal Marker Fill | Green | `#22c55e` | Frame sets and clusters |
| Normal Marker Stroke | Dark Green | `#16a34a` | Marker outline |
| FOV Rectangle Fill | Green (15% opacity) | `rgba(34, 197, 94, 0.15)` | Field of view area |
| FOV Rectangle Stroke | Green | `#22c55e` | FOV outline |
| Test Marker Fill (removed) | Red | `#ef4444` | Test markers |
| Test Marker Stroke (removed) | Dark Red | `#dc2626` | Test outline |

### Size Specifications

| Element | Size | Context |
|---------|------|---------|
| Cross Marker (Zoomed Out) | 8px (64 sq px) | Full sky view |
| Cross Marker (Zoomed In) | 10px (100 sq px) | Regional view |
| Stroke Width (Normal) | 2px | Standard markers |
| Stroke Width (Test) | 3px | Test markers (removed) |
| FOV Rectangle Stroke | 1.5px | Field of view outline |

### Marker Symbols

All markers use D3 cross symbol (`d3.symbolCross`):
```
    |
----+----
    |
```

**Rationale:** Cross shape is visible at small sizes and doesn't obscure the map.

---

## FOV Rectangle Display

### When FOV is Displayed

FOV rectangles appear when ALL conditions are met:
1. User has zoomed in (Mode 2 rendering)
2. Location has `fov_width` property (non-null)
3. Location has `fov_height` property (non-null)
4. FOV data was successfully calculated by backend

### FOV Calculation (Backend)

**Location:** `commands.rs`, lines 1547-1560

**Required Metadata:**
- XPIXSZ: Pixel size in micrometers
- FOCALLEN: Focal length in millimeters
- NAXIS1: Sensor width in pixels
- NAXIS2: Sensor height in pixels
- XBINNING: Horizontal binning factor
- YBINNING: Vertical binning factor

**Formula:**
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
    let fov_degrees = fov_radians.to_degrees();
    fov_degrees
}

let fov_width = calculate_fov(xpixsz, focallen, naxis1, xbinning);
let fov_height = calculate_fov(xpixsz, focallen, naxis2, ybinning);
```

**Example:**
- Sensor: 4656 × 3520 pixels (ZWO ASI533MC Pro)
- Pixel Size: 3.76 μm
- Focal Length: 600mm
- Binning: 1x1

FOV Width: 2.0 * atan((3.76/1000 * 4656 * 1) / (2 * 600)) = **1.487° ≈ 89 arcmin**

### FOV Orientation

FOV rectangles are **aligned with celestial coordinates**:
- Width dimension aligned with RA (horizontal on equatorial grid)
- Height dimension aligned with DEC (vertical on equatorial grid)
- Rotation due to field rotation is NOT accounted for
- Near celestial poles, rectangles may appear distorted due to projection

---

## Tooltip System

### Tooltip Content

**Location:** `SkyAtlas.tsx`, lines 446-452 (zoomed in) and 510-515 (zoomed out)

**Template:**
```
[Type] Object Name
X frames | Y.Y hours
Filters: filter1, filter2, ...
YYYY-MM-DD to YYYY-MM-DD
```

**Type Labels:**
- `[Frame Set]` for location_type === 'frameset'
- `[Unorganized]` for location_type === 'cluster'

**Code:**
```typescript
const typeLabel = d.properties.location_type === 'frameset'
  ? '[Frame Set]'
  : '[Unorganized]';

return `${typeLabel} ${d.properties.name}\n` +
       `${d.properties.frame_count} frames | ` +
       `${(d.properties.total_exposure / 3600).toFixed(2)} hours\n` +
       `Filters: ${d.properties.filters}\n` +
       `${d.properties.date_range[0]} to ${d.properties.date_range[1]}`;
```

### Tooltip Positioning

Tooltips are automatically positioned by d3-celestial to avoid map edges.

### Click Behavior

**Location:** `SkyAtlas.tsx`, lines 440-444 (zoomed in) and 503-507 (zoomed out)

Clicking a marker:
1. Checks if `frame_set_id` is non-null
2. If yes, navigates to frame set detail page: `/objects/${frame_set_id}`
3. If no (cluster marker), click has no effect

**Code:**
```typescript
.on('click', (event, d) => {
  if (d.properties.frame_set_id) {
    navigate(`/objects/${d.properties.frame_set_id}`);
  }
});
```

**Cursor Styling:**
- Frame set markers: `cursor: pointer`
- Cluster markers: `cursor: default`

---

## Performance Considerations

### Marker Count

The system is optimized for rendering up to ~1000 markers efficiently.

**Current Approach:**
- All markers rendered as SVG paths
- D3 handles DOM updates efficiently
- No virtualization or clustering at map level

**Future Optimization (if needed):**
- Implement marker clustering for dense regions
- Canvas rendering for very large datasets
- Progressive loading based on zoom level

### Data Caching

**Location data is cached in React state:**
- Fetched once on component mount
- Not refetched on zoom/pan
- Refetch only when component remounts

**Future Enhancement:**
- Add manual refresh button
- Real-time updates via WebSocket (if applicable)

---

## Debugging

### Console Logging

The system logs several checkpoints:

**Line 272:**
```typescript
console.log('Adding', validLocs.length, 'imaging location markers');
```

**Line 311:**
```typescript
console.log('Pre-transformed data features:', features.length);
```

**Line 517:**
```typescript
console.log('Created', markers.size(), 'marker elements');
```

**Backend (commands.rs, Line 1593):**
```rust
println!("Found {} imaging locations ({} framesets, {} clusters)",
    result.len(),
    result.iter().filter(|l| l.location_type == "frameset").count(),
    result.iter().filter(|l| l.location_type == "cluster").count()
);
```

### Common Issues

**No markers displayed:**
1. Check console: "No valid imaging locations to display"
2. Verify backend returned data: Check terminal output
3. Inspect coordinates: All RA/DEC null or invalid?

**Markers in wrong location:**
1. Verify RA is in degrees [0, 360), not hours [0, 24)
2. Check coordinate normalization in parser

**FOV rectangles not showing:**
1. Zoom level insufficient (need Mode 2)
2. Missing sensor metadata (XPIXSZ, FOCALLEN, NAXIS1/2)
3. FOV calculation returned null

**Frame sets missing:**
See [TROUBLESHOOTING_FRAMESETS.md](./TROUBLESHOOTING_FRAMESETS.md) for comprehensive diagnostic guide.

---

## Related Documentation

- [SQL_QUERIES.md](./SQL_QUERIES.md) - Complete SQL query breakdown
- [TROUBLESHOOTING_FRAMESETS.md](./TROUBLESHOOTING_FRAMESETS.md) - Frame set diagnostic guide
- [DATA_MODEL.md](./DATA_MODEL.md) - Data structure reference
- `../db/COORDINATE_ISSUES.md` - Coordinate handling fixes

---

## Version History

**v1.0 (2025-11-13):**
- Initial documentation
- Removed test markers (M42, M31, M45)
- Documented complete rendering pipeline
