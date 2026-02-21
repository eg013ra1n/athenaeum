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
    println!("cargo:rerun-if-changed=.git/HEAD");

    tauri_build::build();
}
