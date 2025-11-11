# Guide to Fix FOV Marker Coordinate Positioning

## Current Status (November 10, 2024)

### ✅ Working
- SVG overlay successfully created on top of d3-celestial canvas
- 8 imaging location markers rendering as green crosshairs
- Markers follow map on pan/zoom (redraw function operational)
- Click handlers and tooltips implemented
- Zoom-based rendering mode switching (crosses vs FOV rectangles)

### ❌ Issue
- Markers display at incorrect sky coordinates
- Crosshairs don't align with actual imaging target positions

## Problem Analysis

The coordinate mismatch is likely due to one or more of these issues:

### 1. Coordinate System Mismatch
d3-celestial expects coordinates in a specific format. Currently:
- **Input**: RA in degrees (0-360°), Dec in degrees (-90 to 90°)
- **Conversion applied**: RA > 180° → negative (RA - 360°)
- **Issue**: d3-celestial might expect a different coordinate frame or transformation

### 2. Transform Setting
Currently using `transform: "equatorial"` in d3-celestial config, but the coordinate conversion might not align with how d3-celestial interprets equatorial coordinates.

### 3. Longitude vs RA Convention
Astronomical RA increases eastward (0-360°), but map longitude conventions vary. The `raToGeoJsonLongitude()` conversion function may need adjustment.

## Architecture Overview

### Current Implementation

**File**: `/src/pages/SkyAtlas.tsx`

**Key Components**:
1. **Canvas**: d3-celestial renders the sky map on `<canvas>` (not SVG)
2. **SVG Overlay**: Absolutely positioned SVG layer on top of canvas for markers
3. **Markers Group**: `<g class="imaging-markers-layer">` contains all marker paths
4. **Coordinate Flow**:
   ```
   Database (RA: 0-360°, Dec: -90 to 90°)
   ↓
   raToGeoJsonLongitude() conversion (line 233-237)
   ↓
   GeoJSON Feature creation (line 254-275)
   ↓
   window.Celestial.getData() transformation (line 292)
   ↓
   window.Celestial.map.projection() to pixel coords (line 435)
   ↓
   SVG marker positioning
   ```

## Debugging Steps

### Step 1: Verify Expected Coordinate Format

**Location**: `SkyAtlas.tsx` around line 432

Add detailed logging to compare input coordinates vs actual projected positions:

```typescript
// In the marker creation section (around line 432):
.attr('transform', function(d: any, i: number) {
  const coords = d.geometry.coordinates;
  const pt = window.Celestial.map.projection()(coords);

  // DEBUG: Log coordinate details for first marker
  if (i === 0) {
    console.log('=== COORDINATE DEBUG ===');
    console.log('Original RA (0-360°):', d.properties.original_ra);
    console.log('Original Dec:', coords[1]);
    console.log('Converted coords [lon, lat]:', coords);
    console.log('Projected pixel position [x, y]:', pt);

    // Test: Try projecting the original RA without conversion
    const testPt = window.Celestial.map.projection()([d.properties.original_ra, coords[1]]);
    console.log('Test projection (no RA conversion):', testPt);

    // Test: Try with different conversions
    const test2Pt = window.Celestial.map.projection()([360 - d.properties.original_ra, coords[1]]);
    console.log('Test projection (360 - RA):', test2Pt);
  }

  return pt ? `translate(${pt[0]},${pt[1]})` : 'translate(0,0)';
})
```

**What to check**:
- Pan the map to known constellations
- Compare where markers appear vs where they should be
- Note if the offset is consistent or varies with position

### Step 2: Test Different Coordinate Formats

#### Option A: No RA Conversion (Use RA 0-360° directly)

**Location**: `SkyAtlas.tsx` line 233-237

```typescript
// BEFORE:
const raToGeoJsonLongitude = (ra: number): number => {
  return ra > 180 ? ra - 360 : ra;
};

// AFTER (Option A):
const raToGeoJsonLongitude = (ra: number): number => {
  return ra;  // Use RA directly without conversion
};
```

**Rationale**: d3-celestial might natively understand RA in 0-360° range.

#### Option B: Reverse Direction

```typescript
// AFTER (Option B):
const raToGeoJsonLongitude = (ra: number): number => {
  return 360 - ra;  // Mirror the RA coordinate
};
```

**Rationale**: Celestial sphere projection might be mirrored vs standard maps.

#### Option C: Offset by 180°

```typescript
// AFTER (Option C):
const raToGeoJsonLongitude = (ra: number): number => {
  return (ra + 180) % 360;  // Shift by 180°
};
```

**Rationale**: Some projections center at different meridians.

#### Option D: Convert to Radians

```typescript
// AFTER (Option D):
const raToGeoJsonLongitude = (ra: number): number => {
  return (ra * Math.PI) / 180;  // Convert degrees to radians
};
```

**Rationale**: Some projection functions expect radians instead of degrees.

### Step 3: Check Transform Configuration

**Location**: `SkyAtlas.tsx` line 85-159 (Celestial.display config)

