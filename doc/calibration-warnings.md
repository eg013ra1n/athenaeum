# Calibration Warning System Documentation

## Overview

The calibration warning system in Athenaeum tracks two primary types of issues across all calibration types (Flat, Dark, Bias, DarkFlat):

1. **Warnings** (Yellow ⚠) - Calibration found but outside ideal parameters
2. **Missing** (Red ✖) - No calibration found at all

## Warning Types

### 1. Temperature Warnings

**When triggered:**
- CCD temperature differs between your light/flat frame and the calibration frame
- Difference exceeds the configured threshold (default: 2.0°C)
- Only applies when parameter matching mode is set to "Warning"

**Example:**
- Your light frame: -10°C
- Calibration dark frame: -5°C
- Difference: 5°C > 2°C threshold → Warning shown

**Applies to:**
- Lights → Dark (default: enabled, 2°C threshold)
- Lights → Bias (default: enabled, 2°C threshold)
- Flats → DarkFlat (default: enabled, 2°C threshold)
- Flats → Dark (default: enabled, 2°C threshold)
- Flats → Bias (default: enabled, 2°C threshold)

**Does NOT apply to:**
- Lights → Flat (temperature is ignored by default)

### 2. Date Warnings

**When triggered:**
- Calibration frames are older than recommended age
- Hardcoded thresholds per type:
  - **Flats**: > 30 days old
  - **Darks/DarkFlats**: > 365 days old
  - **Bias**: No date warnings (bias frames don't degrade)

**Example:**
- Your light frame taken: Nov 26, 2024
- Calibration flat taken: Sep 1, 2024
- Age: 86 days > 30 day threshold → Warning shown

## Where Warnings Are Displayed

### 1. Session View in Frame Set Details

**Location:** `src/pages/FrameSetDetail.tsx`

**When displayed:**
- When you expand a session in the Sessions tab
- Calibration status loads for all LIGHT frames
- Each frame shows calibration badges in the "Calibration" column

**Badge states:**
- **Green ✓** - Calibration found, no warnings
- **Yellow ⚠** - Calibration found WITH warnings
- **Red ✖** - Calibration missing

**Example display:**
```
[Flat ⚠] [Dark ✓] [Bias ✓]
```

**Important:**
- Shows one badge per calibration type (Flat, Dark, Bias)
- Only shown for LIGHT frames (not for calibration frames themselves)
- Loads on-demand when session is expanded

### 2. Calibration Tab

**Location:** `src/components/CalibrationGroupsView.tsx`

**Detailed warning messages shown:**
- Grouped by calibration type (Flat Groups, Dark Groups, Bias Groups)
- Each calibration set card can show multiple warnings:
  - "Dark temperature differs by 5.1°C (threshold: 2.0°C)"
  - "Flat calibration is 45 days old (>30 days recommended)"
- Warning icon (⚠) shown on groups with any warnings

## Backend Implementation

### Warning Generation Flow

**File:** `src-tauri/src/calibration/configurable_matcher.rs`

#### Parameter Checking (Lines 90-141)

```rust
pub struct ParameterCheckResult {
    pub matches: bool,
    pub warning: bool,
    pub warning_message: Option<String>,
    pub skip_matching: bool,
}
```

**Temperature Warning Detection:**
- Function: `check_float_param()`
- When `MatchMode::Warning` is configured for `ccd_temp`:
  - Compares frame temperature vs calibration set temperature
  - If difference > `warning_threshold` (default 2.0°C), creates warning
  - Still considers it a match (unlike Exact mode)
  - Warning message: `"ccd_temp differs by X.X (threshold: Y.Y)"`

**Date Warning Detection (Lines 372-385):**
- Function: `check_date_warning_days()`
- Compares frame date vs calibration set date range
- Uses closest date (start or end) for calculation
- Returns boolean indicating if warning threshold exceeded

### Data Structures

**File:** `src-tauri/src/models.rs`

```rust
// Lines 519-524
pub struct CalibrationWarning {
    pub warning_type: String,       // "date" or "temperature"
    pub message: String,             // Human-readable warning
    pub calibration_type: String,    // "Dark", "Flat", "Bias", "DarkFlat"
    pub set_id: i64,                 // Which calibration set triggered warning
}

// Lines 485-498
pub struct FrameCalibrationStatus {
    pub frame_id: i64,
    pub has_flats: bool,
    pub has_darks: bool,
    pub has_bias: bool,
    pub has_darkflats: bool,
    pub flats_warning: bool,         // ANY warning for flats
    pub darks_warning: bool,         // ANY warning for darks
    pub bias_warning: bool,          // ANY warning for bias
    pub flat_set_id: Option<i64>,
    pub dark_set_id: Option<i64>,
    pub bias_set_id: Option<i64>,
    pub darkflat_set_id: Option<i64>,
}
```

### Database Storage

**File:** `src-tauri/src/db/calibration_links.rs`

**Table:** `calibration_set_to_frames`

```sql
CREATE TABLE calibration_set_to_frames (
    source_id INTEGER,
    source_type TEXT,
    calibration_set_id INTEGER,
    calibration_type TEXT,
    matched_at TEXT,
    match_score REAL,
    date_warning INTEGER,        -- 0 or 1
    temp_warning INTEGER         -- 0 or 1
)
```

**Retrieval for Frame Status (Lines 93-137):**

```rust
pub fn get_frame_calibration_status(conn: &Connection, frame_id: i64)
    -> Result<FrameCalibrationStatus> {
    // For each calibration link:
    match link.calibration_type.as_str() {
        "Flat" => {
            status.has_flats = true;
            status.flats_warning = link.date_warning || link.temp_warning;
        }
        "Dark" => {
            status.has_darks = true;
            status.darks_warning = link.date_warning || link.temp_warning;
        }
        "Bias" => {
            status.has_bias = true;
            status.bias_warning = link.date_warning || link.temp_warning;
        }
    }
}
```

## Frontend Implementation

### CalibrationStatusBadges Component

**File:** `src/components/CalibrationStatusBadges.tsx`

**Badge States:**
1. **Success** (green) - Calibration found, no warnings
2. **Warning** (yellow) - Calibration found, has warnings
3. **Missing** (red) - Calibration not found
4. **Loading** - Fetching status

**Display Logic (Lines 32-79):**

```typescript
// Flat badge
if (status.has_flats) {
    badge.status = status.flats_warning ? 'warning' : 'success';
    badge.icon = status.flats_warning ? AlertTriangle : CheckCircle;
    badge.tooltip = status.flats_warning
        ? 'Flat calibration found (with warnings)'
        : 'Flat calibration found';
} else {
    badge.status = 'missing';
    badge.icon = XCircle;
    badge.tooltip = 'Flat calibration missing';
}
```

### Session View Display

**File:** `src/pages/FrameSetDetail.tsx`

**Loading Calibration Status (Lines 147-171):**

```typescript
const loadCalibrationStatus = async (frameId: number) => {
    const status = await invoke<FrameCalibrationStatus>(
        'get_frame_status',
        { frameId }
    );
    setCalibrationStatuses(prev => new Map(prev).set(frameId, status));
};

// Load when session is expanded
const toggleSession = (sessionId, sessionFrames) => {
    if (!newSet.has(sessionId)) {
        newSet.add(sessionId);
        // Load calibration status for all LIGHT frames
        loadCalibrationStatusForSession(sessionFrames);
    }
};
```

**Display in Table (Lines 822-890):**

```typescript
<th>Calibration</th>
// ...
<td>
    {isLightFrame && (
        <CalibrationStatusBadges
            status={calibrationStatus}
            loading={isLoadingStatus}
        />
    )}
</td>
```

## All Possible Warning Cases

### Temperature Warnings

| Source | Calibration | Default Config | Default Threshold |
|--------|-------------|---------------|------------------|
| Lights | Dark | Warning enabled | 2.0°C |
| Lights | Bias | Warning enabled | 2.0°C |
| Lights | Flat | Ignored | N/A |
| Flats | DarkFlat | Warning enabled | 2.0°C |
| Flats | Dark | Warning enabled | 2.0°C |
| Flats | Bias | Warning enabled | 2.0°C |
| Darks | Bias | Warning enabled | 2.0°C |

### Date Warnings

| Calibration Type | Threshold | Warning Message |
|-----------------|-----------|-----------------|
| Flat | 30 days | "Flat calibration is X days old (>30 days recommended)" |
| Dark | 365 days | "Dark calibration is X days old (>365 days recommended)" |
| DarkFlat | 365 days | "DarkFlat is X days old" |
| Bias | Never | N/A (Bias frames don't degrade) |

### Missing Calibration States

**These are NOT warnings but distinct red "missing" states:**

1. No Flat found for Light frame
2. No Dark found for Light frame
3. No Bias found for Light/Dark/Flat frame (when Bias optimization enabled)
4. No DarkFlat found for Flat frame

## Configuration

### Temperature Warning Thresholds

**Location:** `CalibrationMatchingConfig` in settings

**Per-Parameter Configuration:**

```rust
// Example for Lights → Dark temperature matching
config.lights.dark.ccd_temp = ParameterConfig {
    mode: MatchMode::Warning,
    warning_threshold: Some(2.0),  // 2.0°C threshold
}
```

**UI Configuration:**
- Settings → Calibration Matching tab
- Configure each source→calibration pairing independently
- Set matching mode: Exact, Warning, or Ignore
- Set warning threshold for Warning mode

### Date Warning Thresholds

**Location:** Hardcoded in `src-tauri/src/calibration/configurable_matcher.rs:372-385`

**Current Implementation:**

```rust
fn check_date_warning_days(
    frame_date: &str,
    set_start: &str,
    set_end: &str,
    calibration_type: &str,
) -> Result<bool> {
    let threshold_days = match calibration_type {
        "Flat" => 30,
        "Dark" | "DarkFlat" => 365,
        "Bias" => return Ok(false),  // No date warnings for Bias
        _ => 30,
    };
    // Compare dates...
}
```

**Note:** Date thresholds are NOT currently user-configurable through the UI.

### Matching Modes

**Three modes control warning behavior:**

1. **Exact**
   - Parameter must match exactly (within small tolerance for floats)
   - Mismatch = no calibration found
   - No warning generated

2. **Warning**
   - Parameter can differ up to threshold
   - Match still succeeds
   - Warning generated if threshold exceeded
   - Stored in database as `temp_warning = 1` or `date_warning = 1`

3. **Ignore**
   - Parameter not checked at all
   - No warnings possible for this parameter

## Complete Data Flow

### Warning Generation Flow

```
1. User triggers calibration finder
   └─> find_calibration_for_frame_set (commands/calibration.rs)
       └─> process_frame_set (calibration/processor.rs)
           └─> build_complete_hierarchy (calibration/hierarchy.rs)
               └─> find_dark_for_light (calibration/configurable_matcher.rs)
                   ├─> check_calibration_match()
                   │   ├─> check_float_param(ccd_temp)
                   │   │   └─> Returns ParameterCheckResult with warning=true
                   │   └─> Returns ConfigMatchResult with warnings[]
                   ├─> check_date_warning_days()
                   │   └─> Returns bool (date_warning)
                   └─> Creates CalibrationCandidate
                       ├─> date_warning: true/false
                       └─> temp_warning: true/false

2. Warnings packaged into CalibrationHierarchy
   └─> warnings: Vec<CalibrationWarning>
       ├─> warning_type: "date" | "temperature"
       ├─> message: "Dark temperature differs by 5.1°C"
       ├─> calibration_type: "Dark"
       └─> set_id: 123

3. Hierarchy stored in database
   └─> insert_calibration_link (db/calibration_links.rs)
       └─> INSERT INTO calibration_set_to_frames
           ├─> date_warning: 1 or 0
           └─> temp_warning: 1 or 0
```

### Warning Display Flow

```
1. User expands session in FrameSetDetail
   └─> toggleSession() called
       └─> loadCalibrationStatusForSession()
           └─> For each LIGHT frame:
               └─> invoke('get_frame_status', { frameId })
                   └─> get_frame_calibration_status (db/calibration_links.rs)
                       ├─> Queries calibration_set_to_frames
                       └─> Returns FrameCalibrationStatus
                           ├─> flats_warning = link.date_warning || link.temp_warning
                           ├─> darks_warning = link.date_warning || link.temp_warning
                           └─> bias_warning = link.date_warning || link.temp_warning

2. Status passed to CalibrationStatusBadges component
   └─> Renders badges based on:
       ├─> has_flats + flats_warning → Yellow badge with AlertTriangle
       ├─> has_darks + darks_warning → Yellow badge with AlertTriangle
       └─> has_bias + bias_warning → Yellow badge with AlertTriangle

3. User views Calibration tab
   └─> invoke('get_frame_set_calibration_groups')
       └─> get_calibration_groups_for_frame_set (db/calibration_links.rs)
           └─> get_calibration_warnings_for_group()
               ├─> Loads current config
               ├─> Checks if warnings enabled
               └─> Returns detailed CalibrationWarning[] per type
                   └─> Displayed in CalibrationGroupsView
                       └─> Shows warning icon + message per calibration set
```

## Key Insights

1. **Two-Level Warning System**
   - Boolean flags in `FrameCalibrationStatus` for quick badge display in session view
   - Detailed `CalibrationWarning` objects for comprehensive information in calibration tab

2. **Configuration-Driven**
   - Warnings are generated based on `MatchMode::Warning` settings
   - Display filters warnings based on current config (can hide previously generated warnings)

3. **Persistence**
   - Warnings stored as booleans in database when calibration is matched
   - Detailed messages reconstructed on retrieval based on current config

4. **Warning vs Missing**
   - **Warning**: Calibration found but outside ideal parameters (yellow state)
   - **Missing**: No calibration found at all (red state, more serious)

5. **Selectivity**
   - Only LIGHT frames show calibration status in session view
   - Warnings displayed contextually (badges in session view, detailed messages in calibration tab)

6. **Warnings Don't Block Calibration**
   - A frame with warnings still has calibration applied
   - Warnings indicate the calibration is outside ideal parameters, not that it failed

## Related Files

### Backend (Rust)
- `src-tauri/src/calibration/configurable_matcher.rs` - Warning generation logic
- `src-tauri/src/calibration/hierarchy.rs` - Warning packaging and propagation
- `src-tauri/src/calibration/config.rs` - Configuration data structures
- `src-tauri/src/db/calibration_links.rs` - Database storage and retrieval
- `src-tauri/src/models.rs` - Data structure definitions
- `src-tauri/src/commands/calibration.rs` - Tauri commands for calibration

### Frontend (React/TypeScript)
- `src/components/CalibrationStatusBadges.tsx` - Badge display component
- `src/pages/FrameSetDetail.tsx` - Session view with calibration status
- `src/components/CalibrationGroupsView.tsx` - Detailed calibration warnings display
- `src/types/models.ts` - TypeScript type definitions

## Future Improvements

Potential areas for enhancement:

1. **User-Configurable Date Thresholds**
   - Currently hardcoded in Rust
   - Could be added to `CalibrationMatchingConfig.warnings`

2. **Warning History**
   - Track when warnings were generated
   - Show if warnings have changed over time

3. **Batch Warning Dismissal**
   - Allow users to acknowledge warnings
   - Hide acknowledged warnings from display

4. **Warning Severity Levels**
   - Minor vs Major warnings
   - Different thresholds for different severity levels
