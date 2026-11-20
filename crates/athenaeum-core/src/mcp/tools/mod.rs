//! The tools (MCP plan): each is a core fn `tools::<name>(host, args) -> Result<Answer, ApiError>` wearing a
//! `tracing::instrument` span with its `tool` field; the `#[tool]` method on `AthenaeumMcp` is a two-line adapter
//! that runs it on a blocking thread and wraps the outcome with [`to_tool_result`](super::to_tool_result).
//!
//! Every scoped question starts in [`prepare`]: it resolves the scope on the host's reader, answers in words when the
//! scope names nothing the catalog holds, and counts the Black Hole files the default state leaves out.

mod catalog;
pub(crate) mod plans;

use std::sync::Arc;

use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rusqlite::{params_from_iter, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub use plans::{plan_status, PlanStatusArgs};
pub use catalog::{catalog_overview, file_details, find_duplicates, find_missing_metadata, query_frames};

use super::{to_tool_result, Answer, McpHost};
use crate::api::ApiError;
use crate::catalog_query::{MissingCategory, QualityMetric};
use crate::catalog_query::GroupKey;
use crate::scope::{hints_text, resolve_scope, FrameState, ResolvedScope, Scope, ScopeFilter};

/// Arguments of `query_frames`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub struct QueryFramesArgs {
    /// Which frames; an empty scope is the whole catalog (Black Hole files left out).
    #[serde(default)]
    pub scope: Scope,
    /// Group the frames by these keys, e.g. `["object", "filter"]`; the answer lists the largest groups.
    #[serde(default)]
    pub group_by: Vec<GroupKey>,
    /// Summarise one stored star measurement over the raw lights (min, median, max, outliers).
    #[serde(default)]
    pub quality: Option<QualityMetric>,
    /// Say which light groups have darks, flats and bias linked.
    #[serde(default)]
    pub calibration: bool,
    /// Say where the files live: per scan root, folder and archive state.
    #[serde(default)]
    pub storage: bool,
    /// List the files themselves, 50 to a page; the zero-based page number. Absent: no file list.
    #[serde(default)]
    pub files_page: Option<u32>,
}

/// Arguments of `find_duplicates`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub struct ScopeArgs {
    /// Which frames; a group is in the scope when one of its copies is. Empty: the whole catalog.
    #[serde(default)]
    pub scope: Scope,
    /// Zero-based page of groups, 50 to a page.
    #[serde(default)]
    pub page: Option<u32>,
}

/// Arguments of `find_missing_metadata`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub struct MissingArgs {
    /// Which frames. Empty: the whole catalog.
    #[serde(default)]
    pub scope: Scope,
    /// What to look for: coordinates, object, date, camera, frame_type. Empty: all five.
    #[serde(default)]
    pub categories: Vec<MissingCategory>,
    /// Zero-based page of files, 50 to a page.
    #[serde(default)]
    pub page: Option<u32>,
}

/// Arguments of `file_details`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub struct FileArgs {
    /// A file: the `file:<number>` handle an earlier answer gave, or its full path as the catalog stores it. The
    /// path is looked up in the catalog; Athenaeum never opens it.
    pub file: String,
}

/// Run a tool body on a blocking thread (it reads SQLite) and wrap what it answers.
pub(crate) async fn run<F>(host: &Arc<McpHost>, body: F) -> CallToolResult
where
    F: FnOnce(&McpHost) -> Result<Answer, ApiError> + Send + 'static,
{
    let worker = Arc::clone(host);
    let answer = match tokio::task::spawn_blocking(move || body(&worker)).await {
        Ok(answer) => answer,
        Err(e) => {
            tracing::error!(error = %e, "mcp tool task failed");
            Err(ApiError::Internal("the question stopped before it could answer; ask again".to_string()))
        }
    };
    to_tool_result(host, answer)
}

// ---- the scope step every scoped question shares ----

/// A scope that names something the catalog holds.
pub(crate) struct Prepared {
    pub resolved: ResolvedScope,
    pub filter: ScopeFilter,
    /// Black Hole files the scope would cover if its state asked for them; `None` when it does ask.
    pub left_out: Option<usize>,
}

pub(crate) enum Prep {
    /// The scope names nothing the catalog holds: the answer says what it does hold.
    Answered(Answer),
    Ready(Prepared),
}

pub(crate) fn to_json<T: Serialize>(value: &T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|e| {
        tracing::error!(error = %e, "mcp answer could not be built");
        ApiError::Internal(format!("could not build the answer: {e}"))
    })
}

fn hints_json(resolved: &ResolvedScope) -> Value {
    Value::Array(
        resolved
            .hints
            .iter()
            .map(|h| json!({ "condition": h.condition, "given": h.given, "existing": h.existing }))
            .collect(),
    )
}

/// Resolve `scope` on the reader. A malformed date is an `Invalid` with its sentence; a value the catalog does not
/// hold is an answer that names the nearest ones and runs nothing.
pub(crate) fn prepare(conn: &Connection, scope: &Scope) -> Result<Prep, ApiError> {
    let resolved = resolve_scope(conn, scope).map_err(|e| ApiError::Invalid(format!("{e:#}")))?;
    let Some(filter) = resolved.filter.clone() else {
        let data = json!({
            "scope": resolved.description,
            "frames": 0,
            "hints": hints_json(&resolved),
        });
        return Ok(Prep::Answered(Answer::new(hints_text(&resolved.hints), data)));
    };
    let left_out = if resolved.black_hole_left_out {
        Some(black_hole_in_scope(conn, scope)?)
    } else {
        None
    };
    Ok(Prep::Ready(Prepared { resolved, filter, left_out }))
}

/// The frames the scope would cover if its state were the Black Hole.
fn black_hole_in_scope(conn: &Connection, scope: &Scope) -> Result<usize, ApiError> {
    let asking = Scope { state: Some(vec![FrameState::BlackHole]), ..scope.clone() };
    let resolved = resolve_scope(conn, &asking).map_err(|e| ApiError::Invalid(format!("{e:#}")))?;
    let Some(filter) = resolved.filter else { return Ok(0) };
    let sql = format!("SELECT COUNT(*) FROM frames fr WHERE {}", filter.sql);
    let n: i64 = conn.query_row(&sql, params_from_iter(filter.params.iter()), |r| r.get(0)).map_err(|e| {
        tracing::error!(error = %e, "mcp could not count the Black Hole files of a scope");
        ApiError::Internal(format!("could not count the Black Hole files: {e}"))
    })?;
    Ok(usize::try_from(n).unwrap_or(0))
}

impl Prepared {
    /// The finished answer: `Scope: <words>.`, the body, and what the default state leaves out.
    pub(crate) fn answer(&self, body: &str, mut data: Map<String, Value>) -> Answer {
        let mut text = format!("Scope: {}.\n{body}", self.resolved.description);
        data.insert("scope".to_string(), json!(self.resolved.description));
        if let Some(n) = self.left_out {
            text.push_str(&format!("\nBlack Hole files are left out ({n})"));
            data.insert("blackHoleLeftOut".to_string(), json!(n));
        }
        Answer::new(text, Value::Object(data))
    }
}