Current setting:
```typescript
projection: 'aitoff',
transform: 'equatorial',  // <-- This determines coordinate interpretation
```

**Test different transforms**:

```typescript
// Option 1: Keep equatorial (current)
transform: 'equatorial'  // RA/Dec

// Option 2: Try ecliptic
transform: 'ecliptic'    // Ecliptic longitude/latitude

// Option 3: Try galactic
transform: 'galactic'    // Galactic coordinates
```

**Note**: If you change the transform, you may also need to adjust how coordinates are passed.

### Step 4: Bypass getData() Transformation

**Location**: `SkyAtlas.tsx` line 292

Currently, we're transforming the GeoJSON through Celestial's getData():

```typescript
// BEFORE:
const data = window.Celestial.getData(imagingData, window.Celestial.settings().transform);

// AFTER (test):
const data = imagingData;  // Skip Celestial's transformation
```

**Rationale**: `getData()` might be applying unwanted transformations. Using raw GeoJSON data and relying only on the projection function might work better.

### Step 5: Manual Projection Calculation

If the built-in projection doesn't work, implement manual Aitoff projection:

**Location**: `SkyAtlas.tsx` around line 433

```typescript
.attr('transform', function(d: any) {
  const ra = d.properties.original_ra;
  const dec = d.geometry.coordinates[1];

  // Convert to radians and center at RA=180°
  const lambda = ((ra - 180) * Math.PI) / 180;  // longitude from -π to π
  const phi = (dec * Math.PI) / 180;             // latitude from -π/2 to π/2

  // Aitoff projection formulas
  const alpha = Math.acos(Math.cos(phi) * Math.cos(lambda / 2));
  const sinc_alpha = (Math.abs(alpha) < 1e-10) ? 1 : Math.sin(alpha) / alpha;

  const x = 2 * Math.cos(phi) * Math.sin(lambda / 2) / sinc_alpha;
  const y = Math.sin(phi) / sinc_alpha;

  // Get canvas dimensions and scale
  const canvas = document.querySelector('#celestial-map canvas') as HTMLCanvasElement;
  const scale = window.Celestial.scale();
  const cx = canvas.width / 2;
  const cy = canvas.height / 2;

  // Convert to pixel coordinates
  const px = cx + x * scale;
  const py = cy - y * scale;  // Invert Y axis

  return `translate(${px},${py})`;
})
```

**Note**: This bypasses d3-celestial's projection entirely. Use as last resort if built-in methods fail.

## Testing with Known Coordinates

Add test markers at well-known celestial objects to verify positioning:

**Location**: `SkyAtlas.tsx` around line 254 (before creating GeoJSON features)

```typescript
// Add test markers for known objects
const testMarkers = [
  {
    type: 'Feature',
    id: 'test-m42',
    properties: {
      name: 'TEST: M42 Orion Nebula',
      object_name: 'M42',
      frame_count: 0,
      total_exposure: 0,
      filters: 'TEST',
      date_range: '',
      frame_set_id: null,
      location_type: 'test',
      fov_width: null,
      fov_height: null,
      original_ra: 83.82  // M42 RA
    },
    geometry: {
      type: 'Point',
      coordinates: [raToGeoJsonLongitude(83.82), -5.39]  // M42: RA=5h35m, Dec=-5°23'
    }
  },
  {
    type: 'Feature',
    id: 'test-m31',
    properties: {
      name: 'TEST: M31 Andromeda',
      object_name: 'M31',
      frame_count: 0,
      total_exposure: 0,
      filters: 'TEST',
      date_range: '',
      frame_set_id: null,
      location_type: 'test',
      fov_width: null,
      fov_height: null,
      original_ra: 10.68  // M31 RA
    },
    geometry: {
      type: 'Point',
      coordinates: [raToGeoJsonLongitude(10.68), 41.27]  // M31: RA=0h43m, Dec=+41°16'
    }
  },
  {
    type: 'Feature',
    id: 'test-m45',
    properties: {
      name: 'TEST: M45 Pleiades',
      object_name: 'M45',
      frame_count: 0,
      total_exposure: 0,
      filters: 'TEST',
      date_range: '',
      frame_set_id: null,
      location_type: 'test',
      fov_width: null,
      fov_height: null,
      original_ra: 56.75  // M45 RA
    },
    geometry: {
      type: 'Point',
      coordinates: [raToGeoJsonLongitude(56.75), 24.12]  // M45: RA=3h47m, Dec=+24°7'
    }
  }
];

// Merge test markers with actual data
const features = [
  ...validLocs.map(loc => ({
    type: 'Feature',
    id: loc.id,
    properties: { ... },
    geometry: { ... }
  })),
  ...testMarkers  // Add test markers
];
```

### Test Marker Reference Coordinates

| Object | RA (degrees) | RA (h:m:s) | Dec (degrees) | Dec (d:m:s) | Constellation | Visibility |
|--------|--------------|------------|---------------|-------------|---------------|------------|
| **M42** (Orion Nebula) | 83.82° | 5h 35m 17s | -5.39° | -5° 23' 28" | Orion | Winter |
| **M31** (Andromeda Galaxy) | 10.68° | 0h 42m 44s | +41.27° | +41° 16' 9" | Andromeda | Fall |
| **M45** (Pleiades) | 56.75° | 3h 47m 0s | +24.12° | +24° 7' 0" | Taurus | Winter |

