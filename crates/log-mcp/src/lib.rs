//! Library surface for the `log-mcp` stdio MCP server.
//!
//! Split out from `main.rs` so `tests/` can exercise `query::scan` and
//! friends directly against hand-authored fixture JSONL, without going
//! through the JSON-RPC protocol layer.

pub mod query;
pub mod rpc;
