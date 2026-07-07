// Athenaeum Core Library
// Shared business logic for desktop (Tauri) and web (Axum) frontends

pub mod models;
pub mod coordinates;
pub mod fingerprint;
pub mod calibration;
pub mod db;
pub mod fits_parser;
pub mod fits_writer;
pub mod clustering;
pub mod settings;
pub mod sessions;
pub mod duplicates;
pub mod relinking;
pub mod frames_set_metadata;
pub mod frames_set_merge;
pub mod logging;
pub mod events;
pub mod scanner;
pub mod monitor;
pub mod auto_merge;
pub mod export;
// rustafits (lib name `astroimage`) image pipeline — `render` feature.
#[cfg(feature = "render")]
pub mod rustafits_processor;
pub mod cache;
#[cfg(feature = "render")]
pub mod analysis;
#[cfg(feature = "render")]
pub mod flat_analysis;
pub mod orientation;
// catalog/gaia builds on both astroimage (proper-motion) and solvemyastro (cache).
#[cfg(all(feature = "render", feature = "solver"))]
pub mod catalog;
// plate_solve + registration consume both astroimage and solvemyastro.
#[cfg(all(feature = "render", feature = "solver"))]
pub mod plate_solve;
pub mod services;
pub mod archive;
pub mod file_op;
#[cfg(all(feature = "render", feature = "solver"))]
pub mod registration;
// ts_export references types from every render/solver-gated module; it is a
// build-time TS-generation harness driven only by tests/ts_contract.rs.
#[cfg(all(feature = "render", feature = "solver"))]
pub mod ts_export;
pub mod api;
// integration/banded reads raw pixels via astroimage — `render` feature.
#[cfg(feature = "render")]
pub mod integration;
pub mod calibration_library;
// Personal-sync transport layer (Stage I). Transport-agnostic trait +
// in-process mock; ungated so the headless agent build includes it. No
// render/solver deps.
pub mod sharing;
// Portable package format (NDJSON manifest + payload files) for Stage I sync.
// Ungated — no render/solver deps, compiles in the headless build.
pub mod package;
// Sender-side sync engine (Stage I, task A4): SyncStore + outbound state machine
// + worker over a SharingTransport. Ungated — pure rusqlite/tokio, no
// render/solver deps, compiles in the headless build.
pub mod sync;
// App account layer (Stage II, task B4): hub client, the shared iroh device
// identity, OS-keychain token store. Ungated — no render/solver deps.
pub mod account;