**How to test**:
1. Add test markers code
2. Navigate to each constellation on the sky map
3. Verify if markers appear at correct positions relative to constellation patterns
4. If markers are consistently offset, measure the offset direction and magnitude

## Quick Testing Procedure

1. **Add debug logging** (Step 1)
2. **Try Option A** - No RA conversion first (most likely fix)
3. **Add test marker at M42** (easily visible in Orion)
4. **Navigate to Orion constellation** on sky map
5. **Check if marker appears near the three belt stars** (M42 is just below)
6. **If wrong, try Option B, C, or D** systematically
7. **Once correct, test with M31 and M45** to verify consistency
8. **Remove all debug code and test markers**

## Files to Modify

### Primary File
**Path**: `/src/pages/SkyAtlas.tsx`

**Key sections**:
- **Line 233-237**: `raToGeoJsonLongitude()` - Coordinate conversion function
- **Line 254-275**: GeoJSON feature creation - Where coordinates are packaged
- **Line 292**: `window.Celestial.getData()` call - Data transformation
- **Line 433-437**: Marker transform attribute - Final pixel positioning

## Expected Behavior After Fix

1. ✅ Crosshairs appear at correct RA/Dec positions
2. ✅ Test markers (M42, M31, M45) align with constellation positions
3. ✅ Markers follow map correctly on pan/zoom
4. ✅ Click handlers work to navigate to frame sets
5. ✅ Tooltips show correct object information
6. ✅ Zoom-in mode shows FOV rectangles at correct positions

## Common Pitfalls

### 1. Mixing Coordinate Systems
- Don't mix equatorial RA with galactic longitude
- Ensure Dec is always in degrees, not radians
- Watch for coordinate wrapping at 0°/360° boundary

### 2. Projection Issues
- Aitoff projection has edge distortion near ±180° longitude
- Points exactly on boundaries may not project correctly
- Some projections fail for coordinates outside valid ranges

### 3. Canvas vs SVG Coordinate Systems
- Canvas Y-axis often needs inversion (screen coords go down, sky coords go up)
- SVG overlays must perfectly align with canvas dimensions
- Zoom/pan transforms can affect coordinate calculations

## Debugging Tools

### Browser Console Commands

```javascript
// Check current projection function
window.Celestial.map.projection()

// Test coordinate projection
window.Celestial.map.projection()([83.82, -5.39])

// Check current transform setting
window.Celestial.settings().transform

// Check current zoom scale
window.Celestial.scale()

// Test clipping (visibility) of coordinates
window.Celestial.clip([83.82, -5.39])
```

### Inspect SVG Markers

```javascript
// Find all markers in DOM
document.querySelectorAll('.imaging-marker')

// Check marker positions
document.querySelectorAll('.imaging-marker').forEach((m, i) => {
  console.log(`Marker ${i}:`, m.getAttribute('transform'));
});

// Count visible markers
document.querySelectorAll('.imaging-marker[style*="display: none"]').length
```

## Additional Resources

- **d3-celestial documentation**: Check `window.Celestial.projection()` API
- **Aitoff projection**: Wikipedia article on map projections
- **RA/Dec conversions**: SIMBAD astronomical database for verifying coordinates
- **Constellation maps**: Use to visually verify marker positions

## Success Criteria

- [ ] Test markers appear at M42, M31, M45 positions
- [ ] Real imaging location markers align with expected sky positions
- [ ] Markers remain stable during pan/zoom operations
- [ ] All 8 imaging locations visible (not hidden by clip boundaries)
- [ ] Click navigation to frame sets works
- [ ] Tooltips display correct information
- [ ] Zoom-in mode (>2.0x) shows FOV rectangles correctly
- [ ] No console errors related to coordinate projection

## Next Steps After Fix

1. Remove all debug logging
2. Remove test markers
3. Test with real imaging data at various sky positions
4. Implement FOV rectangle rotation (if needed for non-north-up images)
5. Add color coding by filter or exposure time
6. Optimize marker rendering performance for large datasets

## Notes

- Current data shows 8 imaging locations across 3 regions:
  - 4 near RA~232° Dec~19° (likely Corona Borealis region)
  - 3 near RA~23° Dec~30° (likely Perseus/Taurus region)
  - 1 near RA~84° Dec~-5° (likely Orion region)

- The conversion `raToGeoJsonLongitude()` was added to match GeoJSON longitude conventions (-180 to +180), but this may not be what d3-celestial expects for equatorial coordinates.

- d3-celestial uses Aitoff projection by default, which wraps longitude at ±180°. This is likely where the mismatch occurs.

## Contact & Updates

Last updated: November 10, 2024
Status: Markers rendering, coordinates incorrect
Next session: Debug coordinate conversion and test with known objects
