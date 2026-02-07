# Implement Plan

## Steps
1. Read the plan document completely before starting
2. Identify all Rust<->TypeScript serialization boundaries and verify serde attributes
3. Implement the MINIMAL viable version first - do not stub unrelated systems
4. After each file edit, run `cargo check` (Rust) or `npx tsc --noEmit` (TypeScript)
5. Ensure all error paths log to console - never swallow errors silently
6. Test with real data files if available, not just synthetic tests
7. Summarize what was changed and what remains
