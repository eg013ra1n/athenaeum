# External processing and exposure versions

## Review workflow

Select objects in Objects, then **Review processing / versions**. The dialog lists
available LIGHT files, stage evidence, observation metadata and proposed links.
Choose **Confirm same exposure** only after reviewing the evidence. Confirmation
is required for every match; there are no automatic merges. Unlink and manual
stage selection are available. Missing evidence remains **Unknown**, never an
inferred raw file. A collection can contain only calibrated/registered versions.

A confirmed group contributes one available representative to exposure totals.
Individual files remain visible and file counts remain counts of files. Object
and session time, export selection time, camera time, calendar and sky-map
exposure queries use confirmed representatives. Object/session subsets choose a
representative within that subset; overlapping objects are not globally disjoint.
Stacks are excluded from single-exposure totals. Their source exposures and true
integration times are not reconstructed from an ambiguous EXPTIME value.
Stack-only objects retain their pointing metadata; exposure-only map/calendar
queries do not render them as acquired single exposures.

## Evidence supported in this slice

| Evidence | Interpretation |
| --- | --- |
| STACKCNT or NCOMBINE > 1 | Multi-input product |
| HISTORY mean/median stacking or ImageIntegration | Integrated product |
| CALSTAT using B/D/F/C flags, or calibration HISTORY | Reported calibration |
| HISTORY Debayer/demosaic | Debayering |
| HISTORY StarAlignment/registration transformation/registered with | Registration |
| No supported marker | Unknown |

Multiple processing steps are retained; the displayed stage prioritizes
integration, registration, debayering, then calibration. These markers describe
reported processing, not its quality. FITS HISTORY and XISF FITSKeyword HISTORY
are parsed; textual PixInsight process-history properties are also recognized.
Encoded/compressed history, arbitrary software-specific metadata and TIFF files
are outside this first slice. A historical process name can be ambiguous: review
the evidence and use a manual stage when needed. Explicit integration evidence
cannot be overridden to a single-exposure stage.

Link suggestions require the preserved ATH_CSRC source ID, or agreement on
observation time, camera, positive exposure duration and filter. Dimensions and
preserved source names support review. Filename patterns alone never create a
link. External tools do not universally preserve a source UUID; no generic
PixInsight/Siril source-ID convention is assumed. Confirmation checks the whole
merged group for conflicting metadata. A changed stored header or an edit to
identity-bearing frame metadata invalidates the affected membership.

The review accepts at most 1,000 files and shows the first 200 candidate pairs;
it asks for a smaller selection rather than silently omitting records.
