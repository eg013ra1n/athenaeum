//! FFI bindings to Siril C library
//!
//! This module contains raw C FFI definitions. These are unsafe and should
//! only be used through the safe wrappers in the parent module.
//!
//! # Safety
//!
//! All functions in this module are unsafe as they directly interface with
//! C code. Memory management, null pointer checks, and thread safety must
//! be handled by the caller.

pub mod functions;
pub mod types;

// Re-export commonly used FFI types
pub use functions::*;
pub use types::*;
