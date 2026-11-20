//! The MCP server against a real catalog, spoken to as a client over `tokio::io::duplex` on both protocol
//! revisions (MCP plan, Task 5).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rmcp::model::{CallToolRequestParams, CallToolResult, ProtocolVersion};
use rmcp::service::RunningService;
use rmcp::{ClientLifecycleMode, RoleClient};
use rusqlite::Connection;
use serde_json::{json, Value};

use super::server::INSTRUCTIONS;
use super::{mcp_server_name, HostKind, McpHost, McpIdentity};
use crate::db;
use crate::events::NullEmitter;
use crate::models::{File, FileFormat, Frame, ImageType};
use crate::services::ServiceContext;

// ---- the catalog ----

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn at(date: &str) -> Option<DateTime<Utc>> {
    Some(DateTime::parse_from_rfc3339(date).expect("an RFC 3339 date").with_timezone(&Utc))
}

/// One catalog row: a file and its frame.
#[derive(Clone)]
struct Row {
    kind: ImageType,
    object: Option<&'static str>,
    filter: Option<&'static str>,
    exptime: f64,
    size: i64,
    header: Option<&'static str>,
}

fn light(filter: &'static str) -> Row {
    Row { kind: ImageType::Light, object: Some("IC 5146"), filter: Some(filter), exptime: 300.0, size: 100, header: None }
}

/// Returns (file id, frame id).
fn put(conn: &Connection, dir: &Path, name: &str, row: &Row) -> (i64, i64) {
    let file_id = db::insert_file(
        conn,
        &File {
            id: None,
            path: text(&dir.join(name)),
            filename: name.to_string(),
            size: row.size,
            modified_at: Utc::now(),
            format: FileFormat::FITS,
            created_at: Utc::now(),
            content_hash: None,
            archived_in_operation: None,
            archive_zip_path: None,
            archive_path_in_zip: None,
            uuid: None,
            updated_at: None,
        },
    )
    .expect("file");
    let frame_id = db::insert_frame(
        conn,
        &Frame {
            file_id,
            object: row.object.map(str::to_string),
            instrume: Some("CamA".to_string()),
            telescop: Some("ScopeA".to_string()),
            filter: row.filter.map(str::to_string),
            imagetyp: Some(row.kind.clone()),
            exptime: Some(row.exptime),
            date_obs: at("2024-10-05T22:00:00Z"),
            ..Default::default()
        },
    )
    .expect("frame");
    if let Some(header) = row.header {
        conn.execute(
            "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, ?2, ?3)",
            rusqlite::params![file_id, header, crate::fingerprint::compute_header_fingerprint(header)],
        )
        .expect("header");
    }
    (file_id, frame_id)
}

struct Rig {
    host: Arc<McpHost>,
    ctx: Arc<ServiceContext>,
    base: PathBuf,
    // Fields drop in declaration order: the pool and the reader holding `catalog.db` open go before its folder.
    _tmp: tempfile::TempDir,
}

fn identity() -> McpIdentity {
    McpIdentity { server_name: mcp_server_name(), version: "9.9.9-test", kind: HostKind::Desktop, web_api_key_set: false }
}

fn host_over(ctx: &Arc<ServiceContext>) -> Arc<McpHost> {
    McpHost::new(Arc::clone(ctx), identity(), Arc::new(NullEmitter), Arc::new(|| {}), Arc::new(|| true))
}

