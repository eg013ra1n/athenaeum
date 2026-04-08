# Calibration Table View Implementation Plan

> **For agentic workers:** Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the graph view with 4 collapsible sortable tables (Lights, Flats, Darks, Bias) with cross-referencing via clickable set IDs and row highlighting.

**Architecture:** New `CalibrationTableView` component replaces `CalibrationGraphView`. Reuses the same data derivation pattern (group frames by flat+dark combo). Tables are standard HTML with sticky headers, no SVG.

**Tech Stack:** React, TypeScript, Tailwind CSS

---

### Task 1: Create CalibrationTableView with data derivation

**Files:**
- Create: `src/components/calibration/CalibrationTableView.tsx`

Create the component with data derivation (reuse grouping logic from CalibrationGraphView) and render all 4 table sections.

- [ ] **Step 1: Create the component**

The component receives the same props as CalibrationGraphView: `data`, `allFrames`, `visibleFrameIds`, `onManualCalibration`.

Data derivation:
1. Merge filter groups across dates by camera (same as graph view)
2. Group frames by (filter, exposure, flat_set_id, dark_set_id) — each combo = one light row
3. Collect unique flat sets, dark sets, bias sets
4. Compute status per light row: green (has flat+dark), orange (partial), red (none)

Render 4 collapsible sections with tables. Each section has:
- Colored header bar (click to collapse/expand)
- Sticky table headers
- Sortable columns (click header to sort)
- Cross-reference: clicking a light row highlights linked flat/dark rows

- [ ] **Step 2: Verify compilation**

Run: `npm run build 2>&1 | grep -E "error|✓ built"`

- [ ] **Step 3: Commit**

---

### Task 2: Wire into CalibrationHierarchyView

**Files:**
- Modify: `src/components/CalibrationHierarchyView.tsx`

- [ ] **Step 1: Replace import and usage**

Replace `CalibrationGraphView` with `CalibrationTableView` — same props.

- [ ] **Step 2: Delete CalibrationGraphView**

Remove `src/components/calibration/CalibrationGraphView.tsx`.

- [ ] **Step 3: Verify and commit**

Run: `npm run build 2>&1 | grep -E "error|✓ built"`

---

### Task 3: Polish and verify

- [ ] **Step 1: Test with real data** — verify tables render correctly with multiple cameras, filters, sets
- [ ] **Step 2: Test cross-referencing** — click light rows, verify flat/dark rows highlight
- [ ] **Step 3: Test sorting** — click column headers
- [ ] **Step 4: Test collapsing** — collapse/expand sections
- [ ] **Step 5: Final build check**
