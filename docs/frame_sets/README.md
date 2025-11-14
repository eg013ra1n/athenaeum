# Frame Sets Documentation

## Overview

Frame sets are hierarchical collections of astronomical image frames organized by observation night and instrument. This documentation covers the complete implementation including architecture, algorithms, commands, and edge cases.

## Documentation Structure

### [architecture.md](./architecture.md)
**Database schema and structural design**

- Complete database schema with all tables and relationships
- Hierarchical structure (frame_set → imaging_night → session → frames)
- Cascade deletion behavior and referential integrity
- Schema evolution and migration strategy
- Performance considerations and query optimization

**Read this if you need to:**
- Understand the database structure
- Design new queries or operations
- Debug data integrity issues
- Plan schema migrations

---

### [algorithms.md](./algorithms.md)
**Core algorithms and computational logic**

- Night matching algorithm (calendar date + time overlap)
- Time range union calculation
- Merge algorithm with deduplication
- Split algorithm with validation
- Metadata calculation (spherical mean coordinates)
- Session detection (time-based grouping)
- Performance characteristics and complexity analysis

**Read this if you need to:**
- Understand how merge/split operations work
- Debug coordinate calculation issues
- Optimize performance
- Implement similar algorithms

---

### [commands.md](./commands.md)
**Complete API reference for Tauri commands**

- All frame set query commands
- Frame set creation commands (auto, custom, selection)
- Modification commands (rename, recalculate)
- Merge and split commands with full examples
- TypeScript interface definitions
- Error handling patterns
- Migration guide from v1.0 to v2.0

**Read this if you need to:**
- Call frame set commands from frontend
- Understand command parameters and return types
- Handle errors gracefully
- Migrate existing code to new schema

---

### [edge_cases.md](./edge_cases.md)
**Edge cases, boundary conditions, and special scenarios**

- Merge edge cases (empty sets, identical sets, partial overlap, etc.)
- Split edge cases (splitting all items, non-existent items, etc.)
- Metadata calculation edge cases (missing data, coordinate boundaries)
- Session detection edge cases (same timestamp, out of order)
- Deduplication edge cases (triple duplicates, no duplicates)
- Transaction and concurrency considerations
- Validation edge cases

**Read this if you need to:**
- Understand how the system handles unusual inputs
- Debug unexpected behavior
- Write comprehensive tests
- Ensure robustness

---

## Quick Start

### Creating Frame Sets

**Auto-generate from LIGHT frames:**
```typescript
const result = await invoke('auto_generate_frame_sets', { projectId: 1 });
console.log(`Created ${result.sets_created} sets`);
```

**Create custom from sessions:**
```typescript
const setId = await invoke('create_custom_frames_set', {
  name: "M31 October 2024",
  sessionIds: [12, 15, 18]
});
```

**Create from map selection:**
```typescript
const setId = await invoke('create_frame_set_from_selection', {
  name: "M31 Core",
  frameIds: [101, 102, 103]
});
```

### Merging Frame Sets

**Drag and drop merge:**
```typescript
const merged = await invoke('merge_frame_sets', {
  sourceId: 42,  // Dragged set (will be deleted)
  targetId: 43   // Drop target (will be updated)
});
```

### Splitting Frame Sets

**Check if split is valid:**
```typescript
const canSplit = await invoke('can_split', {
  sourceSetId: 42,
  selection: { type: "nights", ids: [10, 11] }
});
```

**Perform split:**
```typescript
const newSet = await invoke('split_frame_set', {
  sourceSetId: 42,
  selection: { type: "nights", ids: [10, 11] },
  newName: "M31 Widefield - Split 1"
});
```

## Key Concepts

### Hierarchy

```
frames_set
  ├─ date_obs_start, date_obs_end
  ├─ objctra, objctdec (average coordinates)
  ├─ total_exp_time (sum)
  │
  └─> imaging_nights (1 to N)
       ├─ start_time, end_time
       │
       └─> sessions (1 to N)
            ├─ instrume (camera)
            ├─ frame_count, total_exp_time
            │
            └─> session_members (N)
                 └─> frames (1)
```

### Night Matching

Two nights match if **BOTH**:
1. Same calendar date (accounting for multi-day observations)
2. Overlapping time ranges

Example:
```
Night A: 2024-10-25 01:00 → 03:33
Night B: 2024-10-24 19:34 → 2024-10-25 04:44
→ MATCH (both include Oct 25, times overlap)
```

### Metadata Calculation

Frame set metadata is calculated from member frames:

- **date_obs_start:** Earliest frame observation
- **date_obs_end:** Latest frame observation
- **objctra, objctdec:** Spherical mean of coordinates
- **total_exp_time:** Sum of all exposure times

**Why Spherical Mean?**
Simple averaging fails near RA boundaries:
```
Frames: RA=359°, RA=1°
Simple average: 180° ✗ (wrong!)
Spherical mean: 0° ✓ (correct!)
```

### Frame Uniqueness

Within a frame set, each frame should appear only once. The system ensures this through:
- Deduplication after merge
- Split validation
- Unique constraints on session_members (session_id, frame_id)

## Schema Changes (v2.0)

### What Changed

**Removed:**
- `project_id` from `frames_set` (sets now independent)
- `date_obs` from `frames_set` (replaced with range)

**Added:**
- `date_obs_start` to `frames_set`
- `date_obs_end` to `frames_set`

### Migration

Old code:
```typescript
// v1.0
const set: FramesSet = {
  date_obs: "2024-10-25",
  project_id: 1
};
```

