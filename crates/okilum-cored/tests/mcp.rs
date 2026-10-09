//! The MCP adapter, driven as a real client would: spawn the binary, speak
//! JSON-RPC over its stdin/stdout, read the answers.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

// Every test spawns its own server over its own vault. Keyed on a counter as
// well as the pid: cargo runs these in parallel threads of one process, and a
// shared directory would have them wiping each other's vault mid-scan.
static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn fixture() -> PathBuf {
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("okilum-mcp-{}-{seq}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::create_dir_all(root.join("dup")).unwrap();
    fs::write(
        root.join("demo.md"),
        "# Demo\n\nLinks [[alpha]] and [[notes/alpha]]. The zarquon device.\n",
    )
    .unwrap();
    fs::write(
        root.join("notes/alpha.md"),
        "---\ntitle: Alpha\ntags: [widget]\n---\n\n# Alpha\n\nback to [[demo]]\n",
    )
    .unwrap();
    fs::write(root.join("dup/alpha.md"), "# Other alpha\n").unwrap();
    root
}

struct Client {
    child: std::process::Child,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Client {
    fn spawn(vault: &PathBuf) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_okilum-cored"))
            .args(["mcp", "--vault"])
            .arg(vault)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn okilum-cored");
        let reader = BufReader::new(child.stdout.take().unwrap());
        Client {
            child,
            reader,
            next_id: 1,
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{req}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read reply");
        let v: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("reply is not JSON ({e}): {line:?}"));
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], id, "reply id must match request id");
        v
    }

    fn notify(&mut self, method: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "method": method})).unwrap();
        stdin.flush().unwrap();
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        assert!(
            r.get("error").is_none(),
            "tool call must not be an RPC error: {r}"
        );
        r["result"].clone()
    }

    fn tool_json(&mut self, name: &str, args: Value) -> Value {
        let res = self.tool(name, args);
        assert_ne!(res["isError"], true, "tool reported an error: {res}");
        let text = res["content"][0]["text"].as_str().expect("text content");
        serde_json::from_str(text).expect("tool text is JSON")
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn handshake_then_tools_list() {
    let v = fixture();
    let mut c = Client::spawn(&v);
    let init = c.call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "okilum");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    c.notify("notifications/initialized");

    let list = c.call("tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "list_notes",
        "search",
        "read_note",
        "render_note",
        "note_blocks",
        "backlinks",
        "resolve_link",
        "explain_hit",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}: {names:?}"
        );
    }
}

#[test]
fn a_notification_gets_no_reply_and_the_next_call_still_works() {
    // If the server answered notifications, the client's next read would get
    // the wrong message. Prove the stream stays aligned.
    let v = fixture();
    let mut c = Client::spawn(&v);
    c.notify("notifications/initialized");
    let r = c.call("ping", json!({}));
    assert!(r["result"].is_object());
}

#[test]
fn search_and_explain_through_mcp() {
    let v = fixture();
    let mut c = Client::spawn(&v);
    let hits = c.tool_json("search", json!({"query": "zarquon"}));
    assert_eq!(hits[0]["path"], "demo.md");

    let ex = c.tool_json(
        "explain_hit",
        json!({"query": "zarquon", "path": "demo.md"}),
    );
    assert!(ex["explanation"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("zarquon"));
    let none = c.tool_json(
        "explain_hit",
        json!({"query": "zarquon", "path": "dup/alpha.md"}),
    );
    assert!(none["explanation"].is_null());
}

#[test]
fn resolve_link_never_silently_picks_a_winner() {
    let v = fixture();
    let mut c = Client::spawn(&v);
    let amb = c.tool_json("resolve_link", json!({"target": "alpha"}));
    assert_eq!(amb["kind"], "ambiguous");
    assert_eq!(amb["candidates"].as_array().unwrap().len(), 2);
    let ok = c.tool_json("resolve_link", json!({"target": "notes/alpha"}));
    assert_eq!(ok["kind"], "resolved");
    assert_eq!(ok["path"], "notes/alpha.md");
    let no = c.tool_json("resolve_link", json!({"target": "nothing-here"}));
    assert_eq!(no["kind"], "unresolved");
}

#[test]
fn read_note_rewrites_links_and_backlinks_mark_ambiguity() {
    let v = fixture();
    let mut c = Client::spawn(&v);
    let src = c.tool("read_note", json!({"path": "demo.md"}));
    let text = src["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("okilum://ambiguous/alpha"), "{text}");
    assert!(text.contains("okilum://open/notes/alpha.md"), "{text}");

    let bl = c.tool_json("backlinks", json!({"path": "notes/alpha.md"}));
    let from_demo: Vec<&Value> = bl
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["path"] == "demo.md")
        .collect();
    assert_eq!(from_demo.len(), 1, "one backlink per (source, line): {bl}");
    // The same line names notes/alpha.md precisely, so the mark is cleared.
    assert_eq!(from_demo[0]["ambiguous"], false);
}

#[test]
fn a_bad_query_is_a_tool_error_not_a_crash_and_not_nonsense() {
    let v = fixture();
    let mut c = Client::spawn(&v);
    let res = c.tool("search", json!({"query": "nosuchfield:zarquon"}));
    assert_eq!(res["isError"], true, "{res}");
    let msg = res["content"][0]["text"].as_str().unwrap();
    assert!(msg.contains("nosuchfield"), "{msg}");
    // and the server is still alive afterwards
    let r = c.call("ping", json!({}));
    assert!(r["result"].is_object());
}

#[test]
fn unknown_method_is_a_proper_rpc_error() {
    let v = fixture();
    let mut c = Client::spawn(&v);
    let r = c.call("resources/list", json!({}));
    assert_eq!(r["error"]["code"], -32601);
}
