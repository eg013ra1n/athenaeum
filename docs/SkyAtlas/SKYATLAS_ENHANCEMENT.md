# SkyAtlas Interactive Region Selection - Implementation Guide

## Overview

This document describes the enhancement of Athenaeum's SkyAtlas component to support interactive region selection on the sky map. Users can now:
- Draw circles, rectangles, and polygons to select sky regions
- Query frames within selected regions
- Create custom frame sets from selections
- Visualize field-of-view (FOV) coverage
- Multi-select imaging locations

## Architecture

### Component Stack

```
┌─────────────────────────────────────────────────────────────┐
│  React Page: Observatory / SkyAtlas                         │
├─────────────────────────────────────────────────────────────┤
│  SelectionToolbar (UI Controls)                             │
│  ├─ Circle, Rectangle, Polygon buttons                      │
│  ├─ Mode indicator                                          │
│  └─ Selected frame counter                                  │
│                                                              │
│  SVG Drawing Layer (Interactive)                            │
│  ├─ MouseDown/MouseMove/MouseUp handlers                    │
│  ├─ Real-time shape visualization                           │
│  ├─ Pointer-events: all (captures interaction)             │
│  └─ Coordinate transformation logic                         │
│                                                              │
│  d3-celestial Canvas (Visualization)                        │
│  ├─ Star catalog (magnitude 6+)                             │
│  ├─ Constellations (lines, boundaries, names)               │
│  ├─ Milky Way band                                          │
│  ├─ DSO markers                                             │
│  ├─ Imaging location markers (squares)                      │
│  └─ FOV overlay (when enabled)                              │
│      pointer-events: none (when in selection mode)          │
└─────────────────────────────────────────────────────────────┘
         ↓ (Mouse Events & Queries)
┌─────────────────────────────────────────────────────────────┐
│  Tauri Backend (Rust)                                       │
├─────────────────────────────────────────────────────────────┤
│  Spatial Query Commands:                                     │
│  ├─ query_frames_in_circle()                                │
│  ├─ query_frames_in_bounds()                                │
│  ├─ query_frames_in_polygon()                               │
│  └─ create_frame_set_from_selection()                       │
│                                                              │
│  Database:                                                   │
│  ├─ frames (id, ra, dec, ...)                               │
│  ├─ frames_set (id, name, is_custom, ...)                   │
│  └─ frames_set_members (frames_set_id, frame_id)            │
└─────────────────────────────────────────────────────────────┘
```

### Coordinate Systems

Two coordinate systems are used:

1. **Screen Coordinates**: Pixel positions on the canvas
   - Origin: Top-left (0, 0)
   - Unit: Pixels
   - Used for: Drawing, rendering

2. **Sky Coordinates**: Astronomical positions
   - RA: Right Ascension (0-360° or -180-180°)
   - Dec: Declination (-90° to +90°)
   - Unit: Degrees
   - Used for: Database queries, frame matching

**Transformation:**
```typescript
// Pixel → Sky (RA/Dec)
const [ra, dec] = Celestial.mapProjection.invert([x, y]);

// Sky (RA/Dec) → Pixel
const [x, y] = Celestial.mapProjection([ra, dec]);
```

### Data Flow

```
User Action (e.g., "Draw Circle")
         ↓
SelectionToolbar Button Click
         ↓
setDrawingMode('circle')
         ↓
SVG Overlay Activated
         ↓
Mouse Events Captured
  ├─ mousedown: Set center (RA/Dec)
  ├─ mousemove: Update radius
  └─ mouseup: Query backend
         ↓
query_frames_in_circle(ra, dec, radius)
         ↓
Backend Query
  ├─ Get all frames with coordinates
  ├─ Filter by angular distance
  └─ Return frame IDs
         ↓
Frontend Dialog
  ├─ Show selected frame count
  ├─ Show total exposure time
  └─ User confirms "Create Frame Set"
         ↓
create_frame_set_from_selection(name, frame_ids)
         ↓
New Frame Set Created
  ├─ Stored in frames_set table
  ├─ Members added to frames_set_members
  └─ User navigated to new set
```

## Implementation Phases

### Phase 1: Backend Models & Commands

**Files:**
- `src-tauri/src/models.rs`
- `src-tauri/src/commands.rs`
- `src-tauri/src/lib.rs`

**New Types:**

```rust
// Selection region definitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionBounds {
    pub ra_min: f64,
    pub ra_max: f64,
    pub dec_min: f64,
    pub dec_max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionCircle {
    pub ra: f64,
    pub dec: f64,
    pub radius_degrees: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionPolygon {
    pub vertices: Vec<(f64, f64)>,  // [(ra, dec), ...]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionResult {
    pub frame_ids: Vec<i64>,
    pub count: usize,
    pub total_exposure_seconds: f64,
}
```

