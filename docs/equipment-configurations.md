# Equipment configuration review

On Equipment, add a named telescope/camera configuration with native focal length
(mm), reducer/flattener multiplier (1 for no focal-length change), physical
unbinned pixel pitch (µm), symmetric binning and a fractional matching tolerance.
Choose the camera and review its saved plate solves, 100 files per page.

Expected angular sampling is `(180/π) × 3.6 × pitch × binning / (focal length × multiplier)`
in arcseconds per pixel. The displayed difference is `100 × |observed/expected − 1|`.
The tolerance is an engineering selection threshold, not a statistical confidence.
Camera names must match exactly; both header binning axes must match the profile.
Missing/asymmetric binning, invalid scales and out-of-tolerance values produce no
candidate. All candidates are shown, ordered by difference, retaining ties.
Resampling and drizzle can break the link between stored binning and solved scale.
Scale agreement cannot uniquely identify physically different optical systems.

Only **Confirm match** records an association. Confirmations are per file/frame,
not acquisition counts. Original headers, solves and exposure-version membership
are untouched. Changed solves or profile revisions show **Needs review**; stale
requests are rejected transactionally. Remove configuration deletes that profile
and its confirmations; clear removes only the selected confirmation.

Two additive catalog tables store profiles and explicit matches. Foreign-key
indexes support parent deletion; no prior records are automatically assigned.
Schema initialization is repeatable.
Shared core matching/storage powers mirrored desktop and web endpoints, with
Rust-generated TypeScript models.

Tests use synthetic in-memory catalogs for sampling, binning, ambiguity, missing metadata, stale confirmations, pagination and repeated schema initialization.
