# Phase 2: SVG Overlay Infrastructure - Implementation Guide

**Status:** Complete ✅
**Date:** 2025-11-10
**Purpose:** Build the foundation for interactive drawing on the sky map

## Overview

Phase 2 implements the SVG overlay infrastructure that enables interactive region selection on the d3-celestial sky map. This layer sits between the user's mouse interactions and the Tauri backend, translating screen coordinates to astronomical coordinates.

## Architecture

### Layer Stack (from bottom to top)

```
User Mouse Events (pixels)
         ↓
SVG Overlay Layer ← Custom D3 handlers capture events
         ↓
Coordinate Transform ← Convert pixels ↔ RA/Dec
         ↓
Drawing Tools ← Build selection shapes (Phase 3-5)
         ↓
Backend Queries ← Execute spatial queries
         ↓
Frame Set Creation ← Create custom sets from selections
```

## Components Created

### 1. Custom Hook: `useSvgOverlay.ts`

**Purpose:** Create and manage the transparent SVG layer above d3-celestial

**Key Features:**
- Creates absolute-positioned SVG on top of canvas
- Controls pointer-events (enables/disables interaction)
- Provides clear() method for cleaning drawing state
- Handles z-index layering correctly

**API:**
```typescript
interface SVGOverlayAPI {
  getSvg(): Selection | null       // Get D3 selection of SVG
  enable(): void                   // Allow mouse interaction
  disable(): void                  // Disable mouse interaction
  clear(): void                    // Remove all drawn elements
  isEnabled(): boolean             // Check if overlay is active
}
```

**Usage:**
```typescript
const svgOverlay = useSvgOverlay({ containerId: 'celestial-map' });

// Enable interaction when drawing
svgOverlay.enable();

// Get SVG for adding elements
const svg = svgOverlay.getSvg();

// Clean up between draws
svgOverlay.clear();

// Disable when done
svgOverlay.disable();
```

### 2. Custom Hook: `useCoordinateTransform.ts`

**Purpose:** Interface with d3-celestial's projection system for coordinate conversion

**Key Features:**
- Converts screen pixels to astronomical coordinates
- Converts astronomical coordinates to screen pixels
- Checks coordinate visibility in current projection
- Handles RA wrap-around (0°/360°) normalization
- Clamps declination to ±90° range

**Coordinate Systems:**
```
Screen Coordinates (pixels)
- Origin: Top-left corner
- X: 0 to canvas width
- Y: 0 to canvas height

Sky Coordinates (decimal degrees)
- RA (Right Ascension): 0-360° or -180-180°
- Dec (Declination): -90° to +90°
```

**API:**
```typescript
interface CoordinateTransformAPI {
  pixelToSky(x: number, y: number): [number, number] | null
  skyToPixel(ra: number, dec: number): [number, number] | null
  isVisible(ra: number, dec: number): boolean
  getProjection(): any
}
```

**Usage Example - Circle Drawing:**
```typescript
const { pixelToSky } = useCoordinateTransform();

// User clicks at pixel (500, 300)
const centerSky = pixelToSky(500, 300);  // Returns [RA, Dec]

// User drags to pixel (550, 350)
const boundaryPixel = pixelToSky(550, 350);

// Calculate angular distance
const radius = angularDistance(
  centerSky[0], centerSky[1],
  boundaryPixel[0], boundaryPixel[1]
);
```

### 3. Custom Hook: `useD3MouseEvents.ts`

**Purpose:** Standardized D3-based mouse event handling for drawing operations

**Key Features:**
- Uses `d3.pointer()` for consistent coordinate handling
- Supports mousedown, mousemove, mouseup, dblclick
- Abstracts event binding/unbinding
- Provides normalized event callbacks

**API:**
```typescript
interface D3MouseEventAPI {
  attachMouseHandlers(
    selection: Selection,
    handlers: MouseEventHandlers
  ): void

  getMouseCoordinates(
    svgElement: SVGSVGElement,
    event: MouseEvent
  ): [number, number] | null

  detachMouseHandlers(selection: Selection): void
}

interface MouseEventHandlers {
  onMouseDown?: (x: number, y: number, event: MouseEvent) => void
  onMouseMove?: (x: number, y: number, event: MouseEvent) => void
  onMouseUp?: (x: number, y: number, event: MouseEvent) => void
  onDblClick?: (x: number, y: number, event: MouseEvent) => void
}
```