**New Commands:**

```rust
#[tauri::command]
pub async fn query_frames_in_circle(
    state: State<'_, AppState>,
    ra: f64,
    dec: f64,
    radius_degrees: f64,
) -> Result<SelectionResult, String>

#[tauri::command]
pub async fn query_frames_in_bounds(
    state: State<'_, AppState>,
    bounds: SelectionBounds,
) -> Result<SelectionResult, String>

#[tauri::command]
pub async fn query_frames_in_polygon(
    state: State<'_, AppState>,
    vertices: Vec<(f64, f64)>,
) -> Result<SelectionResult, String>

#[tauri::command]
pub async fn create_frame_set_from_selection(
    state: State<'_, AppState>,
    name: String,
    frame_ids: Vec<i64>,
    project_id: Option<i64>,
) -> Result<i64, String>
```

**Helper Functions (in new module `src-tauri/src/selection/` ):**

```rust
pub fn angular_distance(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64
pub fn point_in_polygon(ra: f64, dec: f64, vertices: &[(f64, f64)]) -> bool
```

### Phase 2: Frontend Types & Utilities

**Files:**
- `src/types/selection.ts` (new)
- `src/hooks/useMapSelection.ts` (new)

**Types:**

```typescript
export interface SelectionBounds {
  raMin: number;
  raMax: number;
  decMin: number;
  decMax: number;
}

export interface SelectionCircle {
  ra: number;
  dec: number;
  radiusDegrees: number;
}

export interface SelectionPolygon {
  vertices: Array<[number, number]>;  // [ra, dec]
}

export interface SelectionResult {
  frameIds: number[];
  count: number;
  totalExposureSeconds: number;
}

export type DrawingMode = 'none' | 'circle' | 'rectangle' | 'polygon';

export interface SelectionState {
  mode: DrawingMode;
  selectedFrames: Set<number>;
  persistentSelection: SelectionData | null;
}

export interface SelectionData {
  type: 'circle' | 'rectangle' | 'polygon';
  center?: [number, number];      // For circle
  radius?: number;                 // For circle
  bounds?: SelectionBounds;        // For rectangle
  vertices?: Array<[number, number]>;  // For polygon
  frameIds: number[];
  frameCount: number;
  totalExposure: number;
}
```

**Utilities:**

```typescript
export function angularDistance(
  coord1: [number, number],
  coord2: [number, number]
): number

export function convertPixelToSky(x: number, y: number): [number, number]

export function convertSkyToPixel(ra: number, dec: number): [number, number]

export function isPointInPolygon(
  ra: number,
  dec: number,
  vertices: Array<[number, number]>
): boolean
```

### Phase 3: SVG Overlay & Core Drawing Infrastructure

**Files:**
- `src/pages/SkyAtlas.tsx` (modified)

**Add to Component:**

```typescript
// State
const [drawingMode, setDrawingMode] = useState<DrawingMode>('none');
const [selectedFrames, setSelectedFrames] = useState<Set<number>>(new Set());
const svgRef = useRef<SVGSVGElement | null>(null);

// Effect: Create SVG overlay on mount
useEffect(() => {
  const container = document.querySelector('#celestial-map');
  if (!container) return;

  const svg = d3.select(container)
    .append('svg')
    .attr('class', 'selection-overlay')
    .style('position', 'absolute')
    .style('top', '0')
    .style('left', '0')
    .style('width', '100%')
    .style('height', '100%')
    .style('z-index', '10');

  svgRef.current = svg.node() as SVGSVGElement;

  return () => {
    svg.remove();
  };
}, []);

// Effect: Update pointer events based on mode
useEffect(() => {
  if (!svgRef.current) return;
  svgRef.current.style.pointerEvents =
    drawingMode !== 'none' ? 'all' : 'none';
}, [drawingMode]);
```

### Phase 4: Selection Tool Implementations

Each tool follows the pattern:

```typescript
useEffect(() => {
  if (drawingMode !== 'circle') return;

  const svg = d3.select('.selection-overlay');

  // Initialize state
  let state = { /* tool-specific state */ };

  // Mouse event handlers
  svg.on('mousedown', (event) => { /* ... */ });
  svg.on('mousemove', (event) => { /* ... */ });
  svg.on('mouseup', (event) => { /* ... */ });

  // Cleanup
  return () => {
    svg.on('mousedown', null);
    svg.on('mousemove', null);
    svg.on('mouseup', null);
  };
}, [drawingMode]);
```

**See implementation details in main code sections above.**

### Phase 5: Components

**SelectionToolbar Component:**
- Located: `src/components/SelectionToolbar.tsx`
- Props: `onModeChange`, `currentMode`, `selectedCount`
- Buttons: Circle, Rectangle, Polygon, Cancel
- Keyboard shortcuts: Esc to cancel

