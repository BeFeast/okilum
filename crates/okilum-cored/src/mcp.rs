//! MCP adapter over the same core the JSON-lines protocol exposes (#5).
//!
//! Depth 0 of the t3 integration decided at the spike: no editor patches, no
//! shell dependency, just a stdio JSON-RPC server that any MCP client can
//! spawn. Reader tools share their implementation with the JSON-lines
//! protocol. View tools share their implementation with the view CLI.
//!
//! Protocol version pinned to the MCP spec revision this was written against.
//! Only the subset needed for tools is implemented: `initialize`,
//! `tools/list`, `tools/call`, `ping`, and the `notifications/initialized`
//! no-op. Everything else returns method-not-found rather than pretending.

use std::{
    io::{BufRead, Write},
    path::Path,
};

use crate::view_cmd::{self, ValidateRequest};

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
            "name": "run_view",
            "description": "Run a declarative vault view and return the view_api: 0 JSON result.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Vault-relative view file path"},
                    "args": {
                        "type": "object",
                        "additionalProperties": {"type": ["string", "number"]}
                    },
                    "as_of": {"type": "string", "format": "date-time"},
                    "all_fields": {"type": "boolean", "default": false}
                },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "validate_view",
            "description": "Validate a vault-relative view file or directory of views.",
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
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

// Opening a disk index writes derived files. Defer it until a tool needs it,
// so a session using only view tools can use a read-only vault.
struct Search<'a> {
    directory: &'a Path,
    searcher: Option<Searcher>,
}

impl Search<'_> {
    fn get(&mut self, vault: &Vault) -> anyhow::Result<&Searcher> {
        if self.searcher.is_none() {
            self.searcher = Some(Searcher::open_or_build(vault, self.directory)?);
        }
        Ok(self.searcher.as_ref().expect("opened search index"))
    }
}

fn call_tool(vault: &Vault, search: &mut Search<'_>, name: &str, args: &Value) -> Value {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
    match name {
        "list_notes" => json_result(&json!(vault.notes)),
        "search" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(30)
                .clamp(1, 100) as usize;
            match search
                .get(vault)
                .and_then(|searcher| searcher.search(s("query"), limit))
            {
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
        "explain_hit" => {
            match search
                .get(vault)
                .and_then(|searcher| searcher.explain(s("query"), s("path")))
            {
                Ok(e) => json_result(&json!({"explanation": e})),
                Err(e) => tool_error(e.to_string()),
            }
        }
        "run_view" => run_view(&vault.root, args).unwrap_or_else(|e| tool_error(format!("{e:#}"))),
        "validate_view" => {
            validate_view(&vault.root, args).unwrap_or_else(|e| tool_error(format!("{e:#}")))
        }
        other => tool_error(format!("unknown tool: {other}")),
    }
}

fn run_view(root: &Path, args: &Value) -> anyhow::Result<Value> {
    let request = serde_json::from_value(args.clone())?;
    Ok(match view_cmd::run(root, request)? {
        Ok(result) => text_result(okilum_core::view::render_json(&result)?),
        Err(findings) => tool_error(serde_json::to_string_pretty(&findings)?),
    })
}

fn validate_view(root: &Path, args: &Value) -> anyhow::Result<Value> {
    let request: ValidateRequest = serde_json::from_value(args.clone())?;
    let validation = view_cmd::validate(root, &request.path, false)?;
    Ok(text_result(serde_json::to_string_pretty(&validation)?))
}

fn rpc_error(id: Value, code: i64, msg: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg.into()}})
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Handle one JSON-RPC message. `None` for notifications, which get no reply.
fn handle(vault: &Vault, search: &mut Search<'_>, line: &str) -> Option<Value> {
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
                             render, resolve links, backlinks, run and validate views."
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            call_tool(vault, search, name, &args)
        }
        other => return Some(rpc_error(id, -32601, format!("method not found: {other}"))),
    };
    Some(rpc_ok(id, result))
}

/// Serve MCP over stdio until stdin closes.
pub fn serve(vault: &Vault, index_dir: &Path) -> anyhow::Result<()> {
    let mut search = Search {
        directory: index_dir,
        searcher: None,
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = handle(vault, &mut search, &line) {
            writeln!(out, "{resp}")?;
            out.flush()?;
        }
    }
    Ok(())
}
