//! Fixture-driven tests for `log_mcp::query` against hand-authored JSONL,
//! matching the as-built envelope emitted by `tracing-subscriber`'s json
//! layer (verified against the real dependency versions, not assumed from
//! docs): UPPERCASE `level`; `span`/`spans` absent outside any span; a
//! span-close event has `fields.message == "close"` plus `fields."time.busy"`
//! and an *empty* `spans` array (the closing span's own fields only survive
//! in `span`).

use log_mcp::query::{self, Filter};
use std::io::Write;
use std::path::PathBuf;

/// One scan operation (`root_id` 1): an info progress event, an error
/// event (both inside the `scan` span), an unrelated out-of-span debug
/// event, a span-close event carrying `time.busy`, and one malformed line
/// the parser must skip without crashing.
const FIXTURE_LINES: &[&str] = &[
    r#"{"timestamp":"2026-07-03T10:00:00.000000Z","level":"INFO","target":"athenaeum_core::scanner","fields":{"message":"scan progress","files_found":42},"span":{"root_id":1,"name":"scan"},"spans":[{"root_id":1,"name":"scan"}]}"#,
    r#"{"timestamp":"2026-07-03T10:00:01.000000Z","level":"ERROR","target":"athenaeum_core::scanner","fields":{"message":"failed to parse frame header","error":"unexpected EOF"},"span":{"root_id":1,"name":"scan"},"spans":[{"root_id":1,"name":"scan"}]}"#,
    r#"{"timestamp":"2026-07-03T10:00:02.000000Z","level":"DEBUG","target":"athenaeum_core::settings","fields":{"message":"loaded setting","key":"grouping.threshold.value"}}"#,
    r#"{"timestamp":"2026-07-03T10:00:03.000000Z","level":"INFO","target":"athenaeum_core::scanner","fields":{"message":"close","time.busy":"1.23ms","time.idle":"4.56µs"},"span":{"root_id":1,"name":"scan"},"spans":[]}"#,
    // Malformed: truncated JSON — must be skipped, not crash the scan.
    r#"{"timestamp": "2026-07-03T10:00:04.000000Z", "level": "INFO", broken"#,
];

fn dirs(dir: &tempfile::TempDir) -> Vec<PathBuf> {
    vec![dir.path().to_path_buf()]
}

fn write_fixture() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("athenaeum-desktop.2026-07-03.jsonl");
    let mut f = std::fs::File::create(&path).expect("create fixture file");
    for line in FIXTURE_LINES {
        writeln!(f, "{line}").expect("write fixture line");
    }
    dir
}

#[test]
fn query_logs_level_error_matches_one() {
    let dir = write_fixture();
    let f = Filter {
        level: Some("error".to_string()),
        limit: query::clamp_limit(None),
        ..Default::default()
    };
    let results = query::scan(&dirs(&dir), &f).expect("scan");
    assert_eq!(
        results.len(),
        1,
        "expected exactly one ERROR-severity event"
    );
    assert_eq!(
        results[0]
            .pointer("/fields/message")
            .and_then(|v| v.as_str()),
        Some("failed to parse frame header")
    );
}

#[test]
fn get_operation_returns_every_event_in_the_scan_span() {
    let dir = write_fixture();
    let results = query::get_operation(&dirs(&dir), "1", query::MAX_LIMIT).expect("get_operation");
    // info progress + error + close = 3; the unrelated debug event and the
    // malformed line must not appear.
    assert_eq!(
        results.len(),
        3,
        "expected the 3 events belonging to root_id 1"
    );
    let messages: Vec<&str> = results
        .iter()
        .filter_map(|v| v.pointer("/fields/message").and_then(|m| m.as_str()))
        .collect();
    assert!(messages.contains(&"scan progress"));
    assert!(messages.contains(&"failed to parse frame header"));
    assert!(messages.contains(&"close"));
}

#[test]
fn tail_logs_returns_all_valid_lines() {
    let dir = write_fixture();
    let results = query::tail(&dirs(&dir), 10).expect("tail");
    // 4 valid lines parsed; the 5th (malformed) line is silently skipped.
    assert_eq!(results.len(), 4);
}

#[test]
fn list_operations_projects_the_close_event() {
    let dir = write_fixture();
    let ops = query::list_operations(&dirs(&dir), None, None, query::clamp_limit(None))
        .expect("list_operations");
    assert_eq!(ops.len(), 1, "expected exactly one completed operation");
    let op = &ops[0];
    assert_eq!(op.kind, "scan");
    assert_eq!(op.id, "1");
    assert_eq!(op.timestamp, "2026-07-03T10:00:03.000000Z");
    assert_eq!(op.duration.as_deref(), Some("1.23ms"));
}

#[test]
fn list_operations_kind_filter_excludes_other_kinds() {
    let dir = write_fixture();
    let ops = query::list_operations(
        &dirs(&dir),
        Some("archive_op"),
        None,
        query::clamp_limit(None),
    )
    .expect("list_operations");
    assert!(ops.is_empty(), "no archive_op spans in the fixture");
}

#[test]
fn malformed_line_does_not_abort_the_scan() {
    let dir = write_fixture();
    // Any of the calls above already exercises this, but assert directly:
    // scanning the fixture dir must succeed (not Err) despite line 5.
    let f = Filter {
        limit: query::clamp_limit(None),
        ..Default::default()
    };
    assert!(query::scan(&dirs(&dir), &f).is_ok());
}

