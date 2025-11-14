# Frame Sets Architecture

## Overview

Frame sets are collections of related astronomical image frames organized hierarchically by observation night and instrument. This document describes the database schema, relationships, and structural hierarchy.

## Database Schema

### Core Tables

#### `frames_set`
Top-level container for a collection of related frames.

```sql
CREATE TABLE frames_set (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT,
    is_custom INTEGER NOT NULL DEFAULT 0,
    date_obs_start TEXT,
    date_obs_end TEXT,
    objctra TEXT,
    objctdec TEXT,
    total_exp_time REAL
);
```

**Fields:**
- `id`: Primary key
- `name`: User-defined or auto-generated name
- `is_custom`: Boolean (0=auto-generated, 1=custom/merged/split)
- `date_obs_start`: ISO 8601 timestamp of earliest frame observation
- `date_obs_end`: ISO 8601 timestamp of latest frame observation
- `objctra`: Average right ascension in sexagesimal format (HH:MM:SS.S)
- `objctdec`: Average declination in sexagesimal format (±DD:MM:SS.S)
- `total_exp_time`: Total exposure time in seconds

**Note:** The `project_id` field was removed as frame sets now exist independently across projects.

#### `imaging_nights`
Groups sessions by observation night.

```sql
CREATE TABLE imaging_nights (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    frames_set_id INTEGER NOT NULL,
    start_time TEXT NOT NULL,
    end_time TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE
);
```

**Fields:**
- `id`: Primary key
- `frames_set_id`: Parent frame set
- `start_time`: ISO 8601 start time of first frame in night
- `end_time`: ISO 8601 end time of last frame in night
- `created_at`: Creation timestamp

**Cascade Behavior:** Deleting a `frames_set` cascades to delete all associated `imaging_nights`.

#### `sessions`
Groups frames by instrument within an imaging night.

```sql
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    imaging_night_id INTEGER NOT NULL,
    instrume TEXT NOT NULL,
    frame_count INTEGER NOT NULL DEFAULT 0,
    total_exp_time REAL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (imaging_night_id) REFERENCES imaging_nights(id) ON DELETE CASCADE
);
```

**Fields:**
- `id`: Primary key
- `imaging_night_id`: Parent imaging night
- `instrume`: Camera/instrument name (from INSTRUME FITS header)
- `frame_count`: Number of frames in session
- `total_exp_time`: Total exposure time for session in seconds
- `created_at`: Creation timestamp

**Cascade Behavior:** Deleting an `imaging_nights` record cascades to delete all associated `sessions`.

#### `session_members`
Junction table linking frames to sessions.

```sql
CREATE TABLE session_members (
    session_id INTEGER NOT NULL,
    frame_id INTEGER NOT NULL,
    PRIMARY KEY (session_id, frame_id),
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE,
    FOREIGN KEY (frame_id) REFERENCES frames(id) ON DELETE CASCADE
);
```

**Fields:**
- `session_id`: Session reference
- `frame_id`: Frame reference

**Cascade Behavior:** Deleting a `sessions` record removes all associated `session_members` entries.

## Hierarchical Structure

```
frames_set (Collection)
  ├─ date_obs_start, date_obs_end
  ├─ objctra, objctdec (average coordinates)
  ├─ total_exp_time (sum of all frames)
  │
  └─> imaging_nights (1 to N)
       ├─ start_time, end_time
       │
       └─> sessions (1 to N)
            ├─ instrume (camera name)
            ├─ frame_count, total_exp_time
            │
            └─> session_members (N)
                 └─> frames (1)
```

## Relationships

### One-to-Many Relationships

1. **`frames_set` → `imaging_nights`**: One frame set can have multiple imaging nights
2. **`imaging_nights` → `sessions`**: One night can have multiple sessions (different instruments)
3. **`sessions` → `session_members`**: One session can have multiple frames

### Many-to-One Relationships

1. **`imaging_nights` → `frames_set`**: Many nights belong to one frame set
2. **`sessions` → `imaging_nights`**: Many sessions belong to one night
3. **`session_members` → `sessions`**: Many members belong to one session
4. **`session_members` → `frames`**: Many members reference frames (but each frame can only be in one session within a frame set)

## Frame Uniqueness

**Important:** Within a frame set, each frame ID should appear only once. The deduplication system ensures:
- No frame appears in multiple sessions within the same frame set
- Merge operations deduplicate frames across combined frame sets
- Split operations do not create duplicate frame references

## Cascade Deletion Chain

When a `frames_set` is deleted:

```
DELETE frames_set
  ↓ CASCADE
DELETE imaging_nights
  ↓ CASCADE
DELETE sessions
  ↓ CASCADE
DELETE session_members
```

The actual `frames` records are preserved as they exist independently and may be referenced by other frame sets or queries.

## Indexes

Key indexes for query performance:

- `idx_imaging_nights_frames_set_id` on `imaging_nights(frames_set_id)`
- `idx_sessions_imaging_night_id` on `sessions(imaging_night_id)`
- `idx_session_members_session_id` on `session_members(session_id)`
- `idx_session_members_frame_id` on `session_members(frame_id)`

## Schema Evolution

### Version History

**v2.0 (Current):**
- Removed `project_id` from `frames_set`
- Removed `date_obs` from `frames_set`
- Added `date_obs_start` to `frames_set`
- Added `date_obs_end` to `frames_set`

**v1.0 (Previous):**
- `frames_set` included `project_id` and single `date_obs`

### Migration Strategy

The schema includes migration logic that:
1. Checks for existence of old columns
2. Adds new columns if missing
3. Does not remove old columns (backwards compatible)
4. Applications should use new columns exclusively

## Performance Considerations

### Query Optimization

1. **Getting all frames for a set:** Use JOIN through the hierarchy:
   ```sql
   SELECT frame_id FROM session_members
   JOIN sessions ON session_id = sessions.id
   JOIN imaging_nights ON imaging_night_id = imaging_nights.id
   WHERE frames_set_id = ?
   ```

2. **Counting frames:** Use COUNT DISTINCT to avoid duplicates:
   ```sql
   SELECT COUNT(DISTINCT frame_id) FROM session_members
   JOIN sessions ON session_id = sessions.id
   JOIN imaging_nights ON imaging_night_id = imaging_nights.id
   WHERE frames_set_id = ?
   ```

3. **Getting frame set metadata:** Direct query on `frames_set` table (pre-calculated)

### Storage Efficiency

- Aggregated metadata (coordinates, exposure times, dates) stored at `frames_set` level
- Session-level metadata (frame counts, exposure times) stored at `sessions` level
- Avoids redundant recalculation on every query

## Design Rationale

### Why Three Levels?

1. **Frame Set Level:** Represents a logical collection (e.g., "M31 Widefield")
2. **Imaging Night Level:** Groups by observation session (important for weather, conditions)
3. **Session Level:** Groups by instrument (important for calibration, processing)

This hierarchy mirrors real-world astrophotography organization where:
- Multiple imaging nights contribute to a target
- Multiple instruments may be used on the same night
- Frames need to be organized for efficient processing

### Why Separate Tables?

- **Normalization:** Avoids data duplication
- **Flexibility:** Can query at any level of granularity
- **Performance:** Indexed relationships for fast lookups
- **Integrity:** Cascade deletion maintains referential integrity
