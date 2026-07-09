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

    tauri_build::build();
}