/// A catalog with: six IC 5146 lights (three H, three O, 300 s) in one frame set that also holds a seventh light now
/// in the Black Hole; one light with no object; one flat on two drives (a header duplicate); a dark.
fn rig(open: bool) -> Rig {
    let (tmp, base) = crate::test_support::canonical_tempdir();
    let ctx = Arc::new(ServiceContext::new_for_tests(base.join("catalog.db")));
    {
        let database = ctx.db.get().expect("db");
        let conn = database.conn();
        db::upsert_scan_root(&conn, &text(&base.join("nas")), "normal").expect("root");
        let night1 = base.join("nas").join("night1");
        let mut members = Vec::new();
        for i in 0..3 {
            members.push(put(&conn, &night1, &format!("h{i}.fits"), &light("H")).1);
        }
        for i in 0..3 {
            members.push(put(&conn, &night1, &format!("o{i}.fits"), &light("O")).1);
        }
        let (bh_file, bh_frame) = put(&conn, &night1, "bh.fits", &light("H"));
        members.push(bh_frame);
        conn.execute(
            "INSERT INTO black_hole (file_id, from_where, moved_at, original_path) VALUES (?1, 'test', 'now', 'x')",
            [bh_file],
        )
        .expect("black hole");

        let set = db::create_frames_set(&conn, Some("IC 5146 set"), false, None, None, None, None, Some(2100.0), None, None, None)
            .expect("set");
        let night = db::create_imaging_night(&conn, set, "2024-10-05T00:00:00Z", "2024-10-05T12:00:00Z").expect("night");
        let session = db::create_session(&conn, night, "CamA", members.len() as i32, None).expect("session");
        db::insert_session_members(&conn, session, &members).expect("members");

        put(&conn, &base.join("nas").join("night2"), "noobj.fits", &Row { object: None, ..light("H") });
        let flat = Row { kind: ImageType::Flat, object: None, filter: Some("H"), exptime: 2.0, size: 200, header: Some("HDR-FLAT") };
        put(&conn, &base.join("nas").join("dup_a"), "flat1.fits", &flat);
        put(&conn, &base.join("nas").join("dup_b"), "flat1.fits", &flat);
        put(&conn, &base.join("nas").join("darks"), "dark1.fits", &Row { kind: ImageType::Dark, object: None, filter: None, exptime: 300.0, size: 300, header: None });
        db::set_setting(&conn, "test.marker", "before").expect("setting");
    }
    let host = host_over(&ctx);
    if open {
        host.on_catalog_open().expect("the reader opens");
    }
    Rig { host, ctx, base, _tmp: tmp }
}

// ---- the clients ----

type Client = RunningService<RoleClient, ()>;

async fn connect(host: &Arc<McpHost>, mode: ClientLifecycleMode) -> Client {
    let (server_end, client_end) = tokio::io::duplex(1 << 20);
    let server = host.server();
    tokio::spawn(async move {
        let running = rmcp::serve_server(server, server_end).await.expect("the server starts");
        let _ = running.waiting().await;
    });
    rmcp::serve_client_with_lifecycle((), client_end, mode).await.expect("the client connects")
}

async fn legacy(host: &Arc<McpHost>) -> Client {
    connect(host, ClientLifecycleMode::Initialize).await
}

async fn ask(client: &Client, tool: &str, args: Value) -> CallToolResult {
    let mut params = CallToolRequestParams::new(tool.to_string());
    if let Value::Object(map) = args {
        params = params.with_arguments(map);
    }
    client.call_tool(params).await.expect("the call is answered")
}

fn text_of(r: &CallToolResult) -> String {
    r.content.iter().filter_map(|c| c.as_text()).map(|t| t.text.clone()).collect::<Vec<_>>().join("\n")
}

fn data_of(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("a structured half")
}

/// Every object with a `handle` also has `fileName` and `folder`.
fn assert_handles_are_named(v: &Value, whence: &str) {
    match v {
        Value::Object(map) => {
            if map.contains_key("handle") {
                assert!(map.contains_key("fileName") && map.contains_key("folder"), "{whence}: a handle without its file name and folder: {v}");
            }
            for (k, x) in map {
                assert_handles_are_named(x, &format!("{whence}.{k}"));
            }
        }
        Value::Array(items) => items.iter().enumerate().for_each(|(i, x)| assert_handles_are_named(x, &format!("{whence}[{i}]"))),
        _ => {}
    }
}

