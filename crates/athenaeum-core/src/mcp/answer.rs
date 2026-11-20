//! The shape of every answer (spec §6.1, §7): line 1 names the build and the catalog it came from, then a text half
//! and a structured half that each read on their own.
//!
//! A failure is an error RESULT in words (`is_error`), never a protocol error: a client shows a protocol error's
//! message to nobody.

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{json, Map, Value};

use super::{McpHost, McpSource};
use crate::api::ApiError;

/// What a tool returns: the text the assistant reads and the data it can compute with. `data` is a JSON object;
/// `source` is added to it by [`to_tool_result`].
#[derive(Debug, Clone)]
pub struct Answer {
    pub text: String,
    pub data: Value,
}

impl Answer {
    pub fn new(text: impl Into<String>, data: Value) -> Self {
        Self { text: text.into(), data }
    }
}

/// What line 1 and `source` say before the catalog is open.
const CATALOG_NOT_OPEN: &str = "not open";

fn not_open(host: &McpHost) -> McpSource {
    McpSource {
        build: host.identity().server_name.to_string(),
        version: host.identity().version.to_string(),
        catalog: CATALOG_NOT_OPEN.to_string(),
    }
}

/// `[athenaeum-dev 0.7.2 · catalog /path/catalog.db]`.
fn first_line(source: &McpSource) -> String {
    format!("[{} {} · catalog {}]", source.build, source.version, source.catalog)
}

fn with_source(source: &McpSource, extra: Map<String, Value>) -> Value {
    let mut out = Map::new();
    out.insert("source".to_string(), json!(source));
    for (k, v) in extra {
        if k != "source" {
            out.insert(k, v);
        }
    }
    Value::Object(out)
}

/// An answer as an MCP tool result. Success: the text under line 1, the data under `source`. Failure:
/// `is_error` with the same line 1 and `{"source", "error"}`.
pub fn to_tool_result(host: &McpHost, answer: Result<Answer, ApiError>) -> CallToolResult {
    let found = host.source();
    let source = found.as_ref().map_or_else(|_| not_open(host), Clone::clone);
    let line = first_line(&source);
    // An answer cannot name a catalog that is not open: that is the failure, said in words.
    let outcome = match (answer, found) {
        (Ok(a), Ok(_)) => Ok(a),
        (Ok(_), Err(e)) | (Err(e), _) => Err(e),
    };
    match outcome {
        Ok(a) => {
            let data = match a.data {
                Value::Object(map) => map,
                Value::Null => Map::new(),
                other => Map::from_iter([("data".to_string(), other)]),
            };
            let mut result = CallToolResult::success(vec![ContentBlock::text(format!("{line}\n{}", a.text))]);
            result.structured_content = Some(with_source(&source, data));
            result
        }
        Err(e) => {
            let message = e.to_string();
            let mut result = CallToolResult::error(vec![ContentBlock::text(format!("{line}\n{message}"))]);
            result.structured_content =
                Some(with_source(&source, Map::from_iter([("error".to_string(), json!(message))])));
            result
        }
    }
}

// ---- words ----

/// `1.5 GB`, binary steps with the usual names.
pub(crate) fn bytes_text(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes.max(0) as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} B", bytes.max(0))
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `1 frame`, `24 frames`.
pub(crate) fn count_of(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
