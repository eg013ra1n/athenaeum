//! The plan tools: `plan_status` (MCP plan Task 13), how an assistant learns what became of a plan it prepared, and the
//! Black Hole pair (Task 14): `find_rebuilt_masters`, which lists masters built more than once, and `plan_black_hole`,
//! which prepares a plan of copies to move to the Black Hole.
//!
//! The plans are the host's (`mcp::plans`), app-wide by handle, so a status question answers for a plan made on any
//! connection. A planning tool only prepares: no tool applies, cancels or replaces a plan the user has seen; the
//! user's own click in Athenaeum's window is the only way (`api::mcp::answer_mcp_plan`). The words are written so that
//! only a plan that really ended `applied` ever says "applied".

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::{prepare, run, to_json, Prep, ScopeArgs};
use crate::api::ApiError;
use crate::calibration_library::rebuilt_copies::{find_rebuilt_master_copies, RebuiltMasterGroup};
use crate::catalog_query::GROUPS_SHOWN;
use crate::mcp::answer::count_of;
use crate::mcp::plans::black_hole::{self, real_verify};
use crate::mcp::plans::{McpPlanView, PlanBlackHoleArgs, PlanReport, PlanState, ReportLine};
use crate::mcp::{Answer, AthenaeumMcp, McpHost};

/// Report lines the text half lists per kind (applied, skipped, failed); the structured half carries them all.
const REPORT_LINES_IN_TEXT: usize = 20;

/// Arguments of `plan_status`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub struct PlanStatusArgs {
    /// The plan's handle, as the planning tool gave it (`plan-…`).
    pub plan_id: String,
}

#[tool_router(router = plan_tools, vis = "pub(crate)")]
impl AthenaeumMcp {
    #[tool(
        description = "Where a plan stands: still being prepared, waiting for the user in Athenaeum's window, being applied, applied (with the report of what was applied, skipped and failed), cancelled, replaced by a newer plan, expired or failed. It is the only way to learn a plan's outcome: never tell the user a plan was applied before this says so. It changes nothing and cannot apply or cancel a plan; only the user can, in Athenaeum's window. Works for a plan made on any connection.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn plan_status(&self, Parameters(args): Parameters<PlanStatusArgs>) -> CallToolResult {
        run(&self.host, move |host| plan_status(host, args)).await
    }

    #[tool(
        description = "Masters of the calibration library that were built more than once: the same camera, frame type, gain, offset, binning, size, exposure, temperature, filter and night, typically the _2 / _3 copy a rebuild wrote beside the first. They are not duplicates by any hash, so find_duplicates does not list them. Each group lists its copies with how many frames calibration links point at each; a linked copy stays. The scope narrows which masters are considered (Black Hole files are never listed). Use the group handles with plan_black_hole (source rebuilt_masters). It changes nothing.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn find_rebuilt_masters(&self, Parameters(args): Parameters<ScopeArgs>) -> CallToolResult {
        run(&self.host, move |host| find_rebuilt_masters(host, args)).await
    }

    #[tool(
        description = "Prepare a plan that moves copies to the Black Hole (a soft delete the user can restore from the Black Hole page). Name the groups in `groups` by the handles find_duplicates (source duplicates) or find_rebuilt_masters (source rebuilt_masters) gave; only those groups enter. In each group one copy stays: say which with keepInFolder (the one copy under that folder, the folder itself or below) or keepFiles (file handles or full paths). A group where the instructions pick no single copy gets nothing selected and the user picks the copy that stays in the window. The other copies are proposed; a copy a calibration link points at, a copy inside a zip and a header-duplicate whose bytes differ from the kept copy are locked and never move. The plan opens in Athenaeum's window and nothing moves until the user presses Apply there; you cannot apply it. Ask plan_status for the outcome. A new plan replaces a plan the user has not decided yet. Byte checks of header duplicates can take a while.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn plan_black_hole(&self, Parameters(args): Parameters<PlanBlackHoleArgs>) -> CallToolResult {
        run(&self.host, move |host| plan_black_hole(host, args)).await
    }
}