New code:
```typescript
// v2.0
const set: FramesSet = {
  date_obs_start: "2024-10-25T00:00:00Z",
  date_obs_end: "2024-10-26T05:00:00Z"
  // No project_id
};
```

## Design Decisions

### Why Three Levels?

**Frame Set** → Logical collection (e.g., "M31 Widefield")
**Imaging Night** → Observing session (important for weather, conditions)
**Session** → Instrument grouping (important for calibration)

This mirrors real-world astrophotography organization:
- Multiple nights contribute to a target
- Multiple instruments on same night
- Frames organized for efficient processing

### Why Deduplication?

After merging, frames might exist in multiple sessions. Deduplication ensures:
- Each frame counted once for statistics
- No double-processing in workflows
- Clear data ownership

### Why Mark as Custom?

Operations like merge and split transform the frame set:
- No longer auto-generated
- User has modified organization
- Should not be re-auto-generated

## Testing Strategy

### Unit Tests

Test algorithms in isolation:
- Night matching logic
- Spherical mean calculation
- Time range union
- Deduplication

### Integration Tests

Test database operations:
- Merge with various overlap scenarios
- Split with different selection types
- Metadata recalculation
- Cascade deletion

### End-to-End Tests

Test complete workflows:
- Auto-generate → merge → split
- Custom creation → split → recalculate
- Map selection → merge into existing

## Performance Guidelines

### Query Optimization

**Fast Queries:**
- Get frame set list (single JOIN with COUNT)
- Get metadata (direct table lookup)
- Count frames (indexed JOIN)

**Slow Queries:**
- Get complete detail (hierarchical, 3-4 queries)
- Large frame sets (>10,000 frames)

**Recommendations:**
- Use pagination for large lists
- Cache frame set metadata
- Lazy-load detail views

### Operation Performance

| Operation | Time | Notes |
|-----------|------|-------|
| Auto-generate | 1-5s | Depends on frame count |
| Custom create | 0.5-2s | Depends on session count |
| Merge | 1-3s | Depends on overlap |
| Split | 0.5-2s | Depends on selection size |
| Recalculate | 0.1-1s | Depends on frame count |

## Troubleshooting

### Frame Set Appears Empty

**Check:**
1. Does it have imaging_nights? (`SELECT * FROM imaging_nights WHERE frames_set_id = ?`)
2. Do nights have sessions? (`SELECT * FROM sessions WHERE imaging_night_id = ?`)
3. Do sessions have frames? (`SELECT * FROM session_members WHERE session_id = ?`)

**Common Causes:**
- Cascade deletion of parent records
- Failed transaction during creation
- Deleted sessions without cleanup

### Coordinates Are Wrong

**Check:**
1. Do frames have coordinates? (`SELECT ra, dec, objctra, objctdec FROM frames`)
2. Are coordinates near RA boundaries? (Spherical mean handles this)
3. Mixed formats? (System handles both decimal and sexagesimal)

**Common Causes:**
- Frames without WCS/plate solving
- Incorrect FITS header values
- Parser errors in sexagesimal strings

### Merge Creates Duplicates

**This shouldn't happen!** Deduplication runs automatically.

**If it does:**
1. Check `deduplicate_session_members_in_set` ran
2. Verify session_members PRIMARY KEY constraint exists
3. Check for concurrent modifications
4. Run `recalculate_frame_set_metadata` to clean up

### Split Leaves Orphans

**Orphan sessions/nights are automatically deleted** via CASCADE.

**If they persist:**
1. Check FK constraints are properly defined
2. Verify CASCADE is enabled
3. May need manual cleanup: `DELETE FROM imaging_nights WHERE frames_set_id NOT IN (SELECT id FROM frames_set)`

## Future Enhancements

### Potential Features

1. **Undo/Redo for Merge/Split**
   - Store operation history
   - Allow rollback of recent changes

2. **Merge Preview**
   - Show what would happen before committing
   - Dry-run mode

3. **Bulk Operations**
   - Merge multiple sources into one target
   - Split multiple selections simultaneously

4. **Smart Auto-Merge**
   - Detect related frame sets
   - Suggest merges based on coordinates/names

5. **Frame Set Templates**
   - Save organization patterns
   - Apply to new data

### Optimization Opportunities

1. **Caching**
   - Cache frame set metadata in memory
   - Invalidate on updates
   - Faster list rendering

2. **Batch Operations**
   - Batch deduplication across multiple sets
   - Batch metadata recalculation
   - Transaction optimization

3. **Parallel Processing**
   - Parallelize coordinate calculations
   - Concurrent session detection
   - Multi-threaded DBSCAN clustering

## Contributing

### Adding New Operations

1. **Define Algorithm** (algorithms.md)
2. **Add Database Operations** (operations.rs)
3. **Implement Command** (commands.rs)
4. **Register Command** (lib.rs)
5. **Document API** (commands.md)
6. **Document Edge Cases** (edge_cases.md)
7. **Write Tests**
8. **Update Migration Guide**

### Documentation Standards

- Keep documentation in sync with code
- Add examples for every command
- Document all edge cases
- Explain "why" not just "what"
- Update performance characteristics
- Version all breaking changes

## Support

For issues, questions, or contributions:
- GitHub Issues: [athenaeum/issues](https://github.com/user/athenaeum/issues)
- Documentation: `docs/frame_sets/`
- Source Code: `src-tauri/src/`

## License

See repository root for license information.

---

**Last Updated:** 2025-01-14
**Version:** 2.0
**Status:** Complete Implementation
