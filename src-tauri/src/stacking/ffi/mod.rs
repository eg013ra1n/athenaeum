//! FFI compatibility module
//!
//! This module provides type definitions and helper functions used throughout
//! the stacking module. Originally designed for C FFI, these types are now
//! used for pure Rust implementations.

pub mod functions;
pub mod types;

// Re-export commonly used types
pub use functions::*;
pub use types::*;