/// What the plan's state is, in words. Only `applied` contains that word.
fn state_words(v: &McpPlanView) -> String {
    match &v.state {
        PlanState::Preparing { done, total } if *total > 0 => format!("still being prepared ({done} of {total})"),
        PlanState::Preparing { .. } => "still being prepared".to_string(),
        PlanState::Waiting => match &v.expires_at {
            Some(t) => format!("waiting for you in Athenaeum's window (expires {t})"),
            None => "waiting for you in Athenaeum's window".to_string(),
        },
        PlanState::Applying => "being carried out: Apply was pressed in Athenaeum's window and it is not finished".to_string(),
        PlanState::Applied => "applied".to_string(),
        PlanState::Cancelled => "cancelled in Athenaeum's window; nothing changed".to_string(),
        PlanState::Replaced => match &v.replaced_by {
            Some(id) => format!("replaced by a newer plan ({id}); nothing changed"),
            None => "replaced by a newer plan; nothing changed".to_string(),
        },
        PlanState::Expired => "expired before the user decided; nothing changed. Prepare it again if it is still wanted".to_string(),
        PlanState::Failed { .. } => "did not finish".to_string(),
    }
}

fn lines_text(out: &mut String, heading: &str, lines: &[ReportLine]) {
    if lines.is_empty() {
        return;
    }
    out.push_str(&format!("\n{heading}:"));
    for l in lines.iter().take(REPORT_LINES_IN_TEXT) {
        out.push_str(&format!("\n- {} in {}: {}", l.file_name, l.folder, l.reason));
    }
    if lines.len() > REPORT_LINES_IN_TEXT {
        out.push_str(&format!("\n- and {} more (the structured half lists them all)", lines.len() - REPORT_LINES_IN_TEXT));
    }
}

/// The report of a plan that ended `applied`.
fn report_text(r: &PlanReport) -> String {
    let mut out = format!(
        "\nReport: {} applied, {} skipped, {} failed.",
        r.applied.len(),
        r.skipped.len(),
        r.failed.len()
    );
    lines_text(&mut out, "Skipped", &r.skipped);
    lines_text(&mut out, "Failed", &r.failed);
    lines_text(&mut out, "Applied", &r.applied);
    run_text(&mut out, r);
    out
}

/// What a plan that ended `failed` got done before it stopped. It never says "applied": the plan was not.
fn partial_text(r: &PlanReport) -> String {
    let mut out = format!(
        "\nDone before it stopped: {} done, {} skipped, {} failed.",
        r.applied.len(),
        r.skipped.len(),
        r.failed.len()
    );
    lines_text(&mut out, "Skipped", &r.skipped);
    lines_text(&mut out, "Failed", &r.failed);
    lines_text(&mut out, "Done", &r.applied);
    run_text(&mut out, r);
    out
}

fn run_text(out: &mut String, r: &PlanReport) {
    if let Some(run_id) = &r.run_id {
        out.push_str(&format!("\nIt started the run {run_id}."));
    }
}

/// The answer for a plan that exists: the words of its state, its report when it has one, and the same in data.
pub(crate) fn status_answer(v: &McpPlanView) -> Answer {
    let mut text = format!("Plan {}, {}: {}.", v.plan_id, v.title, state_words(v));
    if let PlanState::Failed { error } = &v.state {
        // Never the word "applied": a failed plan was not applied, whatever it got done first.
        text.push_str(&format!("\nFailed: {}.", error.trim_end_matches('.')));
        if let Some(report) = &v.report {
            text.push_str(&partial_text(report));
            text.push_str("\nSome files may have changed before it stopped; check them before asking again.");
        }
    } else if let Some(report) = &v.report {
        text.push_str(&report_text(report));
    }
    let mut data = Map::new();
    data.insert("planId".to_string(), json!(v.plan_id));
    data.insert("kind".to_string(), json!(v.kind.as_str()));
    data.insert("title".to_string(), json!(v.title));
    data.insert("state".to_string(), to_json(&v.state).unwrap_or(Value::Null));
    if let Some(t) = &v.expires_at {
        data.insert("expiresAt".to_string(), json!(t));
    }
    if let Some(id) = &v.replaced_by {
        data.insert("replacedBy".to_string(), json!(id));
    }
    if let Some(report) = &v.report {
        if let Ok(value) = to_json(report) {
            data.insert("report".to_string(), value);
        }
    }
    Answer::new(text, Value::Object(data))
}

