---
invokable: true
---

Review this code for potential issues, including:

- **Rust backend**:
  - Correctness of async Tauri commands and proper error handling (`anyhow::Result`).
  - Safety of file‑system operations (path traversal, symlinks) during scanning and export.
  - Consistency of SQLite schema updates and migrations.
  - Performance of the multi‑threaded scanner (use of `rayon` & `walkdir`).
  - Proper use of `xxhash-rust` – ensure hash is computed on the full file and that duplicate groups are correctly persisted.
  - Validation of FITS/XISF parsing – required keywords present, date normalization, handling of corrupted headers.
- **React frontend**:
  - Type safety between TypeScript interfaces in `src/types/models.ts` and Rust structs in `src-tauri/src/models.rs`.
  - Correct usage of Tauri `invoke()` – arguments match command signatures, errors are caught.
  - UI performance when rendering large directory trees (virtualisation, memoisation).
  - Accessibility & responsive design for the dark‑theme layout.
  - Tailwind CSS class consistency and purge configuration.
- **General project**:
  - Ensure `package.json` scripts align with Tauri CLI (`npm run tauri dev`).
  - Verify that all required environment prerequisites (Node 18+, Rust 1.70+, macOS libs) are documented.
  - Check for linting / formatting compliance (`cargo fmt`, `cargo clippy`, `eslint/prettier` if added).

Provide specific, actionable feedback for each finding, suggesting concrete code changes or refactors where appropriate.