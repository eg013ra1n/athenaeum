# Vendor Directory

Reserved for prebuilt static libraries on platforms where building from source is complex.

## Structure

- `prebuilt/<os>-<arch>/` - Platform-specific libraries (placeholders only; no build step reads this directory today)

## Supported Platforms

| Directory | OS | Architecture |
|-----------|-------|--------------|
| `macos-aarch64` | macOS | Apple Silicon (M1/M2/M3) |
| `macos-x86_64` | macOS | Intel |
| `linux-x86_64` | Linux | x86_64 |
| `windows-x86_64` | Windows | x86_64 |

## History

This directory used to hold a prebuilt static library of a third-party stacking
library for an FFI integration that has since been retired. The old build instructions
are dropped; no cargo feature, build script or environment variable references this
directory now.
