
### Scope and goals
- The application scans user-selected directories across multiple disks, builds a single metadata catalog of FITS/XISF files, enables searching and grouping by objects and equipment, and manages a calibration library for each capture day and setup. [3][4]
- Exact duplicate detection is implemented with a fast non-cryptographic hash to prioritize scan speed, while preserving the ability to confirm byte-accurate identity when performing critical actions. [1][5]

### Modes
- File Manager: add monitored paths, trigger scans, view metadata table, run exact duplicate report, and apply metadata filters similar to Lightroom’s Library Filter. [3][6]
- Shoot Calendar: a calendar view aggregates captures by DATE-OBS with equipment and target labels for quick navigation into file lists. [7][6]
- Objects: a library of OBJECT names with drill-down by capture dates and instruments, linking to underlying files. [8][3]
- Equipment: a library of TELESCOP and INSTRUME entries showing which objects were captured and when. [7][9]
- Export: a guided flow that copies selected files into a user-defined directory scheme populated by tokens from FITS/XISF metadata and catalog tags. [2][4]

### Metadata extraction (FITS/XISF)
- The scanner extracts standard FITS keys: OBJECT, DATE-OBS (and TIME-OBS if present), TELESCOP, INSTRUME, EXPTIME, FILTER, and IMAGETYP, following common FITS dictionaries and observatory practices. [8][7]
- XISF headers and embedded FITS-like properties are parsed according to the XISF 1.0 specification to keep values consistent with FITS across the catalog. [4][10]
- DATE-OBS is normalized to an ISO 8601 timestamp or combined DATE-OBS+TIME-OBS representation to ensure consistent calendar grouping and sorting. [7][11]

### Calibration library and linking
- The catalog stores calibration frames as first-class entities: Bias, Dark, Flat, and DarkFlat, with their parameters (EXPTIME, FILTER, sensor temperature, gain/ISO) for matching. [12][13]
- Calibrations are linked to a “day + setup” by type and parameter tolerances, with auto-suggestions and manual confirmation when values are missing or ambiguous. [12][13]

### Exact duplicate detection
- Strategy: candidate grouping by identical file size, followed by a fast non-cryptographic hash (e.g., xxHash XXH3) to confirm identical content with minimal I/O and CPU time. [14][1]
- Rationale: xxHash is engineered to run at RAM speed limits and outperforms MD5/SHA for throughput on large datasets, making it well-suited for directory scans and deduplication workflows. [1][15]
- Optional safeguard: when executing destructive or move operations on duplicates, an additional byte-to-byte verification can be enabled to guarantee identity beyond hash equality. [5][16]
- Caching: a hash cache keyed by file size, path, and timestamps accelerates incremental rescans and avoids re-reading unchanged files. [16][5]

### Export path templating
- The export module accepts a user-defined template with tokens that resolve from FITS/XISF metadata and catalog tags, similar to Lightroom’s filename template editor. [2][17]
- Core tokens: {OBJECT}, {DATE-OBS:%Y-%m-%d}, {TELESCOP}, {INSTRUME}, {EXPTIME}, {FILTER}, {IMAGETYP}, and a derived {FRAME_FOLDER} that maps IMAGETYP to Lights or Calibration/<type>. [7][2]
- FRAME_FOLDER mapping: LIGHT → Lights, DARK → Calibration/Darks, FLAT → Calibration/Flats, BIAS → Calibration/Bias, and DARKFLAT → Calibration/DarkFlats, per FITS usage. [18][7]
- Transformations and defaults: support strftime formatting for DATE-OBS, case/slug transforms, and fallbacks like {OBJECT|Unknown} to ensure valid and portable paths. [2][19]
- Examples: “Object/{DATE-OBS:%Y-%m-%d}/{TELESCOP}/{FRAME_FOLDER}” and “{OBJECT|Unknown}/{DATE-OBS:%Y}/{DATE-OBS:%m}{DATE-OBS:%d}/{INSTRUME:slug}/{FRAME_FOLDER}” for multilingual or instrument-centric layouts. [2][17]
- Preview and collisions: a pre-copy preview expands tokens into final paths and applies Skip/Overwrite/Rename policies including counters ({SEQ:%03d}) when needed. [2][20]

