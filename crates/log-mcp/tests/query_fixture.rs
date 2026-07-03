//! Fixture-driven tests for `log_mcp::query` against hand-authored JSONL,
//! matching the as-built envelope emitted by `tracing-subscriber`'s json
//! layer (verified against the real dependency versions, not assumed from
//! docs): UPPERCASE `level`; `span`/`spans` absent outside any span; a
//! span-close event has `fields.message == "close"` plus `fields."time.busy"`
//! and an *empty* `spans` array (the closing span's own fields only survive
//! in `span`).

use log_mcp::query::{self, Filter};
use std::io::Write;

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
    let results = query::scan(dir.path(), &f).expect("scan");
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
    let results = query::get_operation(dir.path(), "1", query::MAX_LIMIT).expect("get_operation");
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
    let results = query::tail(dir.path(), 10).expect("tail");
    // 4 valid lines parsed; the 5th (malformed) line is silently skipped.
    assert_eq!(results.len(), 4);
}

#[test]
fn list_operations_projects_the_close_event() {
    let dir = write_fixture();
    let ops = query::list_operations(dir.path(), None, None, query::clamp_limit(None))
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
        dir.path(),
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
    assert!(query::scan(dir.path(), &f).is_ok());
}
