//! The running build's own release notes (spec §2.2, §4). The release commit
//! holds `RELEASE_NOTES.md` together with the six version files, so the text
//! embedded at compile time IS the notes of the build that carries it — no
//! network, no channel question.

/// Path is relative to this file: updates/ → src/ → athenaeum-core/ → crates/ → repo root.
pub const EMBEDDED_RELEASE_NOTES: &str = include_str!("../../../../RELEASE_NOTES.md");

/// The docs site's blog slug is the tag with every dot removed
/// (`v0.6.5` → `v065`, `v0.6.5-beta.1` → `v065-beta1`) — the same rule
/// `.gitlab/ci/scripts/verify_release.sh` uses to poll the post.
pub fn blog_url(version: &str) -> String {
    let v = version.trim().trim_start_matches('v');
    format!("https://artfrom.space/blog/v{}/", v.replace('.', ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blog_slug_drops_the_dots() {
        assert_eq!(blog_url("0.6.5"), "https://artfrom.space/blog/v065/");
        assert_eq!(blog_url("0.6.5-beta.1"), "https://artfrom.space/blog/v065-beta1/");
        assert_eq!(blog_url("v0.6.5"), "https://artfrom.space/blog/v065/");
    }

    #[test]
    fn embedded_notes_are_the_release_notes_file() {
        assert!(EMBEDDED_RELEASE_NOTES.starts_with('*'), "notes start with the italic tagline");
        assert!(EMBEDDED_RELEASE_NOTES.contains("## "), "notes carry at least one section");
    }
}
