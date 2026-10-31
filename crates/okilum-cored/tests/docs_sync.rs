//! The product contract and the protocol document must describe what this
//! binary actually serves (#1943). Agents make decisions from `docs/PRD.md` and
//! `docs/PROTOCOL.md`, so a shipped capability described as missing - or a
//! served op/tool missing from the tables - is a defect, not a typo.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

fn doc(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The body of the `## <heading>` section, up to the next `## ` heading.
fn section<'a>(text: &'a str, heading: &str) -> &'a str {
    let start = text
        .find(&format!("\n## {heading}\n"))
        .unwrap_or_else(|| panic!("no section {heading:?}"));
    let body = &text[start + 1..];
    let end = body[3..].find("\n## ").map_or(body.len(), |i| i + 3);
    &body[..end]
}

/// First-column backticked names of the Markdown table rows in `text`.
fn first_column_names(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split('`').next())
        .map(str::to_owned)
        .collect()
}

fn vault() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("demo.md"), "# Demo\n\nSee [[alpha]].\n").unwrap();
    fs::write(root.join("alpha.md"), "# Alpha\n").unwrap();
    (temp, root)
}

/// Feeds `lines` to the binary in `mode` and returns every JSON reply line.
fn exchange(mode: Option<&str>, root: &Path, lines: &[String]) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_okilum-cored"));
    if let Some(mode) = mode {
        command.arg(mode);
    }
    let mut child = command
        .arg("--vault")
        .arg(root)
        .arg("--index-dir")
        .arg(root.with_file_name("index"))
        .env_remove("OKILUM_VAULT")
        .env_remove("OKILUM_INDEX_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn okilum-cored");
    {
        let mut stdin = child.stdin.take().unwrap();
        for line in lines {
            writeln!(stdin, "{line}").unwrap();
        }
    }
    let replies = BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
        .collect();
    assert!(child.wait().unwrap().success());
    replies
}

#[test]
fn protocol_mcp_table_lists_exactly_the_served_tools() {
    let protocol = doc("PROTOCOL.md");
    let documented = first_column_names(section(&protocol, "MCP — the same core, a second wire"));
    let (_temp, root) = vault();
    let replies = exchange(
        Some("mcp"),
        &root,
        &[r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#.to_owned()],
    );
    let served: BTreeSet<String> = replies[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    // Positive control: the parser did read a table, so equality means something.
    assert!(documented.contains("read_note"), "{documented:?}");
    assert_eq!(documented, served);
}

#[test]
fn protocol_ops_table_lists_exactly_the_served_ops() {
    let protocol = doc("PROTOCOL.md");
    let ops: BTreeSet<String> = section(&protocol, "Ops")
        .lines()
        .filter_map(|line| line.split(r#""op":""#).nth(1))
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_owned)
        .collect();
    assert!(ops.contains("resolve"), "{ops:?}");
    let (_temp, root) = vault();
    // Every documented op is answered; one undocumented op is the positive
    // control that "unknown op" would be detected if the daemon lacked one.
    let mut requests: Vec<String> = ops
        .iter()
        .map(|op| {
            format!(r#"{{"id":"{op}","op":"{op}","path":"demo.md","q":"demo","target":"alpha"}}"#)
        })
        .collect();
    requests.push(r#"{"id":"control","op":"write"}"#.to_owned());
    let replies = exchange(None, &root, &requests);
    assert_eq!(replies[0]["ready"], true);
    for reply in &replies[1..] {
        let id = reply["id"].as_str().unwrap();
        let unknown = reply["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("unknown op"));
        assert_eq!(unknown, id == "control", "{reply}");
    }
    assert_eq!(replies.len(), ops.len() + 2);
}

#[test]
fn the_brain_entrypoint_is_documented_as_shipped() {
    let help = Command::new(env!("CARGO_BIN_EXE_okilum-cored"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("okilum-cored brain"), "{help}");

    let protocol = doc("PROTOCOL.md");
    assert!(protocol.contains("`okilum-cored brain`"));
    assert!(!protocol.contains("specified but not implemented"));
    assert!(!doc("ai-brain-contracts.md").contains("not yet implemented"));
}

#[test]
fn prd_scopes_the_write_ban_to_the_reader_protocol() {
    let prd = doc("PRD.md");
    let canonical = section(&prd, "2. Canonical data");
    assert!(!canonical.contains("v0 does not write to your notes"));
    assert!(canonical.contains("reader protocol does not write"));
    // Every approved phase that writes notes is named where the rule is stated.
    for phase in ["#354", "#360", "#440", "#636", "#1862", "ai-brain-poc.md"] {
        assert!(canonical.contains(phase), "§2 omits {phase}");
    }
    assert!(!section(&prd, "3. v0 scope").contains("Editing external"));
    let non_goals = section(&prd, "9. Non-goals for v0");
    assert!(!non_goals.contains("A Markdown editor. Plugin API."));
    assert!(non_goals.contains("#354"));
}

#[test]
fn prd_describes_in_app_rename_as_updating_links() {
    let prd = doc("PRD.md");
    let identity = section(&prd, "4. Note identity and link resolution");
    assert!(!identity.contains("a rename breaks links exactly as it does today"));
    assert!(identity.contains("#440"));
    let open = section(&prd, "10. Still open");
    assert!(open.contains("outside Okilum"), "{open}");
}