#[tracing::instrument(skip_all, fields(tool = "plan_status"), err)]
pub fn plan_status(host: &McpHost, args: PlanStatusArgs) -> Result<Answer, ApiError> {
    let id = args.plan_id.trim();
    if id.is_empty() {
        return Err(ApiError::Invalid("plan_status needs the plan's handle (planId), as the planning tool gave it".to_string()));
    }
    // The catalog must be open before any answer (line 1 names it).
    host.source()?;
    match host.plans.view(Some(id)) {
        Some(view) => Ok(status_answer(&view)),
        None => Ok(Answer::new(
            format!("No plan {id}: Athenaeum restarted since, or the plan is older than a day."),
            json!({ "planId": id, "state": { "kind": "unknown" } }),
        )),
    }
}

// ---- find_rebuilt_masters ----

fn copy_words(c: &crate::calibration_library::rebuilt_copies::RebuiltCopy) -> String {
    let linked = if c.linked_lights == 0 {
        "nothing is linked to it".to_string()
    } else {
        format!("{} linked to it, so it stays", count_of(c.linked_lights, "frame is", "frames are"))
    };
    format!("{} in {}; {linked}", c.line.file_name, c.line.folder)
}

fn rebuilt_json(g: &RebuiltMasterGroup) -> Result<Value, ApiError> {
    let mut v = to_json(g)?;
    if let (Some(obj), Some(first)) = (v.as_object_mut(), g.copies.first()) {
        obj.insert("fileName".to_string(), json!(first.line.file_name));
        obj.insert("folder".to_string(), json!(first.line.folder));
    }
    Ok(v)
}

#[tracing::instrument(skip_all, fields(tool = "find_rebuilt_masters"), err)]
pub fn find_rebuilt_masters(host: &McpHost, args: ScopeArgs) -> Result<Answer, ApiError> {
    let conn = host.reader()?;
    let prep = match prepare(&conn, &args.scope)? {
        Prep::Answered(answer) => return Ok(answer),
        Prep::Ready(prep) => prep,
    };
    let found = find_rebuilt_master_copies(&conn, Some(&prep.filter)).map_err(|e| {
        tracing::error!(error = %format!("{e:#}"), "mcp could not read the rebuilt masters");
        ApiError::Internal(format!("could not read the rebuilt masters: {e:#}"))
    })?;
    drop(conn);
    let total = found.len();
    let page = args.page.unwrap_or(0);
    let pages = total.div_ceil(GROUPS_SHOWN);
    let start = (page as usize).saturating_mul(GROUPS_SHOWN);
    let shown: Vec<&RebuiltMasterGroup> = found.iter().skip(start).take(GROUPS_SHOWN).collect();

    let mut lines: Vec<String> = Vec::new();
    if total == 0 {
        lines.push("No master was built more than once in this scope.".to_string());
    } else {
        lines.push(format!(
            "{} built more than once (they are not duplicates by any hash, so find_duplicates does not list them).",
            count_of(total, "master", "masters")
        ));
        if shown.is_empty() {
            lines.push(format!("Page {} is past the last page ({pages}).", page.saturating_add(1)));
        } else {
            lines.push(format!("Page {} of {pages}: {}.", page + 1, count_of(shown.len(), "group", "groups")));
        }
    }
    for (n, g) in shown.iter().enumerate() {
        lines.push(format!(
            "{}. {} ({}):",
            start + n + 1,
            g.params_label,
            count_of(g.copies.len(), "copy", "copies")
        ));
        for c in &g.copies {
            lines.push(format!("  - {}", copy_words(c)));
        }
    }
    let mut data = Map::new();
    data.insert("groups".to_string(), Value::Array(shown.iter().map(|g| rebuilt_json(g)).collect::<Result<_, _>>()?));
    data.insert("groupsInScope".to_string(), json!(total));
    data.insert("page".to_string(), json!(page));
    data.insert("pageCount".to_string(), json!(pages));
    Ok(prep.answer(&lines.join("\n"), data))
}

