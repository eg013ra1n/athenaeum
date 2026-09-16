//! Embeds the git commit hash as ATHENAEUM_GIT_HASH for the update check's
//! telemetry query (the desktop crate does the same in its own build.rs).
//! Docker builds have no .git → "unknown".

fn main() {
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=ATHENAEUM_GIT_HASH={hash}");
    let git_dir = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../.git");
    let head = git_dir.join("HEAD");
    if head.exists() {
        println!("cargo:rerun-if-changed={}", head.display());
        if let Ok(contents) = std::fs::read_to_string(&head) {
            if let Some(ref_path) = contents.trim().strip_prefix("ref: ") {
                let branch_ref = git_dir.join(ref_path);
                if branch_ref.exists() {
                    println!("cargo:rerun-if-changed={}", branch_ref.display());
                }
            }
        }
    }
}
