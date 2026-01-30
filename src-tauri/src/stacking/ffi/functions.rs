//! FFI function stubs
//!
//! This module previously contained C FFI declarations. Now that we use pure Rust
//! implementations, this module only provides stub functions for backwards compatibility.

/// Check if FFI is available (always returns true now with pure Rust implementation)
pub fn is_ffi_available() -> bool {
    true
}
