# d3-Celestial Initial Load Callback Issue - Root Cause and Solution

**Date**: November 11, 2024
**Component**: Sky Atlas (SkyAtlas.tsx)
**Issue**: Custom markers (crosshairs) not appearing on initial page load
**Status**: ✅ Resolved

---

## Problem Summary

Custom imaging location markers (green crosshairs) were not rendering on the initial load of the Sky Atlas page. However, they would appear correctly after:
- Navigating away and returning to the page
- Panning/zooming the map
- Any user interaction with the celestial map

This indicated a race condition or initialization timing issue rather than a coordinate calculation problem.

---

## Root Cause Analysis

### The Issue with `Celestial.add()` Callbacks

The d3-celestial library provides a `Celestial.add()` method to register custom data layers:

```javascript
window.Celestial.add({
  type: 'raw',
  callback: function(error) {
    // Rendering logic here
  },
  redraw: function() {
    // Update logic for pan/zoom
  }
});
```

**The fundamental problem**: The `callback` function in `Celestial.add()` is designed for **loading external data files** asynchronously, not for rendering in-memory data on initial load.

### Why the Callback Wasn't Executing

1. **Callback Design Assumption**: The `callback` parameter expects an error-first callback signature `function(error)`, which is typical for asynchronous file loading operations
2. **Execution Trigger**: The callback is only reliably triggered when:
   - External data is loaded (e.g., from a URL or file)
   - The map undergoes certain lifecycle events (pan, zoom, redraw)
   - NOT on initial registration with in-memory data
3. **Race Condition**: On first load, the callback registration happens but doesn't execute because:
   - No external data loading is triggered
   - The map initialization might complete before the callback is registered
   - There's no explicit trigger to invoke the callback synchronously

### Evidence from Console Logs

**Before the fix:**
```
🎯 Marker useEffect triggered, calling addImagingMarkers
Adding 55 imaging location markers (zoomed: false) - filtered from 55
✨ Pre-transformed data features: – 58
🔄 Forcing redraw #1
🔄 Forcing redraw #2
🔄 Forcing redraw #3
```

Notice: The callback log `🎨 Callback executing...` **never appeared**, confirming the callback wasn't being called.

**Secondary Issue - Data Format Error:**

When we forced multiple `window.Celestial.redraw()` calls, the callback did execute but threw a TypeError:
```
TypeError: undefined is not an object (evaluating 'n[0]=n&&-n[0]')
```

This occurred because `window.Celestial.getData()` was being called **inside** the callback context, where it didn't have proper access to transform the coordinate data correctly.

---

## Solution Architecture

### Key Insight

Instead of relying on `Celestial.add()` callback to render markers on initial load, we:
1. **Extract rendering logic** into a standalone function
2. **Call it immediately** when data is available (synchronous execution)
3. **Register it with Celestial** for future pan/zoom updates

### Implementation

**Before (broken approach):**
```javascript
// Transform data inside the callback
window.Celestial.add({
  type: 'raw',
  callback: function(error) {
    // ❌ This never executes on first load
    const data = window.Celestial.getData(imagingData, ...);
    // Rendering logic...
  }
});
```

**After (working approach):**
```javascript
// 1. Transform data BEFORE callback registration
const transformedData = window.Celestial.getData(
  imagingData,
  window.Celestial.settings().transform
);

// 2. Extract rendering logic into a function
const renderMarkers = () => {
  const data = transformedData;

  // Get SVG overlay
  let svg = d3.select('#celestial-map').select('svg.imaging-markers-overlay');
  if (svg.empty()) {
    svg = d3.select('#celestial-map')
      .append('svg')
      .attr('class', 'imaging-markers-overlay')
      .style('position', 'absolute')
      .style('top', '0')
      .style('left', '0')
      .style('width', '100%')
      .style('height', '100%');
  }

  // Create markers group
  let markersGroup = svg.select('g.imaging-markers-layer');
  if (markersGroup.empty()) {
    markersGroup = svg.append('g').attr('class', 'imaging-markers-layer');
  }

  // Clear old markers
  markersGroup.selectAll('.imaging-marker').remove();

  // Draw markers with coordinate projection and canvas scaling
  const scaling = getCanvasScaling();
  markersGroup.selectAll('.imaging-marker')
    .data(data.features)
    .enter().append('path')
    .attr('class', 'imaging-marker')
    .attr('d', 'M-8,0 L8,0 M0,-8 L0,8')
    .attr('transform', function(d) {
      const coords = d.geometry.coordinates;
      const pt = window.Celestial.map.projection()(coords);
      if (!pt) return 'translate(0,0)';

      // Apply canvas scaling for correct positioning
      const scaledX = pt[0] * scaling.scaleX;
      const scaledY = pt[1] * scaling.scaleY;
      return `translate(${scaledX},${scaledY})`;
    })
    .style('stroke', '#22c55e')
    .style('stroke-width', '2px');
};

// 3. Call immediately for initial render ✅
renderMarkers();

// 4. Register for pan/zoom updates
window.Celestial.add({
  type: 'raw',
  callback: renderMarkers,  // Reuse the same function
  redraw: function() {
    // Update marker positions on pan/zoom
    // ...
  }
});
```

