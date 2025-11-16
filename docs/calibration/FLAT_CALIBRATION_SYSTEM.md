# Complete Calibration System - Technical Documentation

**Date:** 2025-01-16
**Status:** Design & Implementation
**Updated:** 2025-01-16 (Added Dark/Bias auto-creation)

---

## Overview

The Complete Calibration System provides dynamic, on-demand creation of calibration frame sets with time-based clustering. All calibration types (Flats, Darks, Bias, DarkFlats) use **time-based burst detection** and are created **on-demand** when needed for processing light frames.

### Complete Calibration Hierarchy

For each light frame, the system builds this calibration hierarchy:

```
Light Frame
├── Flat Set (for light, by filter)
│   ├── Dark Set (for flat) [preferred]
│   │   └── Bias Set (for dark)
│   └── Bias Set (for flat) [fallback if no dark available]
└── Dark Set (for light)
    └── Bias Set (for dark)
```

**Key Principles:**
1. **Time-Based Clustering:** All calibration types are grouped by temporal proximity (frames taken in the same burst/session)
2. **On-Demand Creation:** If no matching set exists, the system automatically creates one from individual frames
3. **Fallback Logic:** Flat → Dark (preferred) → Bias (if no dark found)
4. **Temporal Validity:** Calibration sets have max age limits (Flats: varies by pattern, Darks/Bias: 30 days)

---

## Calibration Type Characteristics

| Aspect | Flats | Darks | Bias | DarkFlats |
|--------|-------|-------|------|-----------|
| **Frequency** | Every session or weekly | Yearly/half-yearly | Yearly/half-yearly | During flat sessions |
| **Clustering** | Time-based bursts | Time-based bursts | Time-based bursts | Time-based bursts |
| **Cluster Threshold** | 30 minutes | 30 minutes | 30 minutes | 30 minutes |
| **Max Age** | Configurable (default 30 days) | 30 days | 30 days | 30 days |
| **Reusability** | Session-dependent | High - same camera settings | High - same camera settings | Medium |
| **Creation** | On-demand, pattern-based | On-demand, parameter-based | On-demand, parameter-based | On-demand |
| **Matching** | Parameter + date + pattern + temp | Parameter + date + temp | Parameter + date + temp | Parameter + date |
| **Special Logic** | Pattern selection (before/after session) | None | None | None |

**Important:** Do NOT cluster calibration frames from different years together. Time-based clustering ensures frames from the same acquisition session are grouped, but the max_age setting prevents using very old calibration data.

---

## User Workflows & Patterns

### Pattern 1: Flats Before Session
**Typical workflow:**
- User sets up equipment
- Takes flat frames (e.g., 30 flats per filter)
- Begins light frame acquisition
- Session ends

**Matching logic:** Select flat group taken BEFORE first light frame (closest to session start)

---

### Pattern 2: Flats After Session
**Typical workflow:**
- User begins light frame acquisition
- Session ends (dawn approaching)
- Takes flat frames before teardown
- Packs up equipment

**Matching logic:** Select flat group taken AFTER last light frame (closest to session end)

---

### Pattern 3: Flats Before Filter Changes
**Typical workflow:**
- Takes flats for Filter A (e.g., Ha)
- Images with Filter A (100 frames)
- Changes to Filter B (e.g., Oiii)
- Takes flats for Filter B
- Images with Filter B (80 frames)

**Matching logic:**
1. Detect filter change timestamps in light frames
2. For each filter period, find nearest flat group BEFORE the period
3. Handle mid-session flat groups

---

### Pattern 4: Long-Term Flats
**Typical workflow:**
- User takes comprehensive flat library once per month
- Reuses same flats for multiple sessions
- Acceptable for stable setups (no optical changes)

**Matching logic:** Select oldest valid flat group within max_age setting (prioritizes stability)

---

### Pattern 5: Manual Selection
**Typical workflow:**
- User has mixed approach
- Wants full control over which flats to use
- May use different strategies per filter

**Matching logic:** System presents available flat groups; user selects per filter

---

## Flat Group Detection Algorithm

### Concept
Flat frames are typically captured in **bursts** - consecutive exposures with minimal time gaps (seconds to minutes). The system clusters flat frames by time proximity to identify these natural groupings.

### Algorithm: Time-Based Clustering

```
Input:
  - Matching parameters (camera, filter, binning, gain, focal_length)
  - Time clustering threshold (default: 30 minutes)
  - Optional date range

Process:
1. Query all flat frames with exact parameter match
2. Sort frames by date_obs (chronological order)
3. Iterate through frames:
   - If time_gap(current, previous) <= threshold:
     - Add to current cluster
   - Else:
     - Close current cluster, start new cluster
4. For each cluster, calculate:
   - Frame count
   - Start time (first frame)
   - End time (last frame)
   - Average temperature (if available)
   - Frame IDs

Output: Vec<FlatGroup>
```

