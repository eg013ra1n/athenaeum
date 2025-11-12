# Positioning Debug Guide

## The Issue (SOLVED)

Marker positioning issues on HiDPI displays and during window resize have been resolved.

## Root Cause

The issue was a misunderstanding of d3-celestial's coordinate system:

1. **d3-celestial uses a projection-based coordinate system** that is independent of the canvas's internal resolution
2. **Aitoff projection has a fixed 2:1 aspect ratio** - the projection width is the configured width, and height is always width/2
3. **Canvas can be stretched** to fill the container, but projection coordinates remain in the 2:1 space
4. **HiDPI displays** create high-resolution canvases (e.g., 2691x1346 for a 1344x672 display) but this doesn't affect the projection coordinate system

## The Solution

The correct scaling approach (SkyAtlas.tsx:235-262):

```typescript
const getCanvasScaling = () => {
  const canvas = document.querySelector('#celestial-map canvas') as HTMLCanvasElement;
  if (!canvas) return { scaleX: 1, scaleY: 1, offsetX: 0, offsetY: 0 };

  const displayRect = canvas.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;

  // d3-celestial uses Aitoff projection with 2:1 aspect ratio
  // The projection coordinate system is based on the width we configured
  const celestialConfig = (window as any).Celestial?.settings;
  const projectionWidth = celestialConfig?.width || canvas.width / dpr;
  const projectionHeight = projectionWidth / 2; // Aitoff is always 2:1

  // Scale projection coords to match actual display size
  const scaleX = displayRect.width / projectionWidth;
  const scaleY = displayRect.height / projectionHeight;

  return { scaleX, scaleY, offsetX: displayRect.left, offsetY: displayRect.top };
};
```

**Key insights**:
- Get the configured projection width from `Celestial.settings.width`
- Calculate projection height as `width / 2` (Aitoff's fixed aspect ratio)
- Scale by dividing display dimensions by projection dimensions
- This works on both regular displays (dpr=1) and HiDPI displays (dpr=2)
- Handles window resize and aspect ratio changes correctly

## How to Verify the Fix

### 1. Check Console Logs

When markers are added, you should see:
```
🔍 Canvas internal: 2691x1346
🔍 Canvas display: 1344.0x672.0
🔍 Projection space: 1344x672
🔍 Device pixel ratio: 2
🔍 Scale factors: 1.000x1.000
```

**What to expect**:
- On HiDPI displays (dpr=2): Canvas internal is 2x display size, but scale factors should be ~1.0
- On regular displays (dpr=1): Canvas internal matches display size, scale factors ~1.0
- When maintaining 2:1 aspect ratio: scaleX and scaleY should be equal and ~1.0
- When stretching window: scaleX and scaleY will differ to compensate for aspect ratio change

### 2. Check Device Pixel Ratio

Open browser devtools console and run:
```javascript
window.devicePixelRatio
```

**Expected values**:
- 1.0 = Standard display
- 2.0 = Retina/HiDPI display
- Other values = Non-standard scaling

### 3. Inspect Canvas vs SVG Dimensions

In devtools console:
```javascript
const canvas = document.querySelector('#celestial-map canvas');
const svg = document.querySelector('#celestial-map svg.imaging-markers-overlay');

console.log('Canvas internal:', canvas.width, 'x', canvas.height);
console.log('Canvas displayed:', canvas.getBoundingClientRect().width, 'x', canvas.getBoundingClientRect().height);
console.log('SVG dimensions:', svg.getBoundingClientRect().width, 'x', svg.getBoundingClientRect().height);
```

**What to check**:
- SVG dimensions should match canvas **displayed** dimensions (not internal)
- If they don't match, markers will be offset

### 4. Check Marker Positions

In devtools console:
```javascript
const markers = document.querySelectorAll('.imaging-marker');
markers.forEach((m, i) => {
  const transform = m.getAttribute('transform');
  console.log(`Marker ${i}: ${transform}`);
});
```

Check if transforms look reasonable (should be within canvas bounds).

### 5. Test at Different Resolutions

1. **Full screen** the app
2. **Resize** the window
3. Check if positions update correctly
4. Try **different zoom levels** (Ctrl/Cmd + Plus/Minus)

## Historical Issues (All Resolved)

### Issue 1: DPI Scaling Mismatch ✅ SOLVED

**Symptom**: Markers were offset by 2x on HiDPI displays

**Root Cause**: Code was trying to scale by devicePixelRatio, but d3-celestial's projection system is independent of canvas internal resolution

**Fix**: Use projection space dimensions (configured width and width/2 for height) instead of canvas internal dimensions

### Issue 2: Aspect Ratio Distortion ✅ SOLVED

**Symptom**: Markers were correct at 2:1 aspect ratio but wrong when window was resized

**Root Cause**: Aitoff projection is always 2:1, but canvas can be stretched to any aspect ratio

**Fix**: Calculate separate scaleX and scaleY based on display size vs projection size, allowing for aspect ratio compensation

### Issue 3: Projection Coordinate System Misunderstanding ✅ SOLVED

**Symptom**: Various incorrect scaling attempts (scale=1, scale=1/dpr, etc.) all failed

**Root Cause**: Didn't understand that projection coordinates are in their own space defined by `Celestial.settings.width`

**Fix**: Access the configured width from Celestial settings and use that as the basis for scaling calculations

## Testing the Fix

### Test 1: HiDPI Display
1. Open app on a Retina/HiDPI display (devicePixelRatio = 2)
2. Check console logs - should show projection space matching display size
3. Verify markers align correctly with celestial objects
4. Pan and zoom - markers should track correctly

### Test 2: Aspect Ratio Changes
1. Start with a 2:1 aspect ratio window (scale factors should be ~1.0)
2. Resize window to make it taller/shorter
3. Console should show different scaleX and scaleY values
4. Markers should remain correctly positioned despite aspect ratio change

### Test 3: Rectangle Selection Tool
1. Click the rectangle selection button (or press 'S')
2. Draw a rectangle over a region with known imaging targets
3. Verify that frames are correctly detected within the selection
4. Selection bounds should match the visual rectangle on the map

## Diagnostic Commands

Check projection coordinate space:
```javascript
// Get projection dimensions
const config = window.Celestial.settings;
console.log('Projection width:', config.width);
console.log('Projection height:', config.width / 2);

// Test a known object (M42 Orion Nebula)
const coords = [83.82, -5.39];
const projected = window.Celestial.map.projection()(coords);
console.log('M42 projected coords:', projected);

// Verify visibility
console.log('M42 visible?', window.Celestial.clip(coords));
```

Check display vs projection scaling:
```javascript
const canvas = document.querySelector('#celestial-map canvas');
const rect = canvas.getBoundingClientRect();
const config = window.Celestial.settings;
const projWidth = config.width;
const projHeight = projWidth / 2;

console.log('Display:', rect.width, 'x', rect.height);
console.log('Projection:', projWidth, 'x', projHeight);
console.log('Scale X:', rect.width / projWidth);
console.log('Scale Y:', rect.height / projHeight);
```