---

## Technical Details

### Why This Works

1. **Synchronous Execution**: `renderMarkers()` is called immediately when `addImagingMarkers()` runs, ensuring markers appear on first load
2. **Pre-transformed Data**: `window.Celestial.getData()` is called outside the callback, avoiding the data format TypeError
3. **Closure Scope**: `transformedData` is captured in the closure, accessible to both immediate call and future callback executions
4. **Callback Reuse**: The same `renderMarkers` function is registered for pan/zoom events, maintaining consistency

### Canvas Scaling Fix

An additional issue was canvas stretching affecting marker positions. The solution includes a `getCanvasScaling()` function:

```javascript
const getCanvasScaling = () => {
  const canvas = document.querySelector('#celestial-map canvas');
  if (!canvas) return { scaleX: 1, scaleY: 1 };

  const canvasWidth = canvas.width;
  const canvasHeight = canvas.height;
  const displayRect = canvas.getBoundingClientRect();
  const displayWidth = displayRect.width;
  const displayHeight = displayRect.height;

  return {
    scaleX: displayWidth / canvasWidth,
    scaleY: displayHeight / canvasHeight
  };
};
```

This accounts for CSS `object-fit: fill` stretching the canvas when the window aspect ratio differs from the native Aitoff projection (2:1).

### Coordinate Transformation

The RA coordinate conversion was simplified:

```javascript
// Use RA directly in 0-360° range
const raToGeoJsonLongitude = (ra: number): number => {
  return ra;  // No conversion needed for d3-celestial
};
```

Previously, we tried converting RA > 180° to negative values, but d3-celestial expects 0-360° directly.

---

## Files Modified

### Primary File
- **`src/pages/SkyAtlas.tsx`**
  - Lines 236-238: Simplified RA coordinate conversion
  - Lines 241-261: Added `getCanvasScaling()` function
  - Lines 376-591: Refactored marker rendering architecture
  - Lines 661-667: Simplified marker useEffect (removed forced redraws)

### Supporting File
- **`src/styles/celestial-overrides.css`**
  - Line 24: `object-fit: fill` causes canvas stretching (addressed by scaling calculations)

---

## Lessons Learned

### 1. Library API Assumptions
Don't assume callback-based APIs work the same way for all use cases:
- `Celestial.add()` callback is designed for **async file loading**
- Using it for **in-memory rendering** requires a different approach

### 2. Race Conditions
Initial load timing issues often indicate:
- Callbacks not being triggered as expected
- Need for synchronous execution paths
- Importance of explicit initialization

### 3. Debugging Strategy
The debugging process revealed:
1. **Log absence** (callback not executing) is as important as error logs
2. **Forced redraws** exposed the secondary data format issue
3. **Architectural change** (direct execution) was needed, not just parameter tweaking

### 4. SVG Overlay Pattern
Creating custom overlays on d3-celestial canvas:
- Position absolutely over canvas
- Match dimensions with `width: 100%`, `height: 100%`
- Account for canvas scaling when positioning elements
- Use `pointer-events: none` on overlay, `auto` on interactive elements

---

## Testing Checklist

To verify the fix works correctly:

- [x] Markers appear on initial page load (no navigation required)
- [x] Markers positioned at correct sky coordinates
- [x] Markers follow map on pan operations
- [x] Markers follow map on zoom operations
- [x] Markers scaled correctly at all window aspect ratios
- [x] Click handlers work (navigate to frame sets)
- [x] Tooltips display correctly
- [x] No console errors related to coordinate projection

---

## Related Issues

### Previously Resolved
1. **Coordinate positioning accuracy**: Fixed by using RA 0-360° directly (src/pages/SkyAtlas.tsx:236-238)
2. **Aspect ratio scaling**: Fixed by calculating and applying canvas scale factors (src/pages/SkyAtlas.tsx:241-261)
3. **Mouse panning broken**: Fixed by keeping `orientationfixed: false` (src/pages/SkyAtlas.tsx:101)

### Remaining Warnings (Non-critical)
- JSON parse errors for d3-celestial data files - these are normal for missing optional data layers
- Container not found warnings during initial mount - resolved on retry

---

## References

- **d3-celestial Documentation**: https://github.com/ofrohn/d3-celestial
- **Issue Guide**: `/FOV_COORDINATE_FIX_GUIDE.md` (comprehensive debugging steps)
- **Related Commits**:
  - `330dcc2` - Fix guide from Claude
  - `c131f55` - Draws crosshairs but can fix location
  - `d99df68` - Implement canvas coordinate scaling

---

## Future Improvements

1. **Performance**: Consider debouncing the `redraw` function for smoother pan/zoom
2. **Memory**: Clean up SVG overlay when component unmounts
3. **Accessibility**: Add ARIA labels to markers for screen readers
4. **Testing**: Add unit tests for coordinate transformation and scaling calculations
5. **Documentation**: Add JSDoc comments to `renderMarkers()` and helper functions
