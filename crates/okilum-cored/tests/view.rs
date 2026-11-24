use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../okilum-core/tests/fixtures");
const QUESTIONS: &str = "@views/questions.md";
const INVALID: &str = "@Schema/tests/view-invalid/bad-show.md";
const AS_OF: &str = "2026-11-24T01:30:00+02:00";

fn command(vault: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_okilum-cored"));
    command
        .env("OKILUM_VAULT", vault)
        .env_remove("OKILUM_INDEX_DIR");
    command
}

fn cli(vault: &Path, args: &[&str]) -> Output {
    command(vault).arg("view").args(args).output().unwrap()
}

fn tree_hash(root: &Path) -> Vec<u8> {
    fn visit(root: &Path, dir: &Path, hash: &mut Sha256) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
            hash.update((relative.len() as u64).to_le_bytes());
            hash.update(relative.as_bytes());
            if entry.file_type().unwrap().is_dir() {
                hash.update(b"d");
                visit(root, &path, hash);
            } else {
                assert!(entry.file_type().unwrap().is_file(), "{path:?}");
                hash.update(b"f");
                hash.update(Sha256::digest(fs::read(path).unwrap()));
            }
        }
    }

    let mut hash = Sha256::new();
    visit(root, root, &mut hash);
    hash.finalize().to_vec()
}

#[test]
fn view_cli_and_mcp_leave_the_fixture_vault_unchanged() {
    let vault = Path::new(FIXTURES).join("views");
    let before = tree_hash(&vault);
    let golden = fs::read(Path::new(FIXTURES).join("view-golden/questions.json")).unwrap();

    let json = cli(&vault, &[QUESTIONS, "--format", "json", "--as-of", AS_OF]);
    assert!(json.status.success(), "{json:?}");
    assert_eq!(
        String::from_utf8_lossy(&json.stdout),
        String::from_utf8_lossy(&golden)
    );

    let domain = cli(
        &vault,
        &[
            "@views/domain.md",
            "--arg",
            "domain=Health",
            "--all-fields",
            "--as-of",
            AS_OF,
        ],
    );
    assert!(domain.status.success(), "{domain:?}");
    let domain: Value = serde_json::from_slice(&domain.stdout).unwrap();
    assert_eq!(domain["params"]["domain"], "Health");
    assert!(domain["sections"][0]["rows"][0]["fields"]["mtime"].is_string());

    let md = cli(&vault, &[QUESTIONS, "--format", "md", "--as-of", AS_OF]);
    assert!(md.status.success(), "{md:?}");
    assert!(String::from_utf8(md.stdout)
        .unwrap()
        .contains("# Waiting for Oleg"));

    for (args, code) in [
        (vec!["--validate", QUESTIONS], 0),
        (vec!["--validate", "@views"], 0),
        (vec![INVALID, "--validate"], 1),
        (vec!["--validate", "@Schema/tests/view-invalid"], 1),
        (vec!["--validate", "missing.md"], 2),
        (vec!["--validate", "../outside.md"], 2),
        (vec!["--validate"], 2),
        (vec![QUESTIONS, "--as-of", "invalid"], 2),
    ] {
        let output = cli(&vault, &args);
        assert_eq!(output.status.code(), Some(code), "{args:?}: {output:?}");
    }

    let coverage = cli(&vault, &["--validate", "@views/domain.md", "--coverage"]);
    assert!(coverage.status.success(), "{coverage:?}");
    let report: Value = serde_json::from_slice(&coverage.stdout).unwrap();
    assert!(
        report["coverage"].as_array().unwrap().iter().any(|entry| {
            entry["field"] == "layer"
                && entry["missing"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|path| path.as_str().unwrap().starts_with("Projects/"))
        }),
        "{report}"
    );

    let mut server = command(&vault)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let run = |args| json!({"name": "run_view", "arguments": args});
    let validate = |path| json!({"name": "validate_view", "arguments": {"path": path}});
    {
        let mut stdin = server.stdin.take().unwrap();
        for (id, (method, params)) in [
            ("tools/list", json!({})),
            (
                "tools/call",
                run(json!({"path": QUESTIONS, "as_of": AS_OF})),
            ),
            ("tools/call", validate(QUESTIONS)),
            ("tools/call", validate(INVALID)),
            (
                "tools/call",
                run(json!({"path": QUESTIONS, "all_fields": "yes"})),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            writeln!(
                stdin,
                "{}",
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
            )
            .unwrap();
        }
    }
    let output = server.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let tools = replies[0]["result"]["tools"].as_array().unwrap();
    for (name, properties) in [
        ("run_view", &["path", "args", "as_of", "all_fields"][..]),
        ("validate_view", &["path"][..]),
    ] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["path"]));
        for property in properties {
            assert!(schema["properties"][property]["type"].is_string(), "{tool}");
        }
    }
    let content = |index: usize| {
        replies[index]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
    };
    assert_eq!(content(1).as_bytes(), golden);
    assert_eq!(
        serde_json::from_str::<Value>(content(2)).unwrap()["valid"],
        true
    );
    let invalid: Value = serde_json::from_str(content(3)).unwrap();
    assert_eq!(invalid["valid"], false);
    assert_eq!(invalid["findings"][0]["path"], INVALID);
    assert!(invalid["findings"][0]["line"].is_number());
    assert!(invalid["findings"][0]["col"].is_number());
    assert_eq!(replies[4]["result"]["isError"], true);
    assert_eq!(tree_hash(&vault), before);
}

