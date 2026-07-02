// Commands module - organized by domain
//
// This module structure replaces the monolithic commands.rs file (2,878 lines)
// with focused, domain-specific modules for better maintainability.

use std::sync::{Arc, RwLock};

// Re-export core service types so commands can use them directly
pub use athenaeum_core::services::{ServiceContext, ExportHandle};

/// Tauri-specific app state wrapping the shared ServiceContext.
///
/// The `ctx` field holds all backend-agnostic state. Tauri-only fields
/// (semaphore, max_blink_threads) live here alongside it. `ctx` is `Arc`-wrapped
/// so background tasks (monitor service, etc.) can hold their own reference.
pub struct AppState {
    pub ctx: Arc<ServiceContext>,
    /// Limits concurrent image conversions; wrapped in RwLock so the semaphore
    /// can be swapped at runtime when the user changes blink.threads.
    pub image_semaphore: RwLock<Arc<tokio::sync::Semaphore>>,
    /// CPU-based upper bound for blink threads (min(vCPUs, 16))
    pub max_blink_threads: usize,
    /// Handle to the background folder-monitoring service. Commands use this
    /// to `kick()` the loop awake when settings change so the user doesn't
    /// have to wait for the next scheduled tick.
    pub monitor: athenaeum_core::monitor::MonitorService,
}

pub mod core;
pub mod scan_roots;
pub mod files;
pub mod settings;
pub mod frame_sets;
pub mod calibration;
pub mod duplicates;
pub mod missing_files;
pub mod cache;
pub mod spatial;
pub mod calendar;
pub mod export;
pub mod analysis;
pub mod plate_solve;
pub mod registration;
pub mod utils;
pub mod archive;

// Re-export all commands for convenient access
pub use core::*;
pub use scan_roots::*;
pub use files::*;
pub use settings::*;
pub use frame_sets::*;
pub use calibration::*;
pub use duplicates::*;
pub use missing_files::*;
pub use spatial::*;
pub use calendar::*;
pub use export::*;
pub use analysis::*;
pub use plate_solve::*;
pub use registration::*;
pub use archive::*;
