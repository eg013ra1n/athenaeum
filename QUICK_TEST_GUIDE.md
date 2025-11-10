# Quick Test Guide - Circle Selection Tool

**Last Updated:** 2025-11-10
**Build Status:** ✅ Ready for Testing
**Issue Tracker:** Fixed DOM timing issue with SVG overlay

## TL;DR - Quick Test

```bash
# 1. Build
npm run build

# 2. Run
npm run tauri dev

# 3. Test
- Go to Sky Atlas
- Click "Circle" button (should highlight blue)
- Click on map (blue circle should appear)
- Drag (circle should grow)
- Release (dialog with results)
```

If you see the blue circle appear when you click, IT WORKS! ✅

## What Was Fixed

**Three critical bugs found and fixed:**

1. **Event Handler API Mismatch** ✅
   - Changed from D3 `.on()` to native `addEventListener()`

2. **Hook Instance Duplication** ✅
   - SVG overlay now passed as parameter instead of creating new instances

3. **DOM Timing Issue** ✅
   - SVG overlay container not found when effect ran
   - Added retry logic to wait for container

## Expected Console Output

When you start the app, you should see:

```
[Debug] [vite] connected.
[Log] Initializing sky map with dimensions: 1758x964
[Log] Sky map initialized successfully
[Log] Adding 7 imaging location markers

# One of these two:
# Fast timing:
[Log] SVG overlay container found: celestial-map

# Slow timing:
[Warning] Container with id "celestial-map" not found, will retry
[Log] Container found on retry: celestial-map
```

**If you don't see either of those, there's still a problem.**

## Test Procedure

### Test 1: Basic Circle Drawing

1. Click "Circle" button
   - ✅ Button highlights blue
   - ✅ Instructions appear: "Click to set center, drag to set radius"

2. Click on map (anywhere on the sky)
   - ✅ Blue circle appears at click point
   - ✅ Console shows: "mousedown event fired"
   - ✅ Console shows: "Circle onMouseDown at: X Y"

3. Drag mouse away from center
   - ✅ Circle grows/shrinks as you move
   - ✅ Console shows: many "mousemove event fired" messages

4. Release mouse
   - ✅ Console shows: "mouseup event fired"
   - ✅ Dialog appears with "Circle Selection Results"
   - ✅ Dialog shows frame count (e.g., "Frames Found: 12")
   - ✅ Dialog shows exposure time (e.g., "Total Exposure: 2.5h")

### Test 2: Frame Set Creation

1. After circle query completes (dialog appears):
   - ✅ Type a frame set name (e.g., "M31 Session")
   - ✅ Click "Create Set" button
   - ✅ Loading spinner appears
   - ✅ Success message appears after 1-2 seconds
   - ✅ Dialog auto-closes

### Test 3: Multiple Selections

1. Repeat Test 1 with different circle
   - ✅ Can do multiple circles in succession
   - ✅ Each creates separate frame set
   - ✅ No conflicts between selections

### Test 4: Cancel Selection

1. Click "Circle" button
2. Click "×" (cancel) button
   - ✅ Circle mode deactivates
   - ✅ Can activate again without issues

## Troubleshooting

### Problem: "SVG overlay not available" error
**Solution:** This means the retry logic didn't work
- Check that `celestial-map` div exists in DOM
- Open DevTools → Elements → search for "celestial-map"
- Should be a large div with sky map

### Problem: Circle doesn't appear when clicking
**Solution:** Event listeners not attached
- Open DevTools → Console
- Type: `document.querySelector('.selection-overlay')`
- Should return `<svg class="selection-overlay"...>`
- If `null`, SVG overlay wasn't created

### Problem: Dialog shows but frame set not created
**Solution:** Check backend
- Open DevTools → Network tab
- Look for `invoke` call to `create_frame_set_from_selection`
- Check Response for errors
- Check Tauri console for backend errors

### Problem: Map rotates instead of drawing circle
**Solution:** Event handlers not working
- Open DevTools → Console
- Click Circle button, then click on map
- Look for "mousedown event fired" in console
- If not there, event listeners not attached

## Console Logging Guide

### What You Should See (In Order)

```
1. ✅ "SVG overlay container found" or "Container found on retry"
   → SVG overlay created successfully

2. ✅ "Circle selection effect triggered"
   → Circle mode activated

3. ✅ "Attaching mouse handlers to SVG"
   → Event listeners being setup

4. ✅ "Attaching handlers to element: <svg...> Tag: svg"
   → Listeners attached to correct element

5. ✅ "Element supports addEventListener, attaching events"
   → Native event listeners working

6. ✅ "mousedown listener attached" (3 more for mousemove, mouseup, dblclick)
   → All listeners registered

7. [User clicks on map]

8. ✅ "mousedown event fired"
   → Click detected

9. ✅ "Circle onMouseDown at: X Y"
   → Drawing started

10. ✅ "Center sky coords: [RA, Dec]"
    → Coordinates calculated

11. ✅ "Circle element created at: X Y"
    → Circle SVG created
```

If you're missing any of these, check which one is missing and refer to the troubleshooting guide.

## Browser DevTools Tips

### Inspect SVG Overlay
```javascript
// In browser console:
document.querySelector('.selection-overlay')

// Should show the SVG element
// Check its width/height/position
// Should be same size as map
```

### Check Event Listeners
```javascript
// In browser console:
const svg = document.querySelector('.selection-overlay');
getEventListeners(svg);  // Chrome only

// Shows all attached listeners
```

### Verify Coordinate Transform
```javascript
// In browser console:
window.Celestial.mapProjection.invert([500, 300])
// Should return [RA, Dec] array

window.Celestial.mapProjection([123.456, 45.678])
// Should return [x, y] array
```

## Performance Notes

- **Circle drawing FPS:** Should be smooth (60 FPS)
- **Query time:** 100-500ms depending on data
- **Dialog open:** Instant
- **Frame set creation:** 1-2 seconds

If any operation lags, check:
- Browser: Open DevTools → Performance
- Tauri: Check Rust console for errors
- Database: Check if frames table is large

## Success Criteria

✅ All of these must be true:
- [ ] Circle button highlights when clicked
- [ ] Blue circle appears on map when clicking
- [ ] Circle grows when dragging
- [ ] Dialog appears after release
- [ ] Frame count shows in dialog
- [ ] Exposure time shows in dialog
- [ ] Can create frame set from dialog
- [ ] Success message appears
- [ ] Dialog closes after 2 seconds
- [ ] No red errors in console (only blue debug/warnings OK)

## Next Steps

### If Tests Pass
- Continue to Phase 4 (Rectangle Selection Tool)
- Same architecture pattern

### If Tests Fail
- Check console logs carefully
- Note which step fails
- Refer to troubleshooting section
- Create bug report with:
  - Console output
  - Steps to reproduce
  - Expected vs actual behavior

## File References

For more details, see:
- `FINAL_DEBUG_SOLUTION.md` - Detailed timing analysis
- `CIRCLE_DRAWING_DEBUG.md` - Hook integration fix
- `CIRCLE_SELECTION_FIX.md` - Event handler fix
- `TESTING_CIRCLE_SELECTION.md` - Comprehensive test guide

---

**Version:** 1.0
**Last Updated:** 2025-11-10
**Status:** Ready for Testing ✅
