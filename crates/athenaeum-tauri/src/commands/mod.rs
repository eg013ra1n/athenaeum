// Commands module - organized by domain
//
// This module structure replaces the monolithic commands.rs file (2,878 lines)
// with focused, domain-specific modules for better maintainability.

use std::sync::{Arc, RwLock};

// Re-export core service types so commands can use them directly
pub use athenaeum_core::services::{ServiceContext, ScanHandle, ExportHandle};

/// Tauri-specific app state wrapping the shared ServiceContext.
///
/// The `ctx` field holds all backend-agnostic state. Tauri-only fields
/// (semaphore, max_blink_threads) live here alongside it.
pub struct AppState {
    pub ctx: ServiceContext,
    /// Limits concurrent image conversions; wrapped in RwLock so the semaphore
    /// can be swapped at runtime when the user changes blink.threads.
    pub image_semaphore: RwLock<Arc<tokio::sync::Semaphore>>,
    /// CPU-based upper bound for blink threads (min(vCPUs, 16))
    pub max_blink_threads: usize,
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
pub mod utils;

// Re-export all commands for convenient access
pub use core::*;
pub use scan_roots::*;
pub use files::*;
pub use settings::*;
pub use frame_sets::*;
pub use calibration::*;
pub use duplicates::*;
pub use missing_files::*;
pub use cache::*;
pub use spatial::*;
pub use calendar::*;
pub use export::*;
pub use analysis::*;