// ---- plan_black_hole ----

#[tracing::instrument(skip_all, fields(tool = "plan_black_hole"), err)]
pub fn plan_black_hole(host: &McpHost, args: PlanBlackHoleArgs) -> Result<Answer, ApiError> {
    // The tool runs on a blocking thread (`run`), where the byte checks belong.
    black_hole::prepare(host, args, real_verify())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use rmcp::model::{CallToolRequestParams, CallToolResult, ProtocolVersion};
    use rmcp::service::RunningService;
    use rmcp::{ClientLifecycleMode, RoleClient};
    use serde_json::{json, Value};

    use super::{plan_status, status_answer, PlanStatusArgs};
    use crate::api::ApiError;
    use crate::events::NullEmitter;
    use crate::mcp::plans::{
        McpPlanView, PlanApplier, PlanContent, PlanKind, PlanReport, PlanSelection, PlanState, ReportLine,
    };
    use crate::mcp::plans::black_hole::tests as bh;
use crate::mcp::{mcp_server_name, HostKind, McpHost, McpIdentity, McpSource, PlanAnswer};
    use crate::services::ServiceContext;

    struct Rig {
        host: Arc<McpHost>,
        // Dropped after the host is shut down (see `Drop`): the host holds the catalog open under this directory.
        _tmp: tempfile::TempDir,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            self.host.shutdown();
        }
    }

    fn rig() -> Rig {
        let (tmp, base) = crate::test_support::canonical_tempdir();
        let ctx = Arc::new(ServiceContext::new_for_tests(base.join("catalog.db")));
        let host = McpHost::new(
            ctx,
            McpIdentity { server_name: mcp_server_name(), version: "9.9.9-test", kind: HostKind::Desktop, web_api_key_set: false },
            Arc::new(NullEmitter),
            Arc::new(|| {}),
            Arc::new(|| true),
        );
        host.on_catalog_open().expect("the catalog opens");
        Rig { host, _tmp: tmp }
    }

    struct Nothing;

    impl PlanApplier for Nothing {
        fn validate(&self, _view: &McpPlanView, _sel: &PlanSelection) -> Result<(), String> {
            Ok(())
        }
        fn apply(&self, _host: &McpHost, _plan_id: &str, _sel: &PlanSelection, _fresh: &[String]) -> PlanReport {
            PlanReport::default()
        }
    }

    /// A plan that waits for the user (no items: only its life is under test here).
    fn waiting(host: &Arc<McpHost>) -> String {
        let (id, _) = host.plans.begin(host, PlanKind::BlackHole, "Move duplicates");
        host.plans
            .publish(
                &id,
                PlanContent {
                    title: "Move duplicates".to_string(),
                    columns: vec![],
                    groups: vec![],
                    notes: vec![],
                    snapshots: HashMap::new(),
                    applier: Box::new(Nothing),
                },
            )
            .expect("publish");
        id
    }

    type Client = RunningService<RoleClient, ()>;

    struct Peer {
        client: Client,
        server: tokio::task::JoinHandle<()>,
    }

    impl std::ops::Deref for Peer {
        type Target = Client;
        fn deref(&self) -> &Client {
            &self.client
        }
    }

    impl Peer {
        async fn close(self) {
            let Peer { client, server } = self;
            let _ = client.cancel().await;
            tokio::time::timeout(std::time::Duration::from_secs(10), server)
                .await
                .expect("the server ends once its client is gone")
                .expect("the server task");
        }
    }

    async fn connect(host: &Arc<McpHost>, mode: ClientLifecycleMode) -> Peer {
        let (server_end, client_end) = tokio::io::duplex(1 << 20);
        let server = host.server();
        let server = tokio::spawn(async move {
            let running = rmcp::serve_server(server, server_end).await.expect("the server starts");
            let _ = running.waiting().await;
        });
        let client = rmcp::serve_client_with_lifecycle((), client_end, mode).await.expect("the client connects");
        Peer { client, server }
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

    /// Any object key called `selection`, at any depth.
    fn has_selection_key(v: &Value) -> bool {
        match v {
            Value::Object(map) => map.contains_key("selection") || map.values().any(has_selection_key),
            Value::Array(items) => items.iter().any(has_selection_key),
            _ => false,
        }
    }

    // ---- tests ----

    #[tokio::test]
    async fn no_tool_applies_a_plan() {
        let rig = rig();
        let client = connect(&rig.host, ClientLifecycleMode::Initialize).await;
        let tools = client.list_all_tools().await.expect("tools");
        let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        for want in ["plan_status", "plan_black_hole", "find_rebuilt_masters"] {
            assert!(names.iter().any(|n| n == want), "{want} is not offered: {names:?}");
        }
        for n in &names {
            for word in ["apply", "cancel", "replace", "confirm", "approve"] {
                assert!(!n.contains(word), "a tool named {n} could decide a plan: {names:?}");
            }
        }
        for t in &tools {
            let schema = serde_json::to_value(&t.input_schema).expect("a schema");
            assert!(!has_selection_key(&schema), "{} takes a selection: {schema}", t.name);
        }
        client.close().await;
    }

    #[tokio::test]
    async fn status_waits_for_the_user() {
        let rig = rig();
        let id = waiting(&rig.host);
        let expires = rig.host.plans.view(Some(&id)).expect("plan").expires_at.expect("it waits, so it expires");
        let client = connect(&rig.host, ClientLifecycleMode::Initialize).await;
        let r = ask(&client, "plan_status", json!({ "planId": id })).await;
        assert_eq!(r.is_error, Some(false), "{}", text_of(&r));
        let said = text_of(&r);
        assert!(said.starts_with("[athenaeum-dev 9.9.9-test · catalog "), "{said}");
        assert!(said.contains(&format!("waiting for you in Athenaeum's window (expires {expires})")), "{said}");
        assert!(!said.contains("applied"), "a waiting plan is not applied: {said}");
        let data = data_of(&r);
        assert_eq!(data["planId"], json!(id));
        assert_eq!(data["state"]["kind"], "waiting");
        assert_eq!(data["kind"], "black_hole");
        assert_eq!(data["expiresAt"], json!(expires));
        client.close().await;
    }

    #[tokio::test]
    async fn status_on_another_connection() {
        let rig = bh::rig();
        let keep = bh::header_copy(&rig, "keep", "other-conn.fits", b"AAAA");
        bh::header_copy(&rig, "away", "other-conn.fits", b"AAAA");
        let a = connect(&rig.host, ClientLifecycleMode::Initialize).await;
        let b = connect(&rig.host, ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] }).await;
        // The plan is made through A's connection with the real planning tool; B is asked about it.
        let made = ask(
            &a,
            "plan_black_hole",
            json!({
                "source": "duplicates",
                "groups": [bh::header_handle("other-conn.fits")],
                "keepInFolder": keep.dir().to_string_lossy(),
            }),
        )
        .await;
        assert_eq!(made.is_error, Some(false), "{}", text_of(&made));
        let said = text_of(&made);
        assert!(said.contains("1 file proposed for the Black Hole"), "{said}");
        assert!(said.contains("keeps other-conn.fits in "), "{said}");
        assert!(!said.contains("dup:") && !said.to_lowercase().contains("applied"), "{said}");
        let id = data_of(&made)["planId"].as_str().expect("a plan id").to_string();

        let on_a = ask(&a, "plan_status", json!({ "planId": id })).await;
        let on_b = ask(&b, "plan_status", json!({ "planId": id })).await;
        assert_eq!(on_b.is_error, Some(false), "{}", text_of(&on_b));
        assert!(text_of(&on_b).contains("waiting for you in Athenaeum's window"), "{}", text_of(&on_b));
        assert_eq!(text_of(&on_a), text_of(&on_b));
        assert_eq!(data_of(&on_a), data_of(&on_b));
        assert_eq!(data_of(&on_b)["kind"], "black_hole");
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn find_rebuilt_masters_lists_the_copies_and_what_is_linked() {
        let rig = bh::rig();
        let m1 = bh::master_copy(&rig, "MasterDark-tool-1.xisf", 300.0, b"M1");
        bh::master_copy(&rig, "MasterDark-tool-2.xisf", 300.0, b"M2");
        bh::link_master(&rig, &m1, 3);
        let client = connect(&rig.host, ClientLifecycleMode::Initialize).await;
        let r = ask(&client, "find_rebuilt_masters", json!({})).await;
        assert_eq!(r.is_error, Some(false), "{}", text_of(&r));
        let said = text_of(&r);
        assert!(said.contains("1 master built more than once"), "{said}");
        assert!(said.contains("MasterDark-tool-1.xisf in ") && said.contains("3 frames are linked to it, so it stays"), "{said}");
        assert!(!said.contains("rebuilt:") && !said.contains("file:"), "no handle in the words: {said}");
        let data = data_of(&r);
        let g = &data["groups"][0];
        assert_eq!(g["handle"], json!(bh::rebuilt_handle(&m1)));
        assert_eq!(g["fileName"], "MasterDark-tool-1.xisf");
        assert!(g["folder"].is_string());
        assert_eq!(g["copies"][0]["linkedLights"], 3);
        assert_eq!(g["copies"][1]["linkedLights"], 0);
        client.close().await;
    }

    #[tokio::test]
    async fn unknown_plan_is_said_in_words() {
        let rig = rig();
        let client = connect(&rig.host, ClientLifecycleMode::Initialize).await;
        let r = ask(&client, "plan_status", json!({ "planId": "plan-nope" })).await;
        assert_eq!(r.is_error, Some(false), "an unknown plan is an answer: {}", text_of(&r));
        assert!(
            text_of(&r).contains("No plan plan-nope: Athenaeum restarted since, or the plan is older than a day."),
            "{}",
            text_of(&r)
        );
        let data = data_of(&r);
        assert_eq!(data["planId"], "plan-nope");
        assert_eq!(data["state"]["kind"], "unknown");
        client.close().await;
    }

    #[tokio::test]
    async fn a_missing_plan_id_is_refused_in_words() {
        let rig = rig();
        let r = plan_status(&rig.host, PlanStatusArgs { plan_id: "  ".to_string() });
        assert!(matches!(r, Err(ApiError::Invalid(_))), "{r:?}");
    }

    #[tokio::test]
    async fn a_page_that_opens_later_sees_the_waiting_plan() {
        let rig = rig();
        assert!(crate::api::mcp::get_mcp_plan(&rig.host, None).is_none(), "no plan yet");
        let id = waiting(&rig.host);
        // A page that opens now asks for the newest plan, and then for one by its id.
        let newest = crate::api::mcp::get_mcp_plan(&rig.host, None).expect("the waiting plan");
        assert_eq!(newest.plan_id, id);
        assert_eq!(newest.state, PlanState::Waiting);
        let by_id = crate::api::mcp::get_mcp_plan(&rig.host, Some(id.clone())).expect("by its id");
        assert_eq!(by_id, newest);
        assert!(crate::api::mcp::get_mcp_plan(&rig.host, Some("plan-nope".to_string())).is_none());
    }

    #[tokio::test]
    async fn answer_cancel_over_the_command() {
        let rig = rig();
        let id = waiting(&rig.host);
        let view = crate::api::mcp::answer_mcp_plan(&rig.host, id.clone(), PlanAnswer::Cancel).expect("cancel");
        assert_eq!(view.state, PlanState::Cancelled);
        // What the assistant is told afterwards.
        let r = plan_status(&rig.host, PlanStatusArgs { plan_id: id.clone() }).expect("status");
        assert!(r.text.contains("cancelled"), "{}", r.text);
        assert_eq!(r.data["state"]["kind"], "cancelled");
        // A plan that is decided cannot be cancelled again, and an unknown one is a NotFound.
        let again = crate::api::mcp::answer_mcp_plan(&rig.host, id, PlanAnswer::Cancel);
        assert!(matches!(again, Err(ApiError::Invalid(_))), "{again:?}");
        let gone = crate::api::mcp::answer_mcp_plan(&rig.host, "plan-nope".to_string(), PlanAnswer::Cancel);
        assert!(matches!(gone, Err(ApiError::NotFound(_))), "{gone:?}");
    }

    // ---- the words of every state ----

    fn view(state: PlanState) -> McpPlanView {
        McpPlanView {
            plan_id: "plan-1".to_string(),
            kind: PlanKind::BlackHole,
            title: "Move duplicates".to_string(),
            source: McpSource { build: "athenaeum-dev".into(), version: "9.9.9-test".into(), catalog: "c.db".into() },
            state,
            replaced_by: None,
            expires_at: None,
            columns: vec![],
            groups: vec![],
            notes: vec![],
            report: None,
        }
    }

    fn line(name: &str, why: &str) -> ReportLine {
        ReportLine { item_id: "item:1".into(), file_name: name.into(), folder: "/nas/a".into(), reason: why.into() }
    }

    #[test]
    fn only_an_applied_plan_says_applied() {
        let mut replaced = view(PlanState::Replaced);
        replaced.replaced_by = Some("plan-2".to_string());
        for v in [
            view(PlanState::Preparing { done: 2, total: 9 }),
            view(PlanState::Waiting),
            view(PlanState::Applying),
            view(PlanState::Cancelled),
            replaced,
            view(PlanState::Expired),
            view(PlanState::Failed { error: "the disk is full".to_string() }),
        ] {
            let said = status_answer(&v).text.to_lowercase();
            assert!(!said.contains("applied"), "{:?} says applied: {said}", v.state);
        }
        // A plan that failed after it got something done reports it without saying it was applied.
        let mut broke = view(PlanState::Failed { error: "applying the plan stopped unexpectedly".to_string() });
        broke.report = Some(PlanReport {
            applied: vec![line("c.fits", "moved to the Black Hole")],
            skipped: vec![line("d.fits", "changed on disk since the plan")],
            failed: vec![],
            run_id: None,
        });
        let said = status_answer(&broke).text;
        assert!(!said.to_lowercase().contains("applied"), "a failed plan says applied: {said}");
        assert!(said.contains("Failed: applying the plan stopped unexpectedly."), "{said}");
        assert!(said.contains("Done before it stopped: 1 done, 1 skipped, 0 failed."), "{said}");
        assert!(said.contains("d.fits in /nas/a: changed on disk since the plan"), "{said}");
        assert!(said.contains("c.fits in /nas/a: moved to the Black Hole"), "{said}");
        assert_eq!(status_answer(&broke).data["report"]["applied"][0]["fileName"], "c.fits");
        let mut done = view(PlanState::Applied);
        done.report = Some(PlanReport {
            applied: vec![line("a.fits", "moved")],
            skipped: vec![line("b.fits", "changed on disk since the plan")],
            failed: vec![],
            run_id: None,
        });
        let said = status_answer(&done).text;
        assert!(said.contains("applied"), "{said}");
        assert!(said.contains("1 applied, 1 skipped, 0 failed"), "{said}");
        assert!(said.contains("b.fits in /nas/a: changed on disk since the plan"), "{said}");
    }

    #[test]
    fn each_state_has_its_words_and_its_data() {
        let a = status_answer(&view(PlanState::Preparing { done: 2, total: 9 }));
        assert!(a.text.contains("still being prepared (2 of 9)"), "{}", a.text);
        let mut replaced = view(PlanState::Replaced);
        replaced.replaced_by = Some("plan-2".to_string());
        let a = status_answer(&replaced);
        assert!(a.text.contains("replaced by a newer plan (plan-2)") && a.text.contains("nothing changed"), "{}", a.text);
        assert_eq!(a.data["replacedBy"], "plan-2");
        let a = status_answer(&view(PlanState::Expired));
        assert!(a.text.contains("expired") && a.text.contains("nothing changed"), "{}", a.text);
        let a = status_answer(&view(PlanState::Failed { error: "the disk is full".to_string() }));
        assert!(a.text.contains("Failed: the disk is full."), "{}", a.text);
        assert_eq!(a.data["state"], json!({ "kind": "failed", "error": "the disk is full" }));
        let a = status_answer(&view(PlanState::Applying));
        assert!(a.text.contains("not finished"), "{}", a.text);
    }
}
