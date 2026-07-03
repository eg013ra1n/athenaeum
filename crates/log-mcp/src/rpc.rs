//! Hand-rolled stdio JSON-RPC (the MCP wire protocol): one JSON object per
//! line, no `Content-Length` framing. Handles `initialize`,
//! `notifications/initialized`, `tools/list`, `tools/call`; everything else
//! is `method not found`. Requests with a missing/`null` `id` (JSON-RPC
//! notifications) never get a response, per MCP convention.

use crate::query::{self, Filter};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl Response {
    fn ok(id: Value, result: Value) -> Self {
        Response {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn err(id: Value, code: i64, message: String) -> Self {
        Response {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError { code, message }),
        }
    }
}

/// Handle one request against the log dir. Returns `None` for
/// notifications (no `id`, or `id: null`), which must never get a
/// response.
pub fn handle(req: &Request, log_dir: &Path) -> Option<Response> {
    let is_notification = matches!(req.id, None | Some(Value::Null));
    let id = req.id.clone().unwrap_or(Value::Null);

    if req.method == "notifications/initialized" {
        return None; // no response, ever, regardless of id
    }
    if is_notification {
        return None;
    }

    match req.method.as_str() {
        "initialize" => Some(Response::ok(id, initialize_result())),
        "tools/list" => Some(Response::ok(id, tools_list_result())),
        "tools/call" => match dispatch_tool_call(&req.params, log_dir) {
            Ok(v) => Some(Response::ok(id, v)),
            Err(e) => Some(Response::err(id, -32603, e.to_string())),
        },
        other => Some(Response::err(
            id,
            -32601,
            format!("method not found: {other}"),
        )),
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "athenaeum-logs", "version": env!("CARGO_PKG_VERSION") }
    })
}

fn tools_list_result() -> Value {
    json!({
        "tools": [
            {
                "name": "query_logs",
                "description": "Query Athenaeum's rotating JSONL application logs with level/module/text/time filters.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "level": {
                            "type": "string",
                            "description": "Minimum level (error|warn|info|debug|trace), case-insensitive; matches at-or-above severity."
                        },
                        "module": {
                            "type": "string",
                            "description": "Prefix match against the event's `target` (e.g. \"athenaeum_core::scanner\")."
                        },
                        "contains": {
                            "type": "string",
                            "description": "Substring match against the serialized event."
                        },
                        "since": {
                            "type": "string",
                            "description": "RFC3339 timestamp lower bound (inclusive)."
                        },
                        "until": {
                            "type": "string",
                            "description": "RFC3339 timestamp upper bound (inclusive)."
                        },
                        "operation_id": {
                            "type": "string",
                            "description": "Match events whose current span stack carries this id under root_id/operation_id/frame_id/frame_set_id."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Max matches to return (default 200, cap 1000)."
                        }
                    }
                }
            },
            {
                "name": "tail_logs",
                "description": "Return the last N log lines across all rotated files, unfiltered.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "n": {
                            "type": "integer",
                            "description": "Number of lines to return (default 50, cap 1000)."
                        }
                    }
                }
            },
            {
                "name": "list_operations",
                "description": "List completed operation spans (scan, archive_op, file_op, solve, export) with their id and duration.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "description": "Filter to one operation kind: scan|archive_op|file_op|solve|export."
                        },
                        "since": {
                            "type": "string",
                            "description": "RFC3339 timestamp lower bound (inclusive)."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Max operations to return (default 200, cap 1000)."
                        }
                    }
                }
            },
            {
                "name": "get_operation",
                "description": "Get every log event belonging to one operation, matched by its id (root_id/operation_id/frame_id/frame_set_id).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": ["string", "integer"],
                            "description": "The operation id value, e.g. a scan root_id or archive operation_id."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Max matches to return (default 1000)."
                        }
                    },
                    "required": ["id"]
                }
            }
        ]
    })
}

fn filter_from_args(args: &Value) -> Filter {
    Filter {
        level: args.get("level").and_then(|v| v.as_str()).map(String::from),
        module: args
            .get("module")
            .and_then(|v| v.as_str())
            .map(String::from),
        contains: args
            .get("contains")
            .and_then(|v| v.as_str())
            .map(String::from),
        since: args.get("since").and_then(|v| v.as_str()).map(String::from),
        until: args.get("until").and_then(|v| v.as_str()).map(String::from),
        operation_id: args
            .get("operation_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        limit: query::clamp_limit(
            args.get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize),
        ),
    }
}

fn value_to_id_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn dispatch_tool_call(params: &Value, log_dir: &Path) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("tools/call: missing \"name\""))?;
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);

    let result: Value = match name {
        "query_logs" => {
            let f = filter_from_args(&args);
            serde_json::to_value(query::scan(log_dir, &f)?)?
        }
        "tail_logs" => {
            let n = query::clamp_limit(
                args.get("n")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize)
                    .or(Some(50))
            );
            serde_json::to_value(query::tail(log_dir, n)?)?
        }
        "list_operations" => {
            let kind = args.get("kind").and_then(|v| v.as_str());
            let since = args.get("since").and_then(|v| v.as_str());
            let limit = query::clamp_limit(
                args.get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize),
            );
            serde_json::to_value(query::list_operations(log_dir, kind, since, limit)?)?
        }
        "get_operation" => {
            let id = args
                .get("id")
                .ok_or_else(|| anyhow!("get_operation: missing \"id\""))?;
            let id = value_to_id_string(id);
            let limit = query::clamp_limit(
                args.get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize)
                    .or(Some(query::MAX_LIMIT)),
            );
            serde_json::to_value(query::get_operation(log_dir, &id, limit)?)?
        }
        other => bail!("unknown tool: {other}"),
    };

    let text = serde_json::to_string(&result)?;
    Ok(json!({ "content": [ { "type": "text", "text": text } ] }))
}
