//! Build script for Athenaeum
//!
//! Captures the git commit hash at compile time and passes it to Rust code
//! via the ATHENAEUM_GIT_HASH environment variable.

fn main() {
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=ATHENAEUM_GIT_HASH={}", hash);
    // .git lives at the repo root, two levels up from this crate. A relative
    // ".git/HEAD" resolves inside the crate dir where the file doesn't exist,
    // and cargo treats a missing watched file as always-stale — forcing a full
    // rebuild+relink of this crate on every build. Only watch it when present
    // (Docker builds have no .git).
    let git_dir = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../.git");
    let head = git_dir.join("HEAD");
    if head.exists() {
        println!("cargo:rerun-if-changed={}", head.display());
        // HEAD only changes on branch switch; new commits move the branch ref
        // instead, so watch it too or the embedded hash goes stale. The ref
        // may be packed (no loose file after git gc) — same missing-file rule
        // applies, so only watch it when present.
        if let Ok(contents) = std::fs::read_to_string(&head) {
            if let Some(ref_path) = contents.trim().strip_prefix("ref: ") {
                let branch_ref = git_dir.join(ref_path);
                if branch_ref.exists() {
                    println!("cargo:rerun-if-changed={}", branch_ref.display());
                }
            }
        }
    }

    // Windows: opt the exe into long paths (>260 chars). Deep generated trees
    // (calibration-library outputs, archive staging/restore temp) plausibly
    // exceed MAX_PATH; with the manifest + the OS LongPathsEnabled policy the
    // Win32 limit lifts. A custom manifest REPLACES Tauri's default, so its
    // Common-Controls dependency is restated verbatim (WebView2 dialogs need it).
    let windows = tauri_build::WindowsAttributes::new().app_manifest(
        r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings xmlns:ws2="http://schemas.microsoft.com/SMI/2016/WindowsSettings">
      <ws2:longPathAware>true</ws2:longPathAware>
    </windowsSettings>
  </application>
</assembly>"#,
    );
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("failed to run tauri-build");
}