const TOOLS: [&str; 5] = ["catalog_overview", "file_details", "find_duplicates", "find_missing_metadata", "query_frames"];

// ---- tests ----

#[tokio::test]
async fn both_revisions_list_the_same_tools() {
    let rig = rig(true);
    let mut seen: Vec<Vec<String>> = Vec::new();
    for mode in [
        ClientLifecycleMode::Initialize,
        ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] },
    ] {
        let client = connect(&rig.host, mode).await;
        let mut names: Vec<String> = client.list_all_tools().await.expect("tools").into_iter().map(|t| t.name.to_string()).collect();
        names.sort();
        seen.push(names);
        let _ = client.cancel().await;
    }
    assert_eq!(seen[0], TOOLS.map(String::from).to_vec());
    assert_eq!(seen[0], seen[1], "both protocol revisions list the same tools");
    assert!(seen[0].iter().all(|n| !n.contains("apply")), "no tool applies a plan: {:?}", seen[0]);
}

#[tokio::test]
async fn the_server_introduces_itself() {
    let rig = rig(true);
    let client = legacy(&rig.host).await;
    let info = client.peer_info().expect("server info");
    let server = info.server_info.as_ref().expect("the server names itself");
    assert_eq!(server.name, mcp_server_name());
    assert_eq!(server.version, "9.9.9-test");
    assert_eq!(info.instructions.as_deref(), Some(INSTRUCTIONS));
    assert!(info.capabilities.tools.is_some());
}

#[tokio::test]
async fn every_answer_names_its_build_and_catalog() {
    let rig = rig(true);
    let client = legacy(&rig.host).await;
    for (tool, args) in [
        ("catalog_overview", json!({})),
        ("query_frames", json!({})),
        ("find_duplicates", json!({})),
        ("find_missing_metadata", json!({})),
    ] {
        let r = ask(&client, tool, args).await;
        assert_eq!(r.is_error, Some(false), "{tool}: {}", text_of(&r));
        assert!(text_of(&r).starts_with("[athenaeum-dev 9.9.9-test · catalog "), "{tool}: {}", text_of(&r));
        let data = data_of(&r);
        assert!(data["source"]["catalog"].as_str().expect("catalog").ends_with("catalog.db"), "{tool}");
        assert_eq!(data["source"]["build"], "athenaeum-dev");
        assert_eq!(data["source"]["version"], "9.9.9-test");
    }
}

#[tokio::test]
async fn both_halves_read_on_their_own() {
    let rig = rig(true);
    let client = legacy(&rig.host).await;

    let dups = ask(&client, "find_duplicates", json!({})).await;
    let said = text_of(&dups);
    assert!(said.contains("2 copies"), "{said}");
    for dir in ["dup_a", "dup_b"] {
        let folder = text(&rig.base.join("nas").join(dir));
        assert!(said.contains(&format!("flat1.fits in {folder}")), "{said}");
    }
    let data = data_of(&dups);
    assert_eq!(data["groupsInScope"], 1);
    assert_eq!(data["groups"][0]["copies"].as_array().expect("copies").len(), 2);
    assert_handles_are_named(&data, "find_duplicates");

    let files = ask(&client, "query_frames", json!({ "scope": { "filter": "H" }, "filesPage": 0, "calibration": true, "storage": true, "quality": "fwhm" })).await;
    assert_eq!(files.is_error, Some(false), "{}", text_of(&files));
    assert_handles_are_named(&data_of(&files), "query_frames");
    assert!(text_of(&files).contains("h0.fits in "), "{}", text_of(&files));
    assert!(!text_of(&files).contains("file:"), "handles are not for reading: {}", text_of(&files));

    let missing = ask(&client, "find_missing_metadata", json!({ "categories": ["object"] })).await;
    assert_handles_are_named(&data_of(&missing), "find_missing_metadata");
    assert!(text_of(&missing).contains("noobj.fits"), "{}", text_of(&missing));

    let overview = ask(&client, "catalog_overview", json!({})).await;
    assert_handles_are_named(&data_of(&overview), "catalog_overview");

    let details = ask(&client, "file_details", json!({ "file": text(&rig.base.join("nas").join("night1").join("h0.fits")) })).await;
    assert_eq!(details.is_error, Some(false), "{}", text_of(&details));
    assert_handles_are_named(&data_of(&details), "file_details");
    assert!(text_of(&details).contains("h0.fits in "), "{}", text_of(&details));
}