### Example

**Flat frames in database:**
```
Filter: Ha, Camera: ASI2600MM, Binning: 1x1

Frame 1: 2025-03-15 20:00:00  }
Frame 2: 2025-03-15 20:00:30  } Group A (before session)
Frame 3: 2025-03-15 20:01:00  } 30 frames, avg temp: -10°C
...
Frame 30: 2025-03-15 20:15:00 }

[3 hour gap - light frames taken here]

Frame 31: 2025-03-16 03:00:00 }
Frame 32: 2025-03-16 03:00:30 } Group B (after session)
...                            } 25 frames, avg temp: -12°C
Frame 55: 2025-03-16 03:12:00 }
```

**Result:** 2 flat groups detected

---

## Matching Parameters

### Required Exact Matches
These parameters **MUST** match exactly between flat frames and light frames:

| Parameter | Field | Notes |
|-----------|-------|-------|
| **Camera** | `instrume` | Different cameras = different optical paths |
| **Filter** | `filter` | Must match exactly; null = no filter (OK) |
| **Binning** | `binning` | Different binning = different pixel patterns |
| **Gain** | `gain` | Affects sensor response |
| **Focal Length** | `focallen` | Different focal length = different vignetting |

### Optional Proximity Matches
These parameters are considered for **ranking** but not required:

| Parameter | Weight | Logic |
|-----------|--------|-------|
| **Temperature** | Configurable (0.0-1.0) | Closer temp = bonus score |
| **Date** | Primary ranking | Closer date = higher priority |

---

## Calibration Set Creation Strategy

### Dynamic On-Demand Creation

Unlike Darks/Bias (pre-created and stored), flat calibration sets are created **dynamically** when finding calibration for a frame set:

1. **Analyze Light Frames**
   - Extract unique filters used
   - Get date range of imaging session
   - Identify camera parameters

2. **Detect Available Flat Groups**
   - For each filter, find matching flat groups
   - Apply max_age filter (setting-based)
   - Rank by date proximity and temperature

3. **Apply Pattern Selection**
   - Use frame set's pattern preference
   - Auto-select appropriate flat group
   - OR present options for manual selection

4. **Create Calibration Sets**
   - Create `calibration_set` entry with imagetyp='Flat'
   - Link flat frames to set via `calibration_set_frames`
   - Store metadata (date range, avg temp, frame count)

5. **Link to Light Frames**
   - Create `calibration_set_to_frames` links
   - Store match quality and warnings
   - Each light frame → appropriate flat set for its filter

### Set Reusability

Once created, flat calibration sets are **reusable**:
- If another frame set needs same flats (same filter, date range), reuse existing set
- Prevents duplicate sets in database
- Check for existing set before creating new one

---

## Dark and Bias Calibration

### Time-Based Clustering for Darks/Bias

Like Flats, Dark and Bias frames are shot in **bursts** (consecutive exposures) and are grouped using time-based clustering:

**Typical workflow:**
- User takes 50-100 dark frames with same exposure time
- All frames captured within 30 minutes are clustered into one group
- Frames taken months later (even same camera/settings) form separate group

**Key Differences from Flats:**
- **Frequency:** Darks/Bias are typically taken yearly or half-yearly (not every session)
- **Max Age:** 30 days (shorter than potential reuse period)
- **No Pattern Selection:** Darks/Bias don't have user patterns like Flats
- **Exptime Matching:** Darks must match light frame exposure time; Bias frames are exptime-independent

**Important Rule:** Never cluster frames from different years together. Time-based clustering with max_age setting ensures only recent calibration data is used.

### Dark Calibration Set Creation

**On-Demand Process:**
1. **Need Detected:** Light frame or Flat set needs Dark calibration
2. **Search Existing Sets:** Look for matching Dark set (gain, offset, binning, instrume, exptime, within max_age)
3. **If Not Found:**
   - Query individual Dark frames matching parameters
   - Cluster by time proximity (30-minute threshold)
   - Select best group (most recent, closest temperature)
   - Create `calibration_set` entry with imagetyp='Dark'
   - Link all frames in group to the set
4. **Return Set ID:** Use newly created (or existing) set for calibration

**Matching Parameters (Exact):**
- Camera (`instrume`)
- Binning (`binning`)
- Gain (`gain`)
- Offset (`offset`)
- **Exposure Time (`exptime`)** - Must match light frame
- Focal Length (`focallen`) - Optional

### Bias Calibration Set Creation

**On-Demand Process:**
Similar to Dark, but **no exptime matching** (bias frames are taken with minimal exposure).

