---
name: implement
description: Implement a plan or feature in the Athenaeum codebase, enforcing the project's cardinal rules (two-backend sync, api-layer boundary, serde casing, design tokens, error logging). Use when turning a plan/spec into code, adding or changing a Tauri command, or building a frontend feature.
---

# Implement Plan

Turn a plan/spec into working code while respecting Athenaeum's architecture.
Read CLAUDE.md first if anything below is unclear — it is the source of truth.

## Before writing code

1. **Read the plan/spec completely.** Most live under `docs/superpowers/specs/`
   and `docs/superpowers/plans/`. Note scope boundaries before touching anything.
2. **Locate the layers you'll touch.** Logic belongs in `athenaeum-core`; the
   Tauri/Axum layers are thin wrappers. Find the relevant `commands/<domain>.rs`,
   its mirror `routes/<domain>.rs`, the core module, and the frontend page/hook.
3. **Scope minimally.** Implement the smallest viable version first; do not stub
   or refactor unrelated systems. If scope is ambiguous, ask before expanding.

## Cardinal rules (non-negotiable)

- **Two backends in sync.** Any new/changed Tauri command
  (`crates/athenaeum-tauri/src/commands/<domain>.rs`, registered in
  `lib.rs::invoke_handler`) REQUIRES the matching Axum route
  (`crates/athenaeum-web/src/routes/<domain>.rs`, registered in `routes/mod.rs`)
  in the SAME change. Real logic goes in `athenaeum-core` so both call it. For
  progress, web side uses `SseProgressEmitter::new(state.event_tx.clone())`.
- **Serde boundary: snake_case <-> camelCase.** Use
  `#[serde(rename_all = "camelCase")]` on boundary structs and verify the TS
  interface in `src/types/models.ts` (or `calibration-config.ts`) matches field
  for field.
- **No `@tauri-apps/*` imports outside `src/api/`.** Frontend always goes through
  the `api` object; desktop-only bits live in `src/api/desktop.ts`.
- **Error handling: `anyhow::Result` inside core; convert with
  `.map_err(|e| e.to_string())` at the command/route boundary. Never swallow
  errors** — log to console/stderr before returning. Silent failures have cost
  hours here.
- **Design tokens, not raw colors** in the frontend (`bg-surface`,
  `text-content-muted`, `bg-accent`, `text-error`, ...) so dark/light both work.
- **Notifications** go through `notify()` from `useNotifications()` — never build
  ad-hoc toasts. Notify on discrete outcomes only, never on `*-progress`.
- **Tauri/SSE listeners** use the StrictMode-safe cancelled-flag pattern (see
  CLAUDE.md -> Notifications) to avoid leaked double listeners in dev.

## Verify as you go

- **Rust:** a `Stop` hook runs one `cargo check --workspace` at turn end whenever
  the turn touched a `.rs` file, reporting only the errors once (rust-analyzer LSP
  gives live per-edit feedback in between). Treat a red check as a stop condition —
  fix before moving on. For logic, run `cargo test -p athenaeum-core` (or `--workspace`).
- **TypeScript:** run `npx tsc --noEmit` after frontend edits.
- **Multi-file edits in complete passes** — don't leave the workspace in a
  half-edited non-compiling state across unrelated files; finish a coherent unit,
  then check.
- **Real data first.** Validate with a real FITS/XISF file early; synthetic tests
  can mask real-world parsing/coordinate bugs.

## Finish

- Confirm both backends were updated when a command changed, and TS types match
  the serde structs.
- Summarize what changed (by file/layer) and what remains.
