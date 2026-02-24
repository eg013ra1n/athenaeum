# Vendor Directory

This directory contains prebuilt static libraries for platforms where building from source is complex.

## Structure

- `prebuilt/<os>-<arch>/` - Platform-specific libraries
  - `libsiril.a` (Unix) or `siril.lib` (Windows)
  - Optional: dependency libraries (cfitsio, gsl, etc.)

## Supported Platforms

| Directory | OS | Architecture |
|-----------|-------|--------------|
| `macos-aarch64` | macOS | Apple Silicon (M1/M2/M3) |
| `macos-x86_64` | macOS | Intel |
| `linux-x86_64` | Linux | x86_64 |
| `windows-x86_64` | Windows | x86_64 |

## Building the Siril Static Library

### macOS / Linux

```bash
# Install dependencies first
# macOS: brew install cfitsio gsl wcslib fftw lcms2 meson ninja
# Linux: apt install libcfitsio-dev libgsl-dev wcslib-dev libfftw3-dev liblcms2-dev meson ninja-build

# Navigate to Siril source
cd src/stacking

# Configure with meson (static library, no GUI)
meson setup build \
    --default-library=static \
    --buildtype=release \
    -Denable-gui=false

# Compile
meson compile -C build

# Copy the library to vendor directory
# For Apple Silicon:
cp build/src/libsiril.a ../src-tauri/vendor/prebuilt/macos-aarch64/

# For Intel Mac:
cp build/src/libsiril.a ../src-tauri/vendor/prebuilt/macos-x86_64/

# For Linux:
cp build/src/libsiril.a ../src-tauri/vendor/prebuilt/linux-x86_64/
```

### Windows

Windows builds are more complex. Options:

1. **Using vcpkg (Recommended)**
   - Dependencies are managed by vcpkg (see `src-tauri/vcpkg.json`)
   - Set `VCPKG_ROOT` environment variable
   - The build script will find libraries automatically

2. **Manual MSVC Build**
   - Install Visual Studio with C++ tools
   - Install dependencies via vcpkg
   - Configure with meson using Visual Studio backend
   ```powershell
   meson setup build --backend=vs2022 --default-library=static -Denable-gui=false
   meson compile -C build
   cp build/src/siril.lib ../src-tauri/vendor/prebuilt/windows-x86_64/
   ```

3. **Using Prebuilt Binaries**
   - Download from Siril releases or CI artifacts
   - Place `siril.lib` in `vendor/prebuilt/windows-x86_64/`

## Alternative: Environment Variable

Instead of placing libraries in this directory, you can set the `SIRIL_LIB_DIR` environment variable to point to the directory containing the built library:

```bash
export SIRIL_LIB_DIR=/path/to/siril/build/src
cargo build --features siril-ffi
```

## Verifying the Library

To verify the library contains expected symbols:

```bash
# Unix
nm -g vendor/prebuilt/macos-aarch64/libsiril.a | grep readfits
nm -g vendor/prebuilt/macos-aarch64/libsiril.a | grep stack_median

# Windows (from VS Developer Command Prompt)
dumpbin /symbols vendor/prebuilt/windows-x86_64/siril.lib | findstr readfits
```