1. **Need Detected:** Dark set or Flat set needs Bias calibration
2. **Search Existing Sets:** Look for matching Bias set (gain, offset, binning, instrume, within max_age)
3. **If Not Found:**
   - Query individual Bias frames matching parameters
   - Cluster by time proximity (30-minute threshold)
   - Select best group (most recent, closest temperature)
   - Create `calibration_set` entry with imagetyp='Bias'
   - Link all frames in group to the set
4. **Return Set ID:** Use newly created (or existing) set

**Matching Parameters (Exact):**
- Camera (`instrume`)
- Binning (`binning`)
- Gain (`gain`)
- Offset (`offset`)
- Focal Length (`focallen`) - Optional

### Fallback Logic for Flats

When building calibration for a Flat set:

1. **Preferred:** Find/create Dark set matching flat parameters
   - Then find/create Bias set for that Dark
2. **Fallback:** If no Dark frames available, link Bias directly to Flat
   - Bias set must match flat's gain, offset, binning, instrume

This fallback ensures Flats always have some level of calibration even without matching Darks.

---

## Settings & Configuration

### User-Configurable Settings

| Setting Key | Default | Description |
|-------------|---------|-------------|
| `flats.max_age_days` | 30 | Maximum age of flats to consider valid |
| `flats.time_cluster_minutes` | 30 | Time threshold for clustering flat frames |
| `darks.max_age_days` | 30 | Maximum age of darks to consider valid |
| `darks.time_cluster_minutes` | 30 | Time threshold for clustering dark frames |
| `bias.max_age_days` | 30 | Maximum age of bias to consider valid |
| `bias.time_cluster_minutes` | 30 | Time threshold for clustering bias frames |
| `temperature.match_weight` | 0.3 | Weight for temperature proximity (0.0-1.0) |

### Frame Set Pattern Storage

Each frame set stores its flat pattern preference:
- **Column:** `frames_set.flat_pattern`
- **Values:**
  - `before_session`
  - `after_session`
  - `before_filter_change`
  - `long_term`
  - `manual`
  - `null` (not set - triggers questionnaire)

---

## Database Schema

### New/Modified Tables

#### `frames_set` (modified)
```sql
ALTER TABLE frames_set ADD COLUMN flat_pattern TEXT DEFAULT NULL;
```

Stores user's pattern choice per frame set.

#### `calibration_set` (existing, used for flats)
```sql
-- Flat sets use same table as Dark/Bias
imagetyp = 'Flat'
```

Flat sets have same structure as Dark sets:
- Parameters: filter, binning, gain, instrume, focallen
- Metadata: date range, avg temperature
- Linked frames via `calibration_set_frames`

---

## Warning System

### Age Warnings

If flat group age exceeds threshold, generate warning:
- **Threshold:** Configurable per user (e.g., 30 days)
- **Warning Type:** `date`
- **Message:** "Flat calibration is X days old (captured on DATE)"

### Temperature Warnings

If flat temperature differs significantly from light frame:
- **Threshold:** Configurable (e.g., ±5°C)
- **Warning Type:** `temperature`
- **Message:** "Flat temperature (-10°C) differs from light frame temperature (-15°C)"

### Missing Calibration

If no flat group found within max_age:
- **Type:** Missing calibration
- **Message:** "No Flat calibration found for filter 'Ha' within 30 days"

---

## Filter Change Detection

### Algorithm

For pattern `before_filter_change`, detect filter transitions:

```
Input: Vec<Frame> (light frames, chronological)

Process:
1. Group consecutive frames by filter value
2. Identify filter transition points:
   - Frame N: filter='Ha'
   - Frame N+1: filter='Oiii' ← transition detected
3. Create filter periods:
   [
     { filter: 'Ha', start: frame_1.date_obs, end: frame_N.date_obs },
     { filter: 'Oiii', start: frame_N+1.date_obs, end: frame_M.date_obs }
   ]

Output: Vec<FilterPeriod>
```

### Flat Matching for Filter Changes

For each filter period:
1. Find flat groups with matching filter
2. Prioritize flat group taken BEFORE period start
3. If no flats before, use flats AFTER period end
4. Apply normal ranking (date proximity, temperature)

---

## UI Workflow

### Questionnaire Modal

**Trigger:** User clicks "Find Calibration" on frame set with `flat_pattern = null`

**Modal Content:**
```
How did you take flats for this imaging session?

○ Before session started
○ After session ended
○ Before each filter change
○ Long-term flats (reuse over time)
○ Let me choose manually

☑ Remember this choice for this frame set

[Cancel] [Continue]
```

### Manual Selection Modal

**Trigger:** User selects "manual" pattern

