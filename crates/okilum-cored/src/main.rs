//! JSON-lines sidecar over stdin/stdout, plus an `index` subcommand.
//!
//! This is the reusable half of Okilum: any second client - a web view, an MCP
//! adapter for agent tooling - reaches the vault through this frozen protocol
//! rather than through a private API. Protocol: `docs/PROTOCOL.md`.

use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;
mod mcp;

use okilum_core::{ir, render_html, Resolution, Searcher, Vault};

const USAGE: &str = "\
okilum-cored - JSON-lines sidecar over a Okilum vault

USAGE:
    okilum-cored [index|mcp] --vault <PATH> [--index-dir <PATH>]

    With no subcommand, serves the JSON-lines protocol on stdin/stdout.
    `index` builds the search index and exits.
    `mcp`   serves the Model Context Protocol (JSON-RPC over stdio) instead —
            the same operations as tools, for agent clients.

    --vault may also be given as OKILUM_VAULT, and --index-dir as
    OKILUM_INDEX_DIR. There is no default vault: guessing one is a good way to
    index the wrong directory.
";

/// Index directory name, created inside the vault unless overridden. Derived
/// data - deleting it must always be safe.
const INDEX_DIR_NAME: &str = ".okilum-index";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "brain") {
        return okilum_brain::service::run_cli(&args[2..]);
    }
    let mut vault_root = std::env::var_os("OKILUM_VAULT").map(PathBuf::from);
    let mut index_dir_arg = std::env::var_os("OKILUM_INDEX_DIR").map(PathBuf::from);
    let mut rest = args.iter().skip(1).peekable();
    let mut do_index = false;
    let mut do_mcp = false;
    while let Some(a) = rest.next() {
        match a.as_str() {
            "index" => do_index = true,
            "mcp" => do_mcp = true,
            "--vault" => vault_root = rest.next().map(PathBuf::from),
            "--index-dir" => index_dir_arg = rest.next().map(PathBuf::from),
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            other => {
                eprintln!("unknown argument: {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let Some(corpus) = vault_root else {
        eprintln!("--vault is required (or set OKILUM_VAULT)\n{USAGE}");
        std::process::exit(2);
    };
    let index_dir = index_dir_arg.unwrap_or_else(|| corpus.join(INDEX_DIR_NAME));

    if do_index {
        let t = std::time::Instant::now();
        let vault = Vault::scan(&corpus)?;
        eprintln!("scanned {} notes in {:?}", vault.notes.len(), t.elapsed());
        let t = std::time::Instant::now();
        Searcher::build(&vault, &index_dir)?;
        eprintln!("indexed into {} in {:?}", index_dir.display(), t.elapsed());
        return Ok(());
    }

    let vault = Vault::scan(&corpus)?;
    let searcher = Searcher::open_or_build(&vault, &index_dir)?;
    if do_mcp {
        // No readiness line: MCP clients speak first (initialize), and a stray
        // non-JSON-RPC line on stdout would break them.
        return mcp::serve(&vault, &searcher);
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    // readiness marker for the Qt client
    writeln!(
        out,
        "{}",
        json!({"ready": true, "notes": vault.notes.len()})
    )?;
    out.flush()?;

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let resp = handle(&vault, &searcher, &line);
        writeln!(out, "{resp}")?;
        out.flush()?;
    }
    Ok(())
}

fn handle(vault: &Vault, searcher: &Searcher, line: &str) -> String {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "error": format!("bad json: {e}")}).to_string(),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let op = req.get("op").and_then(Value::as_str).unwrap_or("");
    let result = match op {
        "list" => Ok(json!({"notes": vault.notes})),
        "render" => {
            let path = req.get("path").and_then(Value::as_str).unwrap_or("");
            render_html(vault, path, "InspiredGitHub")
                .map(|html| json!({"html": html, "title": vault.note_title(path)}))
        }
        "doc" => {
            let path = req.get("path").and_then(Value::as_str).unwrap_or("");
            ir::build_doc(vault, path).map(|d| json!({"doc": d}))
        }
        "backlinks" => {
            let path = req.get("path").and_then(Value::as_str).unwrap_or("");
            Ok(json!({"backlinks": vault.backlinks(path)}))
        }
        "search" => {
            let q = req.get("q").and_then(Value::as_str).unwrap_or("");
            searcher.search(q, 30).map(|hits| json!({"hits": hits}))
        }
        // v0.3: why did `path` match `q`? `explanation` is the engine's own
        // scoring tree as JSON text, or null when the note is not a hit at all.
        "explain" => {
            let q = req.get("q").and_then(Value::as_str).unwrap_or("");
            let p = req.get("path").and_then(Value::as_str).unwrap_or("");
            searcher.explain(q, p).map(|e| json!({"explanation": e}))
        }
        "resolve" => {
            let t = req.get("target").and_then(Value::as_str).unwrap_or("");
            // v0.2, optional: a `../` target is relative to the note that
            // contains it and cannot be resolved without knowing which.
            let from = req.get("from").and_then(Value::as_str).unwrap_or("");
            // `path` keeps its v0.1 meaning exactly: the resolved note, or null.
            // An ambiguous target reports null AND lists its candidates, so a
            // v0.1 client sees "unresolved" - wrong, but safe - while a v0.2
            // client can offer the choice. Reporting one candidate as `path`
            // would make old clients silently confident instead.
            match vault.resolve_from(t, from) {
                Resolution::Resolved { path } => Ok(json!({"path": path, "candidates": []})),
                Resolution::Ambiguous { candidates } => {
                    Ok(json!({"path": Value::Null, "candidates": candidates}))
                }
                Resolution::Unresolved => Ok(json!({"path": Value::Null, "candidates": []})),
            }
        }
        other => Err(anyhow::anyhow!("unknown op {other}")),
    };
    match result {
        Ok(mut v) => {
            let obj = v.as_object_mut().unwrap();
            obj.insert("ok".into(), json!(true));
            obj.insert("id".into(), id);
            v.to_string()
        }
        Err(e) => json!({"ok": false, "id": id, "error": e.to_string()}).to_string(),
    }
}