**Usage Example - Drag Handling:**
```typescript
const { attachMouseHandlers } = useD3MouseEvents();

const handlers = {
  onMouseDown: (x, y) => {
    console.log('Started at pixel:', x, y);
    setIsDrawing(true);
  },
  onMouseMove: (x, y) => {
    if (!isDrawing) return;
    // Update drawing as mouse moves
    updateShape(x, y);
  },
  onMouseUp: (x, y) => {
    console.log('Finished at pixel:', x, y);
    setIsDrawing(false);
  }
};

attachMouseHandlers(svg, handlers);
```

### 4. Updated: `SkyAtlas.tsx`

**Changes Made:**
- Added import of selection types and new hooks
- Added `drawingMode` state management
- Integrated `useSvgOverlay` hook
- Integrated `useCoordinateTransform` hook
- Integrated `useD3MouseEvents` hook
- Added effect to manage overlay visibility based on mode

**New State:**
```typescript
const [drawingMode, setDrawingMode] = useState<DrawingMode>('none');
```

**Effect: SVG Overlay Management**
```typescript
useEffect(() => {
  if (!mapReady) return;

  const overlay = svgOverlay.getSvg();
  if (!overlay) return;

  if (drawingMode !== 'none') {
    svgOverlay.enable();      // Enable interaction
  } else {
    svgOverlay.disable();     // Disable interaction
  }
}, [drawingMode, mapReady, svgOverlay]);
```

## Data Flow

### Initialization Sequence

```
Component Mount
    ↓
useSvgOverlay Hook
  - Check container exists
  - Create SVG overlay
  - Set z-index: 10
  - Initial pointer-events: none
    ↓
useCoordinateTransform Hook
  - Cache Celestial projection reference
  - Set up transform utilities
    ↓
useD3MouseEvents Hook
  - Initialize event handler storage
  - Ready for event attachment
    ↓
SVG Overlay Effect
  - Links drawing mode to overlay state
  - Enables/disables pointer-events
```

### Drawing Interaction Flow

```
User moves mouse
    ↓
SVG Overlay captures event (if enabled)
    ↓
d3.pointer() normalizes coordinates
    ↓
useD3MouseEvents handler called
    ↓
Drawing tool processes coordinates
    ↓
useCoordinateTransform converts pixels → RA/Dec
    ↓
Drawing updates in real-time on SVG
    ↓
On completion: Query backend
```

## Coordinate Transformation Details

### Pixel to Sky Conversion

```typescript
// User clicks at SVG pixel (x, y)
const [ra, dec] = projection.invert([x, y]);

// Normalize RA to 0-360°
let normalizedRa = ra % 360;
if (normalizedRa < 0) normalizedRa += 360;

// Clamp Dec to ±90°
const clampedDec = Math.max(-90, Math.min(90, dec));

return [normalizedRa, clampedDec];
```

### Sky to Pixel Conversion

```typescript
// Backend returns RA, Dec (e.g., [180.5, 45.2])
const [x, y] = projection([ra, dec]);

// x, y are screen coordinates
// Can be used to position visual elements
```

### Visibility Check

```typescript
// Check if coordinate is visible in current view
const isVisible = Celestial.clip([ra, dec]);

// Used to hide elements outside projection bounds
```

## Integration Points

### With d3-celestial

- **`Celestial.mapProjection`** - Used for coordinate transformation
- **`Celestial.mapProjection.invert()`** - Pixel to sky conversion
- **`Celestial.clip()`** - Visibility checking
- **No modifications** to existing d3-celestial code

### With Tauri Backend

- **`pixelToSky()`** prepares coordinates for queries
- Results feed into `query_frames_in_circle`, `query_frames_in_bounds`, etc.
- Establishes coordinate system contract between frontend and backend

### With React

- Custom hooks integrate with React lifecycle
- Drawing mode state controls UI behavior
- Coordinates transform to sky system before backend calls

## Drawing Tool Pattern (for Phases 3-5)

All drawing tools follow this pattern:

```typescript
useEffect(() => {
  if (drawingMode !== 'circle') return;

  const svg = svgOverlay.getSvg();
  if (!svg) return;

  // 1. Initialize drawing state
  let centerPixel: [number, number] | null = null;
  let centerSky: [number, number] | null = null;
  let radiusDegrees = 0;

  // 2. Attach mouse handlers
  mouseEvents.attachMouseHandlers(svg, {
    onMouseDown: (x, y) => {
      centerPixel = [x, y];
      centerSky = coordinateTransform.pixelToSky(x, y);

      // Create visual indicator
      svg.append('circle')
        .attr('class', 'selection-circle')
        .attr('cx', x)
        .attr('cy', y)
        .attr('r', 0)
        .style('fill', 'rgba(59, 130, 246, 0.15)')
        .style('stroke', '#3b82f6');
    },

    onMouseMove: (x, y) => {
      if (!centerPixel || !centerSky) return;

      // Calculate distance
      const currentSky = coordinateTransform.pixelToSky(x, y);
      const distance = angularDistance(
        centerSky[0], centerSky[1],
        currentSky[0], currentSky[1]
      );
      radiusDegrees = distance;

      // Update visual
      svg.select('.selection-circle').attr('r', Math.sqrt(
        (x - centerPixel[0]) ** 2 + (y - centerPixel[1]) ** 2
      ));
    },

    onMouseUp: async (x, y) => {
      // Query backend
      const result = await invoke('query_frames_in_circle', {
        ra: centerSky![0],
        dec: centerSky![1],
        radius_degrees: radiusDegrees
      });

      // Show selection dialog
      showSelectionDialog(result);

      // Clean up
      svgOverlay.clear();
      setDrawingMode('none');
    }
  });

  // 3. Cleanup
  return () => {
    mouseEvents.detachMouseHandlers(svg);
  };
}, [drawingMode, svgOverlay, coordinateTransform, mouseEvents]);
```

## Performance Considerations

### SVG Overlay
- Absolute positioning: No layout recalculation
- Pointer-events toggling: Fast browser operation
- Minimal DOM nodes: Only active elements

### Coordinate Transformation
- **Projection lookup:** Cached from d3-celestial
- **Math operations:** All O(1) constant time
- **No database calls** during drawing (until completion)

### Mouse Events
- `d3.pointer()` is optimized for performance
- Event handlers are debounced if needed in drawing tools
- Unbound immediately when tool completes

### Expected Performance
- Smooth 60 FPS mouse tracking
- <1ms coordinate transformations
- Real-time drawing feedback

## Testing in Phase 2

### Manual Testing Checklist
- [ ] SVG overlay is invisible by default
- [ ] SVG overlay appears when entering drawing mode
- [ ] SVG overlay has correct z-index (above markers)
- [ ] Mouse events are captured when overlay enabled
- [ ] Coordinate transformations work both directions
- [ ] Visibility checks correctly identify off-screen coordinates
- [ ] Overlay clears properly between operations

### Edge Cases to Test
- [ ] Drawing at map edges (partial visibility)
- [ ] Drawing at RA = 0°/360° boundary
- [ ] Drawing near poles (Dec = ±90°)
- [ ] Very large circles (>90° radius)
- [ ] Very small circles (<0.1° radius)

## Files Created/Modified

```
New Files:
├── src/hooks/
│   ├── useSvgOverlay.ts (80 lines)
│   ├── useCoordinateTransform.ts (130 lines)
│   └── useD3MouseEvents.ts (120 lines)
└── docs/
    └── PHASE2_SVG_OVERLAY.md (this file)

Modified Files:
├── src/pages/SkyAtlas.tsx (+40 lines)

Total New Code: 330 lines
Total Documentation: 350+ lines
```

## Next Steps (Phase 3)

With the SVG overlay infrastructure in place, Phase 3 will implement the actual drawing tools:

### Phase 3: Circle Selection Tool
- Use `onMouseDown` to capture center
- Use `onMouseMove` to show radius indicator
- Use `onMouseUp` to query backend and show results

### Phase 4: Rectangle Selection Tool
- Handle two corner points
- Support dragging in any direction
- Show bounds on drag

### Phase 5: Polygon Selection Tool
- Multiple vertex clicks
- Preview line from last vertex to cursor
- Double-click to complete

## Architecture Advantages

1. **Separation of Concerns**
   - SVG overlay independent of drawing tools
   - Coordinate transform independent of drawing mode
   - Mouse events independent of interaction type

2. **Reusability**
   - Hooks used by all drawing tools
   - Consistent event handling across tools
   - Shared coordinate system

3. **Maintainability**
   - Each hook has single responsibility
   - Clear interfaces between components
   - Easy to test each layer independently

4. **Performance**
   - No unnecessary DOM operations
   - Efficient coordinate caching
   - Minimal event handler overhead

## Summary

Phase 2 establishes the foundational infrastructure for interactive drawing on the sky map:

✅ SVG overlay layer
✅ Coordinate transformation utilities
✅ D3-based mouse event handling
✅ Integration with existing SkyAtlas component
✅ Clean API for drawing tools to use

**Status:** Ready for Phase 3 (Circle Selection Tool)