fn digest(conn: &Connection) -> String {
    let mut out = String::new();
    for table in ["files", "frames", "settings", "black_hole", "frame_analysis"] {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table} ORDER BY rowid")).expect("prepare");
        let columns = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                let mut cells = Vec::new();
                for i in 0..columns {
                    cells.push(format!("{:?}", r.get::<_, rusqlite::types::Value>(i)?));
                }
                Ok(cells.join("|"))
            })
            .expect("rows");
        out.push_str(&format!("{table}\n"));
        for row in rows {
            out.push_str(&row.expect("row"));
            out.push('\n');
        }
    }
    out
}

#[tokio::test]
async fn questions_change_nothing() {
    let rig = rig(true);
    let client = legacy(&rig.host).await;
    let digest_now = || {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        digest(&conn)
    };
    let before = digest_now();
    assert!(before.contains("h0.fits") && before.contains("before"), "the digest sees the catalog");

    let scopes = [
        json!({}),
        json!({ "object": "IC 5146", "frameType": "lights", "state": ["catalog", "black_hole"] }),
        json!({ "frameSet": "IC 5146 set", "night": "2024-10-05" }),
    ];
    let one_file = text(&rig.base.join("nas").join("night1").join("h0.fits"));
    for scope in scopes {
        for (tool, args) in [
            ("catalog_overview", json!({})),
            ("query_frames", json!({ "scope": scope, "groupBy": ["filter", "night"], "filesPage": 0, "quality": "hfr", "calibration": true, "storage": true })),
            ("find_duplicates", json!({ "scope": scope })),
            ("find_missing_metadata", json!({ "scope": scope })),
            ("file_details", json!({ "file": one_file })),
        ] {
            let r = ask(&client, tool, args).await;
            assert_eq!(r.is_error, Some(false), "{tool}: {}", text_of(&r));
            assert_eq!(digest_now(), before, "{tool} changed the catalog");
        }
    }
}

#[tokio::test]
async fn reader_refuses_a_write() {
    let rig = rig(true);
    {
        let reader = rig.host.reader().expect("the reader");
        let refused = reader.execute("INSERT INTO settings (key, value) VALUES ('test.refused', 'x')", []);
        assert!(refused.is_err(), "the reader wrote");
        let refused = reader.execute("DELETE FROM files", []);
        assert!(refused.is_err(), "the reader deleted");
    }
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    db::set_setting(&conn, "test.after", "written").expect("the pool still writes");
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM settings WHERE key = 'test.refused'", [], |r| r.get(0)).expect("count");
    assert_eq!(n, 0);
    // The reader sees what the pool committed.
    let seen: String = rig
        .host
        .reader()
        .expect("the reader")
        .query_row("SELECT value FROM settings WHERE key = 'test.after'", [], |r| r.get(0))
        .expect("the reader reads the committed write");
    assert_eq!(seen, "written");
}

#[tokio::test]
async fn path_arguments_are_looked_up_not_opened() {
    let rig = rig(true);
    let client = legacy(&rig.host).await;
    let outside = rig.base.join("outside").join("never-opened.fits");

    let r = ask(&client, "file_details", json!({ "file": text(&outside) })).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text_of(&r).contains("no file") && text_of(&r).contains("in the catalog"), "{}", text_of(&r));

    let r = ask(&client, "query_frames", json!({ "scope": { "folder": text(&rig.base.join("outside")) } })).await;
    assert_eq!(r.is_error, Some(false), "{}", text_of(&r));
    assert!(text_of(&r).contains("Totals: no frames."), "{}", text_of(&r));
    assert_eq!(data_of(&r)["totals"]["frames"], 0);

    assert!(!rig.base.join("outside").exists(), "a path the assistant gave was touched");
}