**SelectionDialog Component:**
- Located: `src/components/SelectionDialog.tsx`
- Shows: Frame count, total exposure, first 10 frames preview
- Input: Frame set name
- Actions: Create, Cancel

## Database Schema

### New Query Examples

**Circle Query:**
```sql
-- Get all LIGHT frames with coordinates
SELECT id, ra, dec FROM frames
WHERE ra IS NOT NULL
  AND dec IS NOT NULL
  AND imagetyp = 'Light'
-- Then filter in application using angular distance
```

**Rectangle Query:**
```sql
SELECT id FROM frames
WHERE ra IS NOT NULL
  AND dec IS NOT NULL
  AND imagetyp = 'Light'
  AND ra BETWEEN ?1 AND ?2
  AND dec BETWEEN ?3 AND ?4
```

**Create Frame Set:**
```sql
-- Insert frame set
INSERT INTO frames_set (name, is_custom, project_id, created_at)
VALUES (?, 1, ?, datetime('now'))

-- Insert members
INSERT INTO frames_set_members (frames_set_id, frame_id)
VALUES (?, ?) -- For each frame_id
```

## Testing Checklist

- [ ] Circle selection tool draws correctly
- [ ] Rectangle selection tool handles negative dimensions
- [ ] Polygon selection requires minimum 3 vertices
- [ ] Backend queries return correct frames
- [ ] Selections persist across zoom/pan
- [ ] FOV visualization updates correctly
- [ ] Frame set creation stores data correctly
- [ ] Large selections (>1000 frames) perform well
- [ ] RA wrap-around handled (0/360°)
- [ ] Coordinate transformations accurate
- [ ] UI responsive and intuitive
- [ ] Error messages clear and helpful

## Performance Considerations

### Optimization Strategies

1. **Lazy Loading**: Load frame coordinates only when needed
2. **Spatial Indexing**: Consider adding RA/Dec indexes in database
3. **Batch Operations**: Group frame set member insertions
4. **Debouncing**: Debounce mousemove events during drawing
5. **Caching**: Cache angular distance calculations

### Expected Performance

- Circle query: <50ms for 10,000 frames
- Rectangle query: <20ms (simple bounds check)
- Polygon query: <100ms (point-in-polygon is O(n))
- Frame set creation: <500ms for 1,000 frames

## Future Enhancements

1. **Coverage Heat Map**: Overlay density visualization
2. **Gap Analysis**: Suggest unimaged regions
3. **MOC Export**: Export selections as MOC files
4. **Undo/Redo**: Drawing operation history
5. **Keyboard Shortcuts**: Activate tools via keyboard
6. **Touch Support**: Draw with touch gestures
7. **Region Library**: Save and reuse regions
8. **Multi-Session View**: Compare coverage across sessions

## Resources

### Key Functions/Classes

- `Celestial.mapProjection()` - D3 geo projection
- `Celestial.mapProjection.invert()` - Inverse projection
- `d3.drag()` - Drag behavior
- `d3.mouse()` - Get mouse coordinates
- `d3.select()` - SVG element selection

### Astronomical Formulas

- Haversine Formula: Great circle distance between two points
- Ray Casting Algorithm: Point-in-polygon test
- Angular Distance: Difference between two sky positions

### References

- [d3-celestial Documentation](https://github.com/ofrohn/d3-celestial)
- [D3.js v3 API](https://github.com/d3/d3-3.x-api-reference)
- [SVG Specification](https://www.w3.org/TR/SVG2/)
- [Astronomical Coordinate Systems](https://en.wikipedia.org/wiki/Equatorial_coordinate_system)

## File Structure Summary

```
Athenaeum/
├── src/
│   ├── components/
│   │   ├── SelectionToolbar.tsx (NEW)
│   │   └── SelectionDialog.tsx (NEW)
│   ├── pages/
│   │   └── SkyAtlas.tsx (MODIFIED)
│   ├── hooks/
│   │   └── useMapSelection.ts (NEW)
│   ├── types/
│   │   └── selection.ts (NEW)
│   └── utils/
│       └── coordinates.ts (NEW)
│
└── src-tauri/
    └── src/
        ├── commands.rs (MODIFIED)
        ├── models.rs (MODIFIED)
        ├── lib.rs (MODIFIED)
        └── selection/ (NEW)
            ├── mod.rs
            ├── queries.rs
            └── algorithms.rs
```

## Conclusion

This enhancement transforms SkyAtlas from a passive visualization tool to an interactive selection interface, enabling astrophotographers to organize their imaging data spatially and intuitively. The implementation leverages d3-celestial's existing infrastructure while adding powerful querying and frame set management capabilities.
