# BlinkViewer Unified Refactor

## Overview
Complete refactor of BlinkViewer to be a unified, feature-rich image viewer for both calibration and light frames across the entire app.

## Target UI Layout
```
┌─────────────────────────────────────────────────────────────────────────┐
│ TOOLBAR: [◀][▶][▶/⏸] Speed:[===] | Frame 3/45 | [Actions when selected] │ X
├───────────────────────────────────────────────────────┬─────────────────┤
│                                                       │ FRAME LIST      │
│                                                       │ ☐ frame001.fits │
│                 CANVAS                                │ ☑ frame002.fits │
│            (Image display)                            │ ☐ frame003.fits │
│                                                       │ ...             │
│                                                       │                 │
├───────────────────────────────────────────────────────┴─────────────────┤
│ DETAILS: filename.fits | 2024-01-15 | ASI2600 | Gain:100 Offset:50 | -10°C │
└─────────────────────────────────────────────────────────────────────────┘
```

## Selection Behavior
- **Space bar**: Toggle select/unselect current frame
- **Shift+Click**: Select range from last selected to clicked frame
- **Click**: Navigate to frame (view it on canvas)
- Visual: Checkbox per frame, highlighted when selected

---

# STAGED IMPLEMENTATION PLAN

## Stage 1: Core UI Restructure
**Goal**: New layout with toolbar at top, details at bottom, remove debug console

### Tasks
- [ ] 1.1 Remove debug console state and UI completely
- [ ] 1.2 Move playback controls (◀ ▶ ▶/⏸) to top toolbar
- [ ] 1.3 Move speed slider to top toolbar
- [ ] 1.4 Move frame counter to top toolbar
- [ ] 1.5 Create bottom details bar component
- [ ] 1.6 Show current frame details: filename, date, telescope, camera, gain/offset, temperature
- [ ] 1.7 Update layout: toolbar (top) → main area (canvas + list) → details (bottom)

**Files to modify**:
- `src/components/BlinkViewer.tsx`

---

## Stage 2: Auto-Cache on Open
**Goal**: Automatically start caching all frames when viewer opens

### Tasks
- [ ] 2.1 Trigger cache operation on component mount (useEffect)
- [ ] 2.2 Show subtle progress indicator in toolbar (not blocking)
- [ ] 2.3 Remove manual "Cache All" button
- [ ] 2.4 Ensure caching doesn't block initial frame display
- [ ] 2.5 Optimize: Start with current frame, then cache others in background

**Files to modify**:
- `src/components/BlinkViewer.tsx`

---

## Stage 3: Frame Selection System
**Goal**: Multi-select frames with keyboard and mouse

### Tasks
- [ ] 3.1 Add `selectedFrames: Set<number>` state (indices)
- [ ] 3.2 Add `lastSelectedIndex` state for shift-click range
- [ ] 3.3 Add checkbox UI to each frame in list
- [ ] 3.4 Implement Space bar: toggle current frame selection
- [ ] 3.5 Implement Click: navigate to frame (set currentIndex)
- [ ] 3.6 Implement Shift+Click: select range from lastSelectedIndex to clicked
- [ ] 3.7 Add "Select All" / "Clear Selection" buttons
- [ ] 3.8 Visual styling for selected frames (highlight + checkbox)
- [ ] 3.9 Show selection count in toolbar when > 0

**Files to modify**:
- `src/components/BlinkViewer.tsx`

---

## Stage 4: Blackhole Integration
**Goal**: Send selected frames to blackhole

### Tasks
- [ ] 4.1 Add "Send to Blackhole" button in toolbar (visible when selection exists)
- [ ] 4.2 Add confirmation dialog before blackhole action
- [ ] 4.3 Call `move_to_black_hole` for each selected frame
- [ ] 4.4 Remove blackholed frames from viewer list
- [ ] 4.5 Update frame indices after removal
- [ ] 4.6 Show success/error feedback
- [ ] 4.7 Add `onFramesRemoved` callback prop for parent to refresh

**Files to modify**:
- `src/components/BlinkViewer.tsx`

**Backend commands used**:
- `move_to_black_hole(file_id, from_where)`

---

## Stage 5: Code Optimization
**Goal**: Clean up and optimize performance

### Tasks
- [ ] 5.1 Extract FrameList into separate component
- [ ] 5.2 Extract ToolBar into separate component
- [ ] 5.3 Extract DetailsBar into separate component
- [ ] 5.4 Memoize expensive computations with useMemo
- [ ] 5.5 Memoize callbacks with useCallback
- [ ] 5.6 Review and remove unused state/refs
- [ ] 5.7 Add proper TypeScript interfaces for all props