#[tokio::test]
async fn scope_that_matches_nothing_names_nearest_values() {
    let rig = rig(true);
    let client = legacy(&rig.host).await;
    let r = ask(&client, "query_frames", json!({ "scope": { "camera": "CamB" }, "groupBy": ["filter"] })).await;
    assert_eq!(r.is_error, Some(false), "a scope that names nothing is an answer: {}", text_of(&r));
    let said = text_of(&r);
    assert!(said.contains("no camera 'CamB'") && said.contains("'CamA'"), "{said}");
    let data = data_of(&r);
    assert_eq!(data["frames"], 0);
    assert_eq!(data["hints"][0]["existing"][0], "CamA");
    // It ran nothing else: no totals, no groups.
    assert!(data.get("totals").is_none() && data.get("groups").is_none());

    let r = ask(&client, "find_duplicates", json!({ "scope": { "filter": "Ha" } })).await;
    assert!(text_of(&r).contains("no filter 'Ha'") && text_of(&r).contains("'H'"), "{}", text_of(&r));
}

#[tokio::test]
async fn a_failure_is_an_error_result_in_words() {
    // The catalog is not open yet: a question is refused in words, with line 1 saying so.
    let early = rig(false);
    assert!(early.host.reader().is_err() && early.host.source().is_err());
    let client = legacy(&early.host).await;
    for (tool, args) in [("catalog_overview", json!({})), ("query_frames", json!({})), ("file_details", json!({ "file": "file:1" }))] {
        let r = ask(&client, tool, args).await;
        assert_eq!(r.is_error, Some(true), "{tool}");
        let said = text_of(&r);
        assert!(said.contains("starting"), "{said}");
        assert!(said.starts_with("[athenaeum-dev 9.9.9-test · catalog not open]"), "{said}");
        let data = data_of(&r);
        assert_eq!(data["source"]["catalog"], "not open");
        assert!(data["error"].as_str().expect("error").contains("starting"));
    }
    early.host.on_catalog_open().expect("opens");
    let r = ask(&client, "catalog_overview", json!({})).await;
    assert_eq!(r.is_error, Some(false), "{}", text_of(&r));

    // A malformed date is said as one, with the same line 1.
    let r = ask(&client, "query_frames", json!({ "scope": { "night": "2024-13-45" } })).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text_of(&r).contains("night must be a date YYYY-MM-DD"), "{}", text_of(&r));
    assert!(text_of(&r).starts_with("[athenaeum-dev "), "{}", text_of(&r));

    // An unknown file is not found, in words.
    let r = ask(&client, "file_details", json!({ "file": "file:9999999" })).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text_of(&r).contains("no file file:9999999 in the catalog"), "{}", text_of(&r));
}

#[tokio::test]
async fn two_connections_share_one_host() {
    let rig = rig(true);
    let a = legacy(&rig.host).await;
    let b = connect(&rig.host, ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] }).await;
    let (ra, rb) = tokio::join!(
        ask(&a, "query_frames", json!({ "groupBy": ["filter"] })),
        ask(&b, "query_frames", json!({ "groupBy": ["filter"] })),
    );
    assert_eq!(ra.is_error, Some(false), "{}", text_of(&ra));
    assert_eq!(rb.is_error, Some(false), "{}", text_of(&rb));
    assert_eq!(text_of(&ra), text_of(&rb));
    assert_eq!(data_of(&ra), data_of(&rb));
    // Reading never blocks the catalog's writers.
    let database = rig.ctx.db.get().expect("db");
    db::set_setting(&database.conn(), "test.while-open", "ok").expect("write beside the readers");
}