#[test]
fn list_operations_includes_registration_span_close() {
    // Registration is a sync-fn span opened in `athenaeum_core::registration::
    // service::register_frame_set` (`info_span!("registration", frame_set_id
    // = frames_set_id)`), entered — not `.instrument()`-attached to a future —
    // so its close event has the same shape as any other operation span.
    // Own fixture file/dir (not the shared `FIXTURE_LINES`) so this doesn't
    // perturb the other tests' fixed operation counts.
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("athenaeum-desktop.2026-07-03.jsonl");
    let mut f = std::fs::File::create(&path).expect("create fixture file");
    writeln!(
        f,
        r#"{{"timestamp":"2026-07-03T11:00:00.000000Z","level":"INFO","target":"athenaeum_core::registration::service","fields":{{"message":"close","time.busy":"842ms","time.idle":"1.02µs"}},"span":{{"frame_set_id":7,"name":"registration"}},"spans":[]}}"#
    )
    .expect("write fixture line");

    let ops = query::list_operations(
        &dirs(&dir),
        Some("registration"),
        None,
        query::clamp_limit(None),
    )
    .expect("list_operations");
    assert_eq!(
        ops.len(),
        1,
        "expected the registration span-close to be listed"
    );
    let op = &ops[0];
    assert_eq!(op.kind, "registration");
    assert_eq!(op.id, "7");
    assert_eq!(op.duration.as_deref(), Some("842ms"));
}

#[test]
fn tail_logs_truncates_to_last_n() {
    // Regression test: tail must return the LAST N lines, not the FIRST N.
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("athenaeum-desktop.2026-07-03.jsonl");
    let mut f = std::fs::File::create(&path).expect("create fixture file");

    // Write 5 matching lines with messages m1..m5 (same level/target for filtering).
    for i in 1..=5 {
        let line = format!(
            r#"{{"timestamp":"2026-07-03T10:00:{:02}.000000Z","level":"INFO","target":"athenaeum_core::test","fields":{{"message":"m{i}"}}}}"#,
            i - 1
        );
        writeln!(f, "{line}").expect("write fixture line");
    }

    // Scan with limit=2; should return the last 2 lines only.
    let results = query::tail(&dirs(&dir), 2).expect("tail");
    assert_eq!(results.len(), 2, "expected exactly 2 lines");

    let messages: Vec<&str> = results
        .iter()
        .filter_map(|v| v.pointer("/fields/message").and_then(|m| m.as_str()))
        .collect();
    assert_eq!(messages, vec!["m4", "m5"], "expected [m4, m5] in order");
}

#[test]
fn scan_merges_files_across_multiple_dirs_and_skips_missing() {
    let prod = tempfile::tempdir().expect("tempdir");
    let dev = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        prod.path().join("athenaeum-desktop.2026-08-08.jsonl"),
        r#"{"timestamp":"2026-08-08T10:00:00.000000Z","level":"INFO","target":"athenaeum_core::scanner","fields":{"message":"prod event"}}"#,
    )
    .expect("write prod fixture");
    std::fs::write(
        dev.path().join("athenaeum-desktop.2026-08-09.jsonl"),
        r#"{"timestamp":"2026-08-09T10:00:00.000000Z","level":"INFO","target":"athenaeum_core::scanner","fields":{"message":"dev event"}}"#,
    )
    .expect("write dev fixture");

    let both = vec![prod.path().to_path_buf(), dev.path().to_path_buf()];
    let results = query::tail(&both, 10).expect("tail");
    assert_eq!(results.len(), 2, "events from both dirs: {results:?}");
    // chronological merge ⇒ the older event comes first regardless of which dir owns it
    assert_eq!(results[0]["fields"]["message"], "prod event");
    assert_eq!(results[1]["fields"]["message"], "dev event");

    // a missing dir (the .dev tree before the first dev run) is skipped, never an error
    let with_missing = vec![
        prod.path().to_path_buf(),
        PathBuf::from("/nonexistent-athenaeum-dev-tree"),
    ];
    assert_eq!(query::tail(&with_missing, 10).expect("tail").len(), 1);
}

#[test]
fn tail_merges_same_named_files_from_both_trees_chronologically() {
    // Both build flavors write the SAME filename — `Process::prefix()` is
    // "athenaeum-desktop" regardless of build profile — so on a day both ran,
    // the two trees hold a same-named daily file. Concatenating them (all of
    // one tree's day, then all of the other's) and keeping the last `limit`
    // in stream order biases the ring to whichever dir sorts last; the merge
    // must be by timestamp so `tail` really returns the newest N events.
    let a = tempfile::tempdir().expect("tempdir");
    let b = tempfile::tempdir().expect("tempdir");
    let name = "athenaeum-desktop.2026-08-08.jsonl";
    let event = |ts: &str, msg: &str| {
        format!(
            r#"{{"timestamp":"{ts}","level":"INFO","target":"athenaeum_core::scanner","fields":{{"message":"{msg}"}}}}"#
        )
    };
    std::fs::write(
        a.path().join(name),
        format!(
            "{}\n{}\n",
            event("2026-08-08T10:00:00.000000Z", "a-10:00"),
            event("2026-08-08T12:00:00.000000Z", "a-12:00")
        ),
    )
    .expect("write dir-a fixture");
    std::fs::write(
        b.path().join(name),
        format!("{}\n", event("2026-08-08T11:00:00.000000Z", "b-11:00")),
    )
    .expect("write dir-b fixture");

    let both = vec![a.path().to_path_buf(), b.path().to_path_buf()];
    let results = query::tail(&both, 2).expect("tail");
    let messages: Vec<&str> = results
        .iter()
        .filter_map(|v| v.pointer("/fields/message").and_then(|m| m.as_str()))
        .collect();
    assert_eq!(
        messages,
        vec!["b-11:00", "a-12:00"],
        "expected the 2 newest events across both trees, in chronological order"
    );
}