**Modal Content:**
```
Select flat groups for each filter:

Filter: Ha
┌─────────────────────────────────────────────────────┐
│ • 50 flats - Mar 15, 8:00 PM                       │ ← Selected
│   (2 hours before session)                          │
│   Avg temp: -10°C                                   │
├─────────────────────────────────────────────────────┤
│ ○ 30 flats - Mar 16, 3:00 AM                       │
│   (1 hour after session)                            │
│   Avg temp: -12°C                                   │
├─────────────────────────────────────────────────────┤
│ ○ 40 flats - Mar 10, 9:00 PM                       │
│   (5 days earlier) ⚠️                               │
│   Avg temp: -10°C                                   │
└─────────────────────────────────────────────────────┘

Filter: Oiii
[Similar selection for Oiii filter]

[Cancel] [Apply Selections]
```

### Results Display

**Enhanced ProcessingStats:**
```
Calibration Status:
  ✅ Full calibration: 55 frames
  ⚠️  Partial calibration: 5 frames
  ❌ No calibration: 1 frame

Flat Sets Linked:
  • Ha: 50 flats from Mar 15, 8:00 PM
  • Oiii: 30 flats from Mar 16, 3:00 AM

Warnings (2):
  ⚠️ Oiii flats are 5 days old
  ⚠️ 3 frames have temperature mismatch
```

---

## Implementation Phases

### Phase 1: Core Algorithm (Backend)
- Flat group detection (time clustering)
- Flat matching logic
- Pattern-based selection
- Filter change detection

### Phase 2: Settings Integration
- Add settings keys and defaults
- Pattern storage in frames_set table

### Phase 3: Tauri Commands
- Enhance `find_calibration_for_frame_set`
- Add `get_flat_group_options_for_frame_set`

### Phase 4: Frontend UI
- Pattern selection modal
- Manual selection modal
- Enhanced results display

### Phase 5: Testing & Polish
- Test all patterns with real data
- Edge case handling
- Performance optimization

---

## Edge Cases & Handling

### 1. No Flats Available
**Scenario:** No flat frames in database matching filter/camera
**Handling:**
- Add to `missing_calibration` list
- Show clear error in UI
- Suggest capturing flats

### 2. All Flats Too Old
**Scenario:** Flat groups exist but all exceed max_age
**Handling:**
- Show warning with age of nearest flats
- Offer to temporarily extend max_age
- OR proceed without flats (user choice)

### 3. Multiple Equally Valid Groups
**Scenario:** Two flat groups at same distance from session
**Handling:**
- Prioritize: larger group > better temperature > earlier timestamp
- In manual mode, show both options

### 4. Flats Taken During Session
**Scenario:** Flat group timestamp between first/last light frame
**Handling:**
- For `before_session` pattern: Use if taken before 50% of frames
- For `after_session` pattern: Use if taken after 50% of frames
- Otherwise, let user choose

### 5. Missing Filter in Flats
**Scenario:** Light frames use filter "Sii" but no flats exist for Sii
**Handling:**
- Mark as missing calibration
- Show warning: "No flats found for filter 'Sii'"
- Allow user to manually select compatible flats (optional)

### 6. Temperature Unavailable
**Scenario:** Flat frames don't have CCD_TEMP recorded
**Handling:**
- Skip temperature ranking
- Match by date only
- No temperature warnings

---

## Performance Considerations

### Query Optimization
- Index `frames` table on: `(imagetyp, instrume, filter, binning, gain, focallen)`
- Index `frames` table on: `date_obs` for time-based queries
- Limit date range queries to ±max_age_days from session

### Caching
- Cache detected flat groups during calibration finding
- Reuse groups if multiple light frames need same filter
- Cache pattern preference per frame set

### Batch Processing
- Process all light frames in frame set together
- Detect all flat groups once per filter
- Create calibration sets in batch

---

## Future Enhancements (Not in Current Scope)

### Auto-Detection of Pattern
- Analyze historical data to guess user's typical pattern
- Suggest pattern based on flat timing in database

### Smart Temperature Matching
- Consider camera model's temperature stability
- Weight temperature more heavily for uncooled cameras
- Ignore temperature for short sessions (same ambient temp)

### Flat Quality Metrics
- Analyze flat frame quality (dust donuts, gradients)
- Rank flat groups by quality score
- Warn about problematic flats

### Multi-Night Flat Libraries
- Support "flat library" concept (curated collection)
- Allow marking flat groups as "gold standard"
- Prefer library flats over session-specific flats

---

## References

- Phase 1-6 Documentation: `PHASE1_COMPLETE.md` through `PHASE6_COMPLETE.md`
- Implementation Plan: `IMPLEMENTATION_PLAN.md`
- Database Schema: `src-tauri/src/db/schema.rs`
- Matching Algorithm: `src-tauri/src/calibration/matcher.rs`
- Hierarchy Builder: `src-tauri/src/calibration/hierarchy.rs`
