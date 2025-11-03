fn main() {
    // Set LibVIPS library path for macOS
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-search=/opt/homebrew/lib");
        println!("cargo:rustc-link-search=/opt/homebrew/opt/vips/lib");
    }

    tauri_build::build()
}
