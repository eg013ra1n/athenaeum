### Scope
- The Objects view must provide an action to auto-create frame sets (frame_set) by clustering LIGHT frames whose sky centers are within a configurable cone radius threshold, then list those sets with total integrated exposure time and basic metadata. [1][3]
- A Settings view must define and persist the angular threshold and unit (arcsec/arcmin/deg) used by the grouping action, with application defaults if the database has no value. [1][4]

### Data model

- Add a Settings table with explicit precedence: runtime override > DB value > built-in app defaults, so the grouping feature always has a threshold even if DB is empty. [4][5]

```sql
CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value TEXT,           -- store as string; parse/validate by type
  updated_at TEXT
);

-- Recommended keys:
-- grouping.threshold.value   e.g., "5.0"
-- grouping.threshold.unit    one of {"arcsec","arcmin","deg"}
-- grouping.coord.frame       e.g., "ICRS" (J2000)
-- ui.objects.auto_name_mode  e.g., "majority-object"|"ra-dec"
```


### Coordinate normalization
- All angular computations must be performed in a single reference frame (ICRS/J2000) using decimal degrees for RA/Dec, converting any sexagesimal strings and frame/equinox variants from the FITS headers. [8][9]
- Prefer the numeric RA/DEC columns for clustering; if absent or invalid, attempt to parse OBJCTRA/OBJCTDEC into decimal degrees and populate temporary numeric values for the run. [7][6]

### Grouping action (Objects view)
- Add “Auto-generate sets by coordinates” button that runs deterministic clustering of eligible frames using a cone-radius threshold from Settings. [1][10]
- Eligible frames are those with IMAGETYP indicating LIGHT in frames table and a valid numeric RA/Dec pair after normalization. [3][11]
- Distance test: two frames belong to the same set if the great-circle angular separation between their centers and the evolving set center is ≤ threshold, where separation $$ \theta $$ is computed via the spherical law of cosines or haversine in degrees. [1][12]
- Center update: when merging a frame into a set, update the set center as the spherical mean (unit vectors averaged then re-normalized) to reduce drift with wide hour-angle coverage.
- Determinism: sort candidate frames by RA then Dec then DATE-OBS before seeding to ensure stable results across runs with the same threshold. [12][3]

### Set creation rules
- Seed selection: iterate sorted LIGHT frames; for each unassigned frame, create a new set and assign that frame as the initial center. [12][1]
- Membership: find all unassigned LIGHT frames within the threshold to the current center, add them, recompute center, and repeat until no new members are within the threshold. [1][10]
- Naming: if a strict majority of members share the same OBJECT, use that as name; otherwise name as “Unknown @ RA=HH:MM:SS, Dec=±DD:MM:SS” rounding to nearest 1 arcmin. [13][2]
- date_obs on frames_set should store an indicative value such as the earliest DATE-OBS (UTC) among members to populate the list quickly, while the UI still shows the full date span. [14][9]
- objctra/objctdec on frames_set store representative center in sexagesimal for display; source-of-truth numeric center must be maintained in memory for clustering and can be persisted in auxiliary columns if needed. [7][8]

### Membership persistence
- For each created set, insert one row in frames_set and bulk insert rows in frames_set_members for each member frame_id. [4][5]
- Deleting a set from the UI must delete the frames_set row and cascade-delete its frames_set_members rows without touching frames. [4][5]
- Re-running auto-generate should offer: Replace auto sets (delete all frames_set rows created by auto mode within the project_id, then rebuild) or Add-only if manual sets exist. [4][5]

### Filters, exclusion, and edge cases
- Exclude frames with missing or unparsable RA/Dec after normalization; list them under “No coordinates” with guidance to fix OBJCTRA/OBJCTDEC or RA/DEC metadata. [7][6]
- If DATE-OBS is missing, allow inclusion in clustering by coordinates but mark date span as “unknown” and exclude such frames from calendar summaries. [14][9]
- If EQUINOX/RADESYS imply non-ICRS values, convert to ICRS before clustering to avoid biases from coordinate systems. [8][15]

### Threshold and units (Settings)
- Grouping threshold is stored as value + unit and applied globally per project when auto-generating sets. [1][16]
- Accepted units: arcsec, arcmin, deg, with conversions performed before clustering. [1][16]
- Defaults: if settings are absent, use app defaults (e.g., 5 arcmin, ICRS frame) and render an informational banner that defaults are in effect. [1][4]

### UI behavior (Objects view)
- After execution, display a list of sets: name, center (RA/Dec), number of LIGHT frames, total integration hours, date span, and instrument summary (unique TELESCOP/INSTRUME pairs). [3][17]
- Total integration must sum EXPTIME across LIGHT members and display hours as $$ \frac{\sum EXPTIME}{3600} $$ with one decimal precision. [3][14]
- Provide actions per set: View members, Open in Files, Delete set, and Rename set. [4][5]

### Database operations and integrity
- Transactions: run grouping in a single transaction per project to ensure either all sets and memberships are created or none, with rollback on error. [4][5]
- Indexing: ensure frames has indexes on date_obs, object, instrume, RA, DEC, OBJCTRA, OBJCTDEC, exptime, filter, focallen as specified, and add composite index on (project_id, RA, DEC) if project scoping is implemented at the frame level. [12][1]
- Spatial optimization (optional): for large catalogs, pre-compute HEALPix cell or use a q3c/q3c_radial_query-like approach in the data layer for candidate lookup in a radius. [12][1]
- Consistency: enforce foreign keys frames_set_members.frames_set_id → frames_set.id and frames_set_members.frame_id → frames.id with ON DELETE CASCADE on the set side and RESTRICT on the frame side. [4][5]

### Settings precedence and defaults
- Precedence order: runtime override (session) > settings table value > compiled application default, ensuring predictable behavior when DB is empty or partially populated. [4][5]
- On first run, write effective defaults into settings for visibility, but grouping must not depend on settings inserts to complete. [4][5]

### Data quality rules
- LIGHT only: include frames where IMAGETYP equals LIGHT (or equivalent instrument-specific variants) during set creation, excluding dark/flat/bias frames by design. [11][3]
- Exposure summation: ignore frames lacking EXPTIME when computing total hours but include them as members if they meet coordinate criteria, and flag incomplete integration in the UI. [3][18]
- Object naming: OBJECT may be instrument-specific or absent; do not fail grouping when OBJECT is missing, and fall back to RA/Dec naming. 

### Acceptance criteria
- With a defined threshold, running auto-generate produces a deterministic set list for a fixed input catalog, with stable names, centers, counts, date spans, and total hours computed as specified. [1][12]
- Deleting a set removes only the frames_set row and its frames_set_members without deleting any frames or files. [4][5]
- Changing the threshold and re-running with “Replace auto sets” removes prior auto-generated sets and creates new ones accordingly, leaving manually created sets intact. [4][5]
- The Objects list shows total integration time in hours computed from EXPTIME in seconds with visible precision rules, and handles missing EXPTIME gracefully. [3][14]

### Notes on FITS/XISF mapping to DB
- Map standard FITS keywords to DB columns: OBJECT→object, DATE-OBS→date_obs, TELESCOP→telescop, INSTRUME→instrume, EXPTIME→exptime, FILTER→filter, IMAGETYP→imagetyp, OBJCTRA→objctra, OBJCTDEC→objctdec, and ensure RA/DEC numeric columns reflect normalized decimal degrees used in clustering. [2][3]
- Ensure DATE-OBS is parsed to an ISO timestamp string when available; if DATE-OBS and TIME-OBS are split, combine to a single value for consistent sorting and calendar grouping. [14][9]