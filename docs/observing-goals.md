# Observing goals and integration progress

Open an object/field detail and expand
**Observing goals & integration progress**. Add a target integration in hours for
each exact filter name, including filters with no observations yet. A blank filter
means missing catalog filter metadata. Goals are explicit; a one-hour form default
is saved only when the user chooses Save. Without a saved goal, no completion
percentage is shown. Edits use revision checks; conflicting edits require refresh.

## Meaning of the counts

One field is one existing frame set. Create separate field sets for mosaic panels
when panel-specific goals are needed. This feature does not infer panel boundaries
or desired exposure depth from the data. It measures integration completeness,
not sky-area coverage, footprint overlap, limiting depth or signal-to-noise gain.

1. Select the field's LIGHT frames; exclude black-holed file records.
2. Exclude products classified as integrated and deduplicate confirmed exposure
   versions within that selection using the existing representative policy:
   positive EXPTIME, available coordinates, then lowest frame ID. No raw preference
   or best-quality-version selection is added. Unconfirmed versions may count twice.
3. Apply the filter's saved policy to that representative's saved analysis.
4. Report accepted, rejected and unknown counts separately. Sum positive finite
   exposure seconds only for accepted representatives. Completion is accepted
   seconds / target seconds; values above 100% remain visible.

Fields may overlap: do not sum these rows across fields to obtain a unique project
exposure total. The read transaction keeps each progress response consistent with
its goal, membership and measurements. Progress reads do not alter any records.

## Explicit quality policy

Without an analysis requirement, every positive finite EXPTIME qualifies. Missing,
zero, negative or nonfinite EXPTIME is unknown. This default is a duration count,
not a claim that the images passed scientific quality review.

With **Require valid star analysis**, the representative must have detected stars,
a positive finite median FWHM and finite eccentricity in [0,1]. Missing or invalid
analysis is unknown, even if another version has a usable measurement. Optional
maximum FWHM (image pixels), maximum eccentricity and rejection of the trail flag
then define rejection. Equality passes each numeric limit. Blank limits impose no
threshold. No-star measurements do not pass merely because a stored value is zero.

FWHM depends on sampling, binning and resampling. Choose thresholds only after
checking those properties. The table uses saved catalog measurements; it does not
reanalyze images or verify their current bytes. Refresh after analysis, membership,
classification or exposure-link changes. Rejection here is a counting policy only;
it does not black-hole a file or change its image/header/solve.

## Storage and validation

One additive `observing_goals` table stores per-field/filter policies and revision
numbers, with a cascading field foreign key. Goals contain seconds internally;
the UI displays hours. Core storage, qualification and aggregation are separate
small modules; Tauri/Axum expose the same three endpoints and generated TS models.

Synthetic full-schema tests cover linked versions, integrated/black-holed exclusions,
no quality cherry-picking, missing measurements, threshold boundaries, trail flags,
goals without observations, stale writes and repeatable initialization. React
server-render tests cover absent goals, zero completion and visible surplus.
