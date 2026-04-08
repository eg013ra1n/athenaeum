# Calibration Coverage — Sectioned Tables Design

## Problem

The graph view doesn't scale for large datasets — too many SVG lines, wasted space, and hard to parse visually. Astrophotographers need dense, sortable data to research calibration coverage and parameter matching across many filters and nights.

## Design

Replace the graph view with **4 collapsible table sections**: Lights, Flats, Darks, Bias. Each is a sortable, dense table with cross-referencing via clickable set IDs. Clicking a light row highlights its linked flat and dark rows.

### Layout

4 stacked sections, each collapsible:

1. **Lights** (default open) — one row per unique (filter, exposure, flat_set_id, dark_set_id) combination
2. **Flats** — one row per unique flat calibration set
3. **Darks** — one row per unique dark calibration set
4. **Bias** — one row per unique bias calibration set

Each section has a colored header bar with type name + count badge. Click header to collapse/expand.

### Section Colors

- Lights: blue (`#5e81ac`)
- Flats: cyan (`#88c0d0`)
- Darks: purple (`#b48ead`)
- Bias: orange (`#d08770`)

### Lights Table Columns

| Column | Description |
| ---- | ---- |
| Status | Colored dot: green (flat+dark), orange (partial — flat or dark missing), red (none) |
| Frames | Frame count (bold) |
| Filter | Filter name (cyan bold) |
| Exposure | Exposure time (monospace bold) |
| Temp | Temperature or range |
| Date Range | Date+time range (monospace) |
| Flat | Linked flat set ID (cyan, clickable) or red ✗ if missing |
| Dark | Linked dark set ID (purple, clickable) or red ✗ if missing |
| FWHM | Average FWHM (green/yellow/red by quality) |
| Ecc | Average eccentricity |
| SNR | Average SNR dB |

Lights do NOT have a Bias column — bias calibrates darks, not lights directly.

### Flats Table Columns

| Column | Description |
| ---- | ---- |
| Set | Set ID (cyan) + Master badge |
| Filter | Filter name |
| Exposure | Flat exposure time |
| Temp | Temperature |
| Bin | Binning |
| Frames | Frame count |
| Date Range | Date+time |
| DarkFlat | Linked darkflat set ID (purple, clickable) |
| G | Gain match badge (value, green/yellow) |
| B | Binning match badge |
| O | Offset match badge |
| Lights | Count of light frames using this flat (green, clickable) |

### Darks Table Columns

| Column | Description |
| ---- | ---- |
| Set | Set ID (purple) + Master badge |
| Exposure | Dark exposure time |
| Temp | Temperature |
| Bin | Binning |
| Frames | Frame count |
| Date Range | Date+time |
| Bias | Linked bias set ID (orange, clickable) |
| G | Gain match |
| B | Binning match |
| O | Offset match |
| Lights | Count of light frames using this dark |

### Bias Table Columns

| Column | Description |
| ---- | ---- |
| Set | Set ID (orange) + Master badge |
| Bin | Binning |
| Frames | Frame count |
| Date Range | Date+time |
| G | Gain match |
| B | Binning match |
| O | Offset match |
| Lights | Count of light frames (indirect, through darks) |

No "Darks" column in Bias table.

### Interactions

- **Click light row**: highlights the row + its linked flat row + dark row (all glow blue). Auto-expands collapsed sections if needed.
- **Click set ID in any table**: scrolls to and highlights that row in the respective section.
- **Click "Lights" count in cal tables**: highlights all light rows using that set.
- **Sort by any column**: click column header to sort asc/desc.
- **Collapse/expand sections**: click section header.

### Status Logic

- **Green dot**: has flat AND has dark
- **Orange dot**: has flat OR dark, but not both
- **Red dot**: has neither flat nor dark

### Match Badges (G/B/O)

Show the actual value from the calibration set. Color:

- Green: matches the camera's value (from the light frames)
- Yellow: does not match

### Toolbar

Same as current — Find Calibration button (purple) with count annotation underneath.

### Data Derivation

Reuse the existing grouping logic: frames grouped by (filter, exposure, flat_set_id, dark_set_id). Each unique combination becomes one light row. Flat/Dark/Bias sets are deduplicated globally.

### What Changes

| Current | New |
| ---- | ---- |
| CalibrationGraphView (SVG node graph) | CalibrationTableView (4 sortable tables) |
| Absolute positioned nodes + SVG paths | Standard HTML tables with sticky headers |
| Doesn't scale for large datasets | Handles any number of frames/sets |

### What Stays

- Left panel tree navigation (By Night / By Camera)
- ManualCalibrationModal for assignment
- Split / Create Set in left panel footer
- Blackholed frames section
- Find Calibration button in toolbar
- Data model (CalibrationHierarchyView from backend)

## Files

| File | Change |
| ---- | ---- |
| New: `src/components/calibration/CalibrationTableView.tsx` | New table-based component |
| Modify: `src/components/CalibrationHierarchyView.tsx` | Replace CalibrationGraphView with CalibrationTableView |
| Delete: `src/components/calibration/CalibrationGraphView.tsx` | Remove graph view |
