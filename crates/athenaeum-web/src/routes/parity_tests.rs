//! The two hosts register the same commands.
//!
//! A Tauri command without a web route (or the reverse) is the "two backends
//! in sync" rule broken; nothing else catches it, because each host compiles
//! on its own. This test reads both registration lists as text — the
//! `generate_handler![…]` list in the desktop crate and the
//! `.route("/api/…")` calls in `build_router` — and compares the names.
//!
//! A name that exists in one host on purpose goes into the allowlist below,
//! with its reason.

use std::collections::BTreeSet;

const TAURI_LIB: &str = include_str!("../../../athenaeum-tauri/src/lib.rs");
const WEB_ROUTES: &str = include_str!("mod.rs");

/// Routes the web host has and the desktop does not need.
const WEB_ONLY: &[&str] = &[
    // The SSE stream itself; the desktop delivers events through Tauri.
    "events",
    // Server-side folder picker; the desktop uses the native dialog.
    "browse_directories",
    // The server's configured export directory (`ATHENAEUM_EXPORT_DIR`).
    "get_export_dir",
    // Entry count of the server's in-memory image cache.
    "get_cache_stats",
    // An image served by URL; the desktop reads the preview over IPC.
    "stacking/master-preview",
];

/// Commands the desktop has and the web host does not. Empty: a
/// desktop-only command still gets a route that answers "not available in
/// web mode", so the frontend sees one error shape.
const TAURI_ONLY: &[&str] = &[];

fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tauri_commands() -> BTreeSet<String> {
    let src = strip_line_comments(TAURI_LIB);
    let start = src
        .find("generate_handler![")
        .expect("generate_handler![ in athenaeum-tauri/src/lib.rs");
    let list = &src[start + "generate_handler![".len()..];
    let end = list.find(']').expect("end of the generate_handler! list");
    list[..end]
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| item.rsplit("::").next().unwrap_or(item).to_string())
        .collect()
}

fn web_routes() -> BTreeSet<String> {
    let src = strip_line_comments(WEB_ROUTES);
    let mut names = BTreeSet::new();
    for call in src.split(".route(").skip(1) {
        // The path is the first string literal of the call.
        let Some(open) = call.find('"') else { continue };
        let rest = &call[open + 1..];
        let Some(close) = rest.find('"') else { continue };
        if let Some(name) = rest[..close].strip_prefix("/api/") {
            names.insert(name.to_string());
        }
    }
    names
}

#[test]
fn both_hosts_register_the_same_commands() {
    let tauri = tauri_commands();
    let web = web_routes();

    // A parser that silently reads nothing would pass the comparison below.
    assert!(tauri.len() > 300, "read only {} Tauri commands", tauri.len());
    assert!(web.len() > 300, "read only {} web routes", web.len());

    let web_only: BTreeSet<String> = WEB_ONLY.iter().map(|s| s.to_string()).collect();
    let tauri_only: BTreeSet<String> = TAURI_ONLY.iter().map(|s| s.to_string()).collect();

    let missing_in_web: Vec<&String> =
        tauri.difference(&web).filter(|n| !tauri_only.contains(*n)).collect();
    let missing_in_tauri: Vec<&String> =
        web.difference(&tauri).filter(|n| !web_only.contains(*n)).collect();

    assert!(
        missing_in_web.is_empty(),
        "Tauri commands with no /api/ route in routes/mod.rs: {missing_in_web:?}"
    );
    assert!(
        missing_in_tauri.is_empty(),
        "web routes with no Tauri command in generate_handler!: {missing_in_tauri:?}"
    );

    // An allowlist entry that stopped being true is a stale excuse.
    let stale: Vec<&String> = web_only
        .iter()
        .filter(|n| !web.contains(*n) || tauri.contains(*n))
        .chain(tauri_only.iter().filter(|n| !tauri.contains(*n) || web.contains(*n)))
        .collect();
    assert!(stale.is_empty(), "allowlist entries that no longer hold: {stale:?}");
}
