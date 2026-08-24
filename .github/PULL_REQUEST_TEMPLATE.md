## What this changes

<!-- One or two sentences. Link the issue if there is one. -->

## Checklist

- [ ] If a Tauri command changed, the matching Axum route in `crates/athenaeum-web/src/routes/` changed too (and vice versa)
- [ ] Logic lives in `athenaeum-core`; the command and route layers stay thin wrappers
- [ ] No `@tauri-apps/*` import outside `src/api/`
- [ ] New or changed structs crossing the boundary use `#[serde(rename_all = "camelCase")]`, and `src/types/models.ts` matches
- [ ] No new colour literals — design tokens only
- [ ] Errors are logged before being returned; no `println!`/`eprintln!` in production code
- [ ] `cargo build --workspace` passes
- [ ] `cargo test -p athenaeum-core` passes
- [ ] `npx tsc --noEmit` passes

## How it was tested

<!-- Real FITS/XISF data if the change touches parsing, calibration or scanning. -->
