//! Build script for Athenaeum
//!
//! This build script handles:
//! 1. Standard Tauri build setup
//! 2. Optional FFI linking to Siril stacking library (when siril-ffi feature enabled)
//!
//! # Siril FFI Integration
//!
//! The Siril stacking library is a complex C/C++ codebase with many dependencies:
//! - cfitsio (FITS file I/O)
//! - wcslib (World Coordinate System)
//! - GSL (GNU Scientific Library)
//! - GLib/GObject
//! - OpenCV (optional, for advanced registration)
//! - FFTW3 (Fast Fourier Transform)
//! - lcms2 (Color management)
//!
//! ## Build Approaches
//!
//! ### Option 1: Pre-compiled library (Recommended for initial development)
//! Build Siril as a static library using meson, then link against it:
//! ```bash
//! cd src/stacking
//! meson setup build --default-library=static
//! meson compile -C build
//! ```
//! Then set SIRIL_LIB_DIR to point to the built library.
//!
//! ### Option 2: System-installed Siril
//! If Siril is installed via package manager, use pkg-config to find it.
//!
//! ### Option 3: Compile from source (Complex)
//! Use the cc crate to compile necessary C files. This requires all
//! dependencies to be available and correctly configured.

fn main() {
    // Standard Tauri build
    tauri_build::build();

    // FFI configuration (when siril-ffi feature is enabled)
    #[cfg(feature = "siril-ffi")]
    configure_siril_ffi();
}

#[cfg(feature = "siril-ffi")]
fn configure_siril_ffi() {
    use std::env;

    println!("cargo:rerun-if-env-changed=SIRIL_LIB_DIR");
    println!("cargo:rerun-if-env-changed=SIRIL_INCLUDE_DIR");

    // Check for pre-built Siril library
    if let Ok(lib_dir) = env::var("SIRIL_LIB_DIR") {
        println!("cargo:rustc-link-search=native={}", lib_dir);
        println!("cargo:rustc-link-lib=static=siril");

        // Link required system libraries
        link_siril_dependencies();

        return;
    }

    // Try pkg-config for system-installed Siril
    if try_pkg_config() {
        return;
    }

    // Fall back to trying to find libraries in common locations
    try_find_libraries();
}

#[cfg(feature = "siril-ffi")]
fn link_siril_dependencies() {
    // Core dependencies that Siril requires

    // cfitsio - FITS file I/O
    println!("cargo:rustc-link-lib=cfitsio");

    // GSL - GNU Scientific Library
    println!("cargo:rustc-link-lib=gsl");
    println!("cargo:rustc-link-lib=gslcblas");

    // wcslib - World Coordinate System
    println!("cargo:rustc-link-lib=wcs");

    // GLib (usually dynamic)
    #[cfg(target_os = "macos")]
    {
        // On macOS, GLib is typically installed via Homebrew
        if let Ok(brew_prefix) = std::process::Command::new("brew")
            .args(["--prefix", "glib"])
            .output()
        {
            if brew_prefix.status.success() {
                let prefix = String::from_utf8_lossy(&brew_prefix.stdout)
                    .trim()
                    .to_string();
                println!("cargo:rustc-link-search=native={}/lib", prefix);
            }
        }
        println!("cargo:rustc-link-lib=glib-2.0");
        println!("cargo:rustc-link-lib=gobject-2.0");
    }

    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-lib=glib-2.0");
        println!("cargo:rustc-link-lib=gobject-2.0");
    }

    // FFTW3
    println!("cargo:rustc-link-lib=fftw3");
    println!("cargo:rustc-link-lib=fftw3f");

    // lcms2 - Color management
    println!("cargo:rustc-link-lib=lcms2");

    // OpenCV (optional, for advanced registration)
    // println!("cargo:rustc-link-lib=opencv_core");
    // println!("cargo:rustc-link-lib=opencv_imgproc");
}

#[cfg(feature = "siril-ffi")]
fn try_pkg_config() -> bool {
    // Try to use pkg-config to find Siril or its dependencies
    // Siril doesn't typically install a .pc file, but dependencies do

    let deps = [
        "cfitsio",
        "gsl",
        "wcslib",
        "glib-2.0",
        "fftw3",
        "fftw3f",
        "lcms2",
    ];

    for dep in &deps {
        if let Err(e) = pkg_config_probe(dep) {
            eprintln!("Warning: pkg-config failed for {}: {}", dep, e);
            // Don't fail here - we might still be able to link
        }
    }

    false // Return false to indicate we didn't find a complete Siril install
}

#[cfg(feature = "siril-ffi")]
fn pkg_config_probe(name: &str) -> Result<(), String> {
    // Simple pkg-config probe
    let output = std::process::Command::new("pkg-config")
        .args(["--libs", "--cflags", name])
        .output()
        .map_err(|e| format!("Failed to run pkg-config: {}", e))?;

    if output.status.success() {
        let flags = String::from_utf8_lossy(&output.stdout);
        for flag in flags.split_whitespace() {
            if flag.starts_with("-L") {
                println!("cargo:rustc-link-search=native={}", &flag[2..]);
            } else if flag.starts_with("-l") {
                println!("cargo:rustc-link-lib={}", &flag[2..]);
            }
        }
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

#[cfg(feature = "siril-ffi")]
fn try_find_libraries() {
    // Try common library locations

    #[cfg(target_os = "macos")]
    {
        // Homebrew locations
        let brew_lib = "/opt/homebrew/lib";
        let brew_lib_x86 = "/usr/local/lib";

        if std::path::Path::new(brew_lib).exists() {
            println!("cargo:rustc-link-search=native={}", brew_lib);
        }
        if std::path::Path::new(brew_lib_x86).exists() {
            println!("cargo:rustc-link-search=native={}", brew_lib_x86);
        }
    }

    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-search=native=/usr/lib");
        println!("cargo:rustc-link-search=native=/usr/local/lib");
        println!("cargo:rustc-link-search=native=/usr/lib/x86_64-linux-gnu");
    }
}