**Files to create**:
- `src/components/blink/FrameList.tsx`
- `src/components/blink/ToolBar.tsx`
- `src/components/blink/DetailsBar.tsx`
- `src/components/blink/types.ts`

**Files to modify**:
- `src/components/BlinkViewer.tsx` (refactor to use new components)

---

## Stage 6: Unify Usage Across App
**Goal**: Ensure BlinkViewer works consistently everywhere

### Tasks
- [ ] 6.1 Update CalibrationSetTable integration
- [ ] 6.2 Update FrameSetDetail integration
- [ ] 6.3 Add `frameType` prop to distinguish calibration vs lights
- [ ] 6.4 Ensure blackhole works for both frame types
- [ ] 6.5 Test with FITS and XISF files

**Files to modify**:
- `src/components/CalibrationSetTable.tsx`
- `src/pages/FrameSetDetail.tsx`
- `src/components/BlinkViewer.tsx`

---

## Stage 7 (BONUS): Split/Merge/Create Set Actions
**Goal**: Add frame set management from within blink viewer

### Tasks
- [ ] 7.1 Add "Create Set" button (visible when selection exists)
- [ ] 7.2 Add name input dialog for new set
- [ ] 7.3 Call `create_frame_set_from_selection` with selected frame IDs
- [ ] 7.4 Add "Split to New Set" for existing frame set context
- [ ] 7.5 Mark split sets as custom (is_custom=true) so they survive rebuilds
- [ ] 7.6 Add `onSetCreated` callback prop for parent refresh
- [ ] 7.7 Conditionally show split/merge based on context (calibration vs lights)

**Files to modify**:
- `src/components/BlinkViewer.tsx`
- `src/components/blink/ToolBar.tsx`

**Backend commands used**:
- `create_frame_set_from_selection(name, frame_ids)`
- `split_frame_set(source_set_id, selection, new_name)`

---

## Props Interface (Target)

```typescript
interface BlinkViewerProps {
  frames: FileWithFrame[];
  initialIndex?: number;
  onClose: () => void;

  // Context for actions
  sourceType: 'light' | 'calibration';
  sourceSetId?: number;  // For split operations

  // Callbacks
  onFramesRemoved?: (frameIds: number[]) => void;
  onSetCreated?: (setId: number) => void;
}
```

---

## Files Summary

### Modified
- `src/components/BlinkViewer.tsx` - Main refactor
- `src/components/CalibrationSetTable.tsx` - Update props
- `src/pages/FrameSetDetail.tsx` - Update props

### Created
- `src/components/blink/FrameList.tsx`
- `src/components/blink/ToolBar.tsx`
- `src/components/blink/DetailsBar.tsx`
- `src/components/blink/types.ts`

---

## Implementation Order

1. **Stage 1** - UI restructure (foundation)
2. **Stage 2** - Auto-cache (improves UX immediately)
3. **Stage 3** - Selection system (required for Stage 4+)
4. **Stage 4** - Blackhole (core feature)
5. **Stage 5** - Code optimization (cleanup)
6. **Stage 6** - Unify usage (consistency)
7. **Stage 7** - Bonus features (if time permits)

Each stage is independent and testable. We can pause after any stage.

---

## Progress Log

### Stage 1: Core UI Restructure
- Status: COMPLETED
- Removed debug console completely
- Moved playback controls (◀ ▶ ▶/⏸) to top toolbar
- Moved speed slider and frame counter to toolbar
- Created bottom details bar with frame metadata
- Updated layout: toolbar (top) → main area (canvas + list) → details (bottom)

### Stage 2: Auto-Cache on Open
- Status: COMPLETED
- Trigger cache operation on component mount
- Show subtle progress indicator in toolbar
- Caching doesn't block initial frame display
- Starts with current frame, then caches others in background

### Stage 3: Frame Selection System
- Status: COMPLETED
- Added `selectedFrames: Set<number>` state
- Added `lastSelectedIndex` state for shift-click range
- Added checkbox UI to each frame in list
- Space bar: toggle current frame selection
- Ctrl+Space: toggle playback
- Shift+Click: select range from lastSelectedIndex to clicked
- Ctrl+A: select all frames
- "Select All" / "Clear Selection" buttons
- Visual styling for selected frames (yellow highlighting)
- Selection count shown in toolbar

### Stage 4: Blackhole Integration
- Status: COMPLETED
- Added "Send to Blackhole" button in toolbar (visible when selection exists)
- Added confirmation dialog before blackhole action
- Calls `move_to_black_hole` for each selected frame
- Removes blackholed frames from viewer list (via localFrames state)
- Updates frame indices after removal
- Shows error feedback
- Added `onFramesRemoved` callback prop for parent to refresh
- Added `sourceType` prop ('light' | 'calibration')
