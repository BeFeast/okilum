//! MCP adapter over the same core the JSON-lines protocol exposes (#5).
//!
//! Depth 0 of the t3 integration decided at the spike: no editor patches, no
//! shell dependency, just a stdio JSON-RPC server that any MCP client can
//! spawn. Every tool here is a thin wrapper over an operation `handle()`
//! already serves — the point is that agent tooling and the JSON-lines client
//! reach the vault through one core, not two.
//!
//! Protocol version pinned to the MCP spec revision this was written against.
//! Only the subset needed for tools is implemented: `initialize`,
//! `tools/list`, `tools/call`, `ping`, and the `notifications/initialized`
//! no-op. Everything else returns method-not-found rather than pretending.

use std::io::{BufRead, Write};

use okilum_core::{ir, render_html, Resolution, Searcher, Vault};
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

fn tools() -> Value {
    json!([
        {
            "name": "list_notes",
            "description": "Every note in the vault, as vault-relative paths with titles.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "search",
            "description": "Full-text search over the vault. Supports phrases in quotes, AND by default, \
                            -negation, and field prefixes: title:, path:, tag:, heading:, links_to:, \
                            frontmatter.<key>:, date:[YYYY-MM-DD TO YYYY-MM-DD]. Falls back to \
                            one-typo fuzzy matching when the exact query finds nothing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 30}
                },
                "required": ["query"],
                "additionalProperties": false
            }
        },
        {
            "name": "read_note",
            "description": "A note's Markdown source, with [[wikilinks]] rewritten to okilum:// links. \
                            Frontmatter stripped.",
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string", "description": "vault-relative path"}},
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "render_note",
            "description": "A note rendered to HTML.",
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "note_blocks",
            "description": "A note as a block-level IR (headings, paragraphs, lists, code, tables).",
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "backlinks",
            "description": "Notes that link to this one, with the linking line. `ambiguous: true` \
                            marks a backlink whose source link named several notes.",
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "resolve_link",
            "description": "Where a wikilink target points. Returns exactly one of: resolved (one path), \
                            ambiguous (several candidates — never silently picks one), unresolved.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": {"type": "string", "description": "the text inside [[...]]"},
                    "from": {"type": "string", "description": "vault-relative path of the linking note; needed for ./ and ../ targets"}
                },
                "required": ["target"],
                "additionalProperties": false
            }
        },
        {
            "name": "explain_hit",
            "description": "Why a note matched a query: the engine's scoring tree. Null if it is not a hit.",
            "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string"}, "path": {"type": "string"}},
                "required": ["query", "path"],
                "additionalProperties": false
            }
        }
    ])
}

fn text_result(v: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": v.into()}]})
}

fn json_result(v: &Value) -> Value {
    // MCP content is text; structured data goes as pretty JSON so an agent
    // can read it and a client can parse it.
    text_result(serde_json::to_string_pretty(v).unwrap_or_default())
}

fn tool_error(msg: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": msg.into()}], "isError": true})
}

fn call_tool(vault: &Vault, searcher: &Searcher, name: &str, args: &Value) -> Value {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
    match name {
        "list_notes" => json_result(&json!(vault.notes)),
        "search" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(30)
                .clamp(1, 100) as usize;
            match searcher.search(s("query"), limit) {
                Ok(hits) => json_result(&json!(hits)),
                Err(e) => tool_error(e.to_string()),
            }
        }
        "read_note" => match okilum_core::render::note_source(vault, s("path")) {
            Ok(src) => text_result(src),
            Err(e) => tool_error(e.to_string()),
        },
        "render_note" => match render_html(vault, s("path"), "InspiredGitHub") {
            Ok(html) => text_result(html),
            Err(e) => tool_error(e.to_string()),
        },
        "note_blocks" => match ir::build_doc(vault, s("path")) {
            Ok(d) => json_result(&json!(d)),
            Err(e) => tool_error(e.to_string()),
        },
        "backlinks" => json_result(&json!(vault.backlinks(s("path")))),
        "resolve_link" => {
            let r = match vault.resolve_from(s("target"), s("from")) {
                Resolution::Resolved { path } => json!({"kind": "resolved", "path": path}),
                Resolution::Ambiguous { candidates } => {
                    json!({"kind": "ambiguous", "candidates": candidates})
                }
                Resolution::Unresolved => json!({"kind": "unresolved"}),
            };
            json_result(&r)
        }
        "explain_hit" => match searcher.explain(s("query"), s("path")) {
            Ok(e) => json_result(&json!({"explanation": e})),
            Err(e) => tool_error(e.to_string()),
        },
        other => tool_error(format!("unknown tool: {other}")),
    }
}

fn rpc_error(id: Value, code: i64, msg: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg.into()}})
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Handle one JSON-RPC message. `None` for notifications, which get no reply.
pub fn handle(vault: &Vault, searcher: &Searcher, line: &str) -> Option<Value> {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(rpc_error(Value::Null, -32700, format!("parse error: {e}"))),
    };
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    // Notifications carry no id and expect no response.
    let id = id?;

    let result = match method {
        "initialize" => json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "okilum", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Read-only access to a plain-Markdown vault: list, search, read, \
                             render, resolve links, backlinks. Nothing here writes to the vault."
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            call_tool(vault, searcher, name, &args)
        }
        other => return Some(rpc_error(id, -32601, format!("method not found: {other}"))),
    };
    Some(rpc_ok(id, result))
}

/// Serve MCP over stdio until stdin closes.
pub fn serve(vault: &Vault, searcher: &Searcher) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = handle(vault, searcher, &line) {
            writeln!(out, "{resp}")?;
            out.flush()?;
        }
    }
    Ok(())
}