#[test]
fn domain_instance_renders_a_gardener_snapshot_with_the_same_sections_as_the_template() {
    let vault = Path::new(FIXTURES).join("views/@Schema/domain-template-vault");
    let before = tree_hash(&vault);
    let render = |args: &[&str]| {
        let output = cli(&vault, args);
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let instance = render(&["@views/domain-health.md", "--as-of", AS_OF]);
    let direct = render(&[
        "@views/domain.md",
        "--arg",
        "domain=health",
        "--as-of",
        AS_OF,
    ]);

    assert_eq!(instance["view_api"], 0);
    assert_eq!(instance["view"]["path"], "@views/domain-health.md");
    assert_eq!(instance["view"]["extends"], "@views/domain.md");
    assert_eq!(instance["params"]["domain"], "Health");
    assert_eq!(instance["sections"], direct["sections"]);

    let sections = instance["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 5);
    for (section, (id, column, expected)) in sections.iter().zip([
        (
            "contexts",
            "title",
            vec!["Sleep tracking", "Mobility routine"],
        ),
        (
            "recent_log",
            "title",
            vec!["Bedtime window", "Sleep review", "Tracking handover"],
        ),
        (
            "open_tasks",
            "text",
            vec!["Make a stretching plan", "Review sleep data"],
        ),
        ("inbox", "title", vec!["Evening routine idea"]),
        ("sources", "title", vec!["Circadian light"]),
    ]) {
        assert_eq!(section["id"], id);
        assert_eq!(section["status"], "ok", "{section}");
        let titles = section["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["fields"][column].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(titles, expected);
    }
    assert_eq!(sections[0]["groups"][0]["key"], json!(["active"]));
    assert_eq!(sections[0]["groups"][1]["key"], json!(["parked"]));

    let scoped = render(&[
        "@views/domain.md",
        "--arg",
        "domain=Health",
        "--arg",
        "context=mobility",
        "--as-of",
        AS_OF,
    ]);
    let scoped = scoped["sections"].as_array().unwrap();
    assert_eq!(scoped[0]["rows"][0]["fields"]["title"], "Mobility routine");
    assert_eq!(scoped[0]["total"], 1);
    assert_eq!(scoped[1]["total"], 0);
    assert_eq!(scoped[2]["total"], 1);
    assert_eq!(scoped[3], sections[3]);
    assert_eq!(scoped[4], sections[4]);
    assert_eq!(tree_hash(&vault), before);
}