#[tokio::test]
async fn frame_set_question_equals_its_page() {
    let rig = rig(true);
    let client = legacy(&rig.host).await;
    let (page_members, page_total) = {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        let (set, members) = db::get_frames_sets_by_project(&conn, 0)
            .expect("page")
            .into_iter()
            .find(|(s, _)| s.name.as_deref() == Some("IC 5146 set"))
            .expect("the set is on the page");
        (members, set.total_exp_time.unwrap_or(0.0))
    };
    assert_eq!(page_members, 6, "the page leaves the Black Hole member out");
    assert_eq!(page_total, 1800.0, "and so does its time, whatever the stored column says (2100)");

    let r = ask(&client, "query_frames", json!({ "scope": { "frameSet": "ic 5146  SET" }, "groupBy": ["filter"] })).await;
    assert_eq!(r.is_error, Some(false), "{}", text_of(&r));
    let data = data_of(&r);
    assert_eq!(data["frameSet"]["frames"], page_members);
    assert_eq!(data["frameSet"]["integrationS"], page_total);
    // The per-filter groups are the page's members per filter.
    let totals = {
        let reader = rig.host.reader().expect("reader");
        let set_id: i64 = reader.query_row("SELECT id FROM frames_set WHERE name = 'IC 5146 set'", [], |r| r.get(0)).expect("set id");
        crate::catalog_query::frame_set_totals(&reader, set_id).expect("totals")
    };
    let groups = data["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), totals.by_filter.len());
    for (filter, frames, seconds) in &totals.by_filter {
        let g = groups.iter().find(|g| g["key"]["filter"] == *filter).unwrap_or_else(|| panic!("no group for {filter}"));
        assert_eq!(g["totals"]["frames"], *frames);
        assert_eq!(g["totals"]["integrationS"], *seconds);
    }
    let said = text_of(&r);
    assert!(said.contains("Frame set 'IC 5146 set', as its page shows it: 6 frames, integration 30 min."), "{said}");
    assert!(said.contains("1 frame of this set is in the Black Hole and not counted (5 min)."), "{said}");
    assert!(!said.contains("35 min"), "never the stored column: {said}");
    assert_eq!(data["frameSet"]["blackHoleLeftOut"], json!({ "frames": 1, "integrationS": 300.0 }));

    // Two Black Hole members read in the plural.
    {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        let o0: i64 = conn.query_row("SELECT id FROM files WHERE filename = 'o0.fits'", [], |r| r.get(0)).expect("file");
        conn.execute("INSERT INTO black_hole (file_id, from_where, moved_at, original_path) VALUES (?1, 'test', 'now', 'x')", [o0]).expect("black hole");
    }
    let r = ask(&client, "query_frames", json!({ "scope": { "frameSet": "IC 5146 set" } })).await;
    assert!(text_of(&r).contains("2 frames of this set are in the Black Hole and not counted (10 min)."), "{}", text_of(&r));
    assert!(text_of(&r).contains("5 frames, integration 25 min."), "{}", text_of(&r));
}

#[tokio::test]
async fn the_default_state_says_what_it_leaves_out() {
    let rig = rig(true);
    let client = legacy(&rig.host).await;
    let r = ask(&client, "query_frames", json!({ "scope": { "object": "IC 5146" } })).await;
    assert!(text_of(&r).ends_with("Black Hole files are left out (1)"), "{}", text_of(&r));
    assert_eq!(data_of(&r)["blackHoleLeftOut"], 1);
    assert_eq!(data_of(&r)["totals"]["frames"], 6, "the object's six lights; the Black Hole one is not counted");

    let r = ask(&client, "query_frames", json!({ "scope": { "object": "IC 5146", "state": ["black_hole"] } })).await;
    assert!(!text_of(&r).contains("left out"), "{}", text_of(&r));
    assert_eq!(data_of(&r)["totals"]["frames"], 1);
}