### Functional requirements by mode
- File Manager: add/remove scan roots, start/stop scans, filter table by metadata, view “Duplicates” groups by equal size and xxHash, and perform safe actions with optional byte-verify. [3][1]
- Shoot Calendar: navigate month/week views, click a day to see sessions and equipment, and jump to filtered file lists for that date. [6][7]
- Objects: list of unique OBJECT values with counts and last activity, with drill-down by instrument and date into actual files. [8][3]
- Equipment: list of TELESCOP and INSTRUME with captured objects and dates, supporting filters and navigation back to files. [7][9]
- Export: select files by filters or lists, choose a template, preview generated paths, resolve conflicts, and execute copying with a completion report. [2][20]

### Data model
- File: absolute path, size, timestamps, format (FITS/XISF), non-cryptographic content hash, and duplicate-group identifier. [5][16]
- Frame: pointer to File plus OBJECT, DATE-OBS, TELESCOP, INSTRUME, EXPTIME, FILTER, IMAGETYP, and derived fields used in filtering and export. [8][7]
- Day: aggregates frames by normalized DATE-OBS for calendar views and calibration linking. [7][11]
- Setup: telescope/camera/filter/binning/gain fields extracted from headers for matching and summaries. [7][9]
- Calibration set: groups of Dark/Flat/Bias/DarkFlat with parameters and linked days/setups. [12][13]
- Tags: user-defined labels used in filtering and token substitution. [6][3]

### Architecture and technology
- Metadata service: local background service for scanning directories, extracting FITS/XISF metadata, computing xxHash, and updating the catalog database. [4][1]
- FITS/XISF parsing: adhere to FITS keyword dictionaries and XISF 1.0 parsing rules for consistent metadata extraction. [8][4]
- UI: desktop application with views for Files, Calendar, Objects, Equipment, and Export, mirroring familiar Lightroom-style filtering and templating UX. [3][2]
- Hashing: xxHash (prefer XXH3 64/128) to maximize scan throughput on large datasets, with streaming mode for large files. [15][21]

### Non-functional requirements
- Performance: multi-threaded traversal, size-first candidate grouping, and streaming xxHash must support high sustained throughput for spinning and SSD volumes. [14][15]
- Reliability: optional byte-verify before destructive actions on duplicates eliminates operational risk from rare non-cryptographic hash collisions. [5][16]
- Compatibility: robust handling of OBJECT, DATE-OBS, TELESCOP, INSTRUME, EXPTIME, FILTER, and IMAGETYP across common FITS conventions and XISF 1.0. [8][4]

### Acceptance criteria
- Scanning and cataloging: given multiple roots, the system lists FITS/XISF frames with extracted metadata and maintains a deduplication report by equal size and xxHash. [3][1]
- Modes: File Manager, Shoot Calendar, Objects, Equipment, and Export function as specified with filter presets and cross-navigation. [3][6]
- Export: user templates resolve tokens correctly, FRAME_FOLDER maps from IMAGETYP as defined, preview shows final paths, and conflict policies work as configured. [2][18]
- Calibrations: automatic and manual linking of calibration frames to “day + setup” is reflected in views and considered during export selection. [12][13]

If desired, the hash can default to XXH3_64bits for minimal storage and maximum speed, with a preference toggle to XXH3_128bits when extra headroom against accidental collisions is desired. [15][1]

