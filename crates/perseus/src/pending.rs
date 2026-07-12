//! The "To sync" tree for the web view.
//!
//! Pure derivation: given the batcher's pending accumulator snapshot
//! (`Vec<(capture_dir, file)>` from [`crate::batcher::BatcherHandle::pending_snapshot`])
//! and the [`Config`](crate::config::Config), group every queued file into a
//! trie keyed on its [`compute_rel_path`](crate::run::compute_rel_path) segments
//! (object / date / type / file). No transport, no DB — just the shape the web
//! settings page renders as a collapsible tree.

use std::path::PathBuf;

/// One node of the "To sync" tree.
///
/// A node is a directory (or the synthetic root); [`files`](Self::files) holds
/// the rel_path leaf segments (filenames) that terminate directly at this node,
/// and [`children`](Self::children) holds its sub-directories. [`count`](Self::count)
/// is the total number of pending files at or below this node (so the root's
/// `count` is the batch total).
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PendingNode {
    /// This node's path segment. The root node's name is empty (`""`).
    pub name: String,
    /// Total pending files at or below this node.
    pub count: usize,
    /// Sub-directory nodes, sorted by [`name`](Self::name).
    pub children: Vec<PendingNode>,
    /// Filenames that terminate directly at this node, sorted lexically.
    pub files: Vec<String>,
}

impl PendingNode {
    /// A fresh, empty node with the given name and a zero count.
    fn new(name: impl Into<String>) -> Self {
        PendingNode {
            name: name.into(),
            count: 0,
            children: Vec::new(),
            files: Vec::new(),
        }
    }

    /// The existing child named `name`, creating it if absent. Children are left
    /// in insertion order here and sorted once at the end (see [`sort_recursive`]).
    fn child_mut(&mut self, name: &str) -> &mut PendingNode {
        match self.children.iter().position(|c| c.name == name) {
            Some(idx) => &mut self.children[idx],
            None => {
                self.children.push(PendingNode::new(name));
                self.children
                    .last_mut()
                    .expect("just pushed a child, it must be present")
            }
        }
    }
}

/// Group each pending file by its [`compute_rel_path`](crate::run::compute_rel_path)
/// segments (object / date / type / file) into a trie.
///
/// For each `(capture_dir, file)` the file's rel_path is computed, split on `/`,
/// and inserted: every directory node along the path has its `count` incremented,
/// and the final filename segment is pushed into its parent directory node's
/// [`files`](PendingNode::files) (so a directory node lists its own files and has
/// child nodes for its sub-directories). Each node's `children` and `files` are
/// sorted for determinism. The root node has an empty `name` and a `count` equal
/// to the total number of pending files.
pub fn pending_tree(
    snapshot: &[(PathBuf, PathBuf)],
    config: &crate::config::Config,
) -> PendingNode {
    let mut root = PendingNode::new("");

    for (capture_dir, file) in snapshot {
        let rel = crate::run::compute_rel_path(config, capture_dir, file);
        let segments: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            // Defensive: a rel_path can't be empty in practice (compute_rel_path
            // falls back to the basename), but never fabricate a phantom node.
            continue;
        }
        insert(&mut root, &segments);
    }

    sort_recursive(&mut root);
    root
}

/// Insert one file's rel_path `segments` under `node`, incrementing `count` on
/// every node along the way. The last segment (the filename) is pushed into the
/// deepest directory node's `files` rather than becoming its own child.
fn insert(node: &mut PendingNode, segments: &[&str]) {
    node.count += 1;
    match segments {
        [] => {}
        [file] => node.files.push((*file).to_string()),
        [dir, rest @ ..] => insert(node.child_mut(dir), rest),
    }
}

/// Sort `node`'s `children` by name and `files` lexically, then recurse — the one
/// place ordering is imposed, so the trie build itself can stay insertion-ordered.
fn sort_recursive(node: &mut PendingNode) {
    node.children.sort_by(|a, b| a.name.cmp(&b.name));
    node.files.sort();
    for child in &mut node.children {
        sort_recursive(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// A config whose single capture dir is `dir` (no per-dir label) — mirrors the
    /// `single_root_config` helper in `run`'s rel_path tests.
    fn single_root_config(dir: &str) -> Config {
        let mut c = Config::fallback();
        c.capture_dir = Some(PathBuf::from(dir));
        c.capture_dirs = Vec::new();
        c
    }

    #[test]
    fn groups_files_by_rel_path_segments() {
        let cap = PathBuf::from("/data/astro");
        let snap = vec![
            (cap.clone(), cap.join("M31/2026-07-12/lights/L_0001.fits")),
            (cap.clone(), cap.join("M31/2026-07-12/lights/L_0002.fits")),
            (cap.clone(), cap.join("M31/2026-07-12/flats/F_0001.fits")),
        ];
        let cfg = single_root_config("/data/astro");
        let root = pending_tree(&snap, &cfg);
        assert_eq!(root.count, 3);
        let m31 = root.children.iter().find(|n| n.name == "M31").unwrap();
        assert_eq!(m31.count, 3);
        let date = &m31.children[0]; // 2026-07-12
        let lights = date.children.iter().find(|n| n.name == "lights").unwrap();
        assert_eq!(lights.count, 2);
        assert_eq!(lights.files.len(), 2);
    }
}