Sources
[1] xxHash - Extremely fast non-cryptographic hash algorithm https://xxhash.com
[2] The Filename Template Editor and Text Template Editor https://helpx.adobe.com/lightroom-classic/help/filename-template-editor-text-template.html
[3] How to find photos in a catalog in Lightroom Classic https://helpx.adobe.com/lightroom-classic/help/finding-photos-catalog.html
[4] XISF Version 1.0 Specification https://pixinsight.com/doc/docs/XISF-1.0-spec/XISF-1.0-spec.html
[5] File Checksums - Duplicate File Detective https://www.duplicatedetective.com/content/static/help/html/filechecksums.html
[6] Metadata basics and actions in Lightroom Classic https://helpx.adobe.com/lightroom-classic/help/metadata-basics-actions.html
[7] FITS File Header Definitions - Diffraction Limited https://cdn.diffractionlimited.com/help/maximdl/FITS_File_Header_Definitions.htm
[8] Dictionary of Commonly Used FITS Keywords - HEASARC https://heasarc.gsfc.nasa.gov/docs/fcg/common_dict.html
[9] Description of FITS headers at the NOT https://www.not.iac.es/instruments/development/NOTfitsV101.pdf
[10] XISF https://nighttime-imaging.eu/docs/master/site/advanced/file_formats/xisf/
[11] FITS Headers https://lco.global/documentation/data/fits-headers/
[12] Understanding Calibration Frames https://telescope.live/blog/understanding-calibration-frames
[13] Understanding Calibration Frames | High Point Scientificwww.highpointscientific.com › post › astro-photography-guides › understa... https://www.highpointscientific.com/astronomy-hub/post/astro-photography-guides/understanding-calibration-frames
[14] Fastest algorithm to detect duplicate files https://stackoverflow.com/questions/53314863/fastest-algorithm-to-detect-duplicate-files
[15] Benchmarks https://xxhash.com/doc/v0.8.2/
[16] Caching https://www.duplicatedetective.com/content/static/help/html/caching.html
[17] Renaming Photos using File Name Templates in Lightroom ... https://jkost.com/blog/2024/07/renaming-photos-using-file-name-templates-in-lightroom-classic.html
[18] SBIGFITSEXT FITS Standard Version 1r0 https://diffractionlimited.com/wp-content/uploads/2016/11/sbfitsext_1r0.pdf
[19] The Filename Template Editor and Text Template Editor https://helpx.adobe.com/si/lightroom-classic/help/filename-template-editor-text-template.html
[20] Photo Renaming Options in Lightroom https://lightroomkillertips.com/photo-renaming-options-lightroom/
[21] Cyan4973/xxHash: Extremely fast non-cryptographic hash ... https://github.com/Cyan4973/xxHash
[22] xxHash vs Fast-Hash - A Comprehensive Comparison https://mojoauth.com/compare-hashing-algorithms/xxhash-vs-fast-hash/
[23] xxHash - Extremely fast non-cryptographic hash algorithm https://www.reddit.com/r/programming/comments/700xiv/xxhash_extremely_fast_noncryptographic_hash/
[24] The Filename Template Editor and Text Template Editor https://helpx.adobe.com/ie/lightroom-classic/help/filename-template-editor-text-template.html
[25] What is difference between xxHash vs dhash https://compile7.org/compare-hashing-algorithms/what-is-difference-between-xxhash-vs-dhash/
[26] Making the Most of Lightroom Classic Templates https://lightroomkillertips.com/making-the-most-of-lightroom-classic-templates/
[27] xxHash vs MD2 https://ssojet.com/compare-hashing-algorithms/xxhash-vs-md2/
[28] SHA-1 vs xxHash - A Comprehensive Comparison https://mojoauth.com/compare-hashing-algorithms/sha-1-vs-xxhash/
[29] FITS Standard (NASA) - HEASARC https://heasarc.gsfc.nasa.gov/docs/fcg/standard_dict.html
[30] Rename Photo - Filename Template Editor https://www.lightroomqueen.com/community/threads/rename-photo-filename-template-editor.45737/
[31] php - Fastest hash for non-cryptographic uses? https://stackoverflow.com/questions/3665247/fastest-hash-for-non-cryptographic-uses
[32] FITS Keywords - PyEmir - Read the Docs https://pyemir.readthedocs.io/en/latest/user/keywords.html
[33] Renaming Photos using File Name Templates in Lightroom ... https://www.youtube.com/watch?v=WNc0T8ISclU
[34] xxHash vs UMAC https://ssojet.com/compare-hashing-algorithms/xxhash-vs-umac/
