//! Command-line parsing, driven through the real binary: a flag that takes a
//! value must not swallow a missing value or the next flag and then fall back
//! to an index directory the user never asked for.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// An isolated working directory holding a tiny synthetic vault. Every
/// invocation runs with this as its cwd, so a relative directory created by a
/// misparsed flag (`--help`, `-h`, `.okilum-index`) lands where the test can
/// see it.
struct Sandbox {
    _temp: tempfile::TempDir,
    cwd: PathBuf,
    vault: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("cwd");
        let vault = cwd.join("vault");
        write_vault(&vault);
        Sandbox {
            _temp: temp,
            cwd,
            vault,
        }
    }

    fn run(&self, args: &[&str], env: &[(&str, &Path)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_okilum-cored"));
        command
            .args(args)
            .current_dir(&self.cwd)
            .env_remove("OKILUM_VAULT")
            .env_remove("OKILUM_INDEX_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().expect("run okilum-cored")
    }

    /// Every entry under the cwd other than the vault's own notes, sorted, so a
    /// stray directory created anywhere in the sandbox shows up by name.
    ///
    /// Entries stay `PathBuf`s and the expected ones are built with
    /// `Path::join`, so the comparison is by path components and holds on
    /// Windows, where `strip_prefix` yields `vault\demo.md`, not `vault/demo.md`.
    fn strays(&self) -> Vec<PathBuf> {
        let vault = Path::new("vault");
        let expected = [
            vault.to_path_buf(),
            vault.join("demo.md"),
            vault.join("alpha.md"),
        ];
        let mut found = Vec::new();
        collect(&self.cwd, &self.cwd, &mut found);
        found.retain(|p| !expected.contains(p));
        found.sort();
        found
    }
}

fn write_vault(root: &Path) {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("demo.md"), "# Demo\n\nLinks [[alpha]].\n").unwrap();
    fs::write(root.join("alpha.md"), "# Alpha\n\nback to [[demo]]\n").unwrap();
}

fn collect(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let rel = path.strip_prefix(base).unwrap();
        out.push(rel.to_path_buf());
        if path.is_dir() {
            collect(base, &path, out);
        }
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The invocation was rejected at argument parsing: exit 2, the named flag in
/// the message, the usage text, and nothing created anywhere in the sandbox.
fn assert_missing_value(sandbox: &Sandbox, output: &Output, flag: &str, untouched: &[&Path]) {
    let err = stderr(output);
    assert_eq!(output.status.code(), Some(2), "stderr: {err}");
    assert!(
        err.contains(&format!("missing value for {flag}")),
        "stderr must name the flag: {err}"
    );
    assert!(err.contains("USAGE:"), "stderr must carry usage: {err}");
    assert!(
        !err.contains("scanned") && !err.contains("indexed into"),
        "must stop before scanning: {err}"
    );
    assert!(output.stdout.is_empty(), "nothing on stdout");
    assert_eq!(sandbox.strays(), Vec::<PathBuf>::new(), "nothing created");
    for path in untouched {
        assert!(!path.exists(), "{} must not be created", path.display());
    }
}

/// The invocation indexed for real: exit 0, the scan saw both notes, and the
/// index directory it reported now holds files.
fn assert_indexed(output: &Output, index_dir: &Path) {
    let err = stderr(output);
    assert!(output.status.success(), "stderr: {err}");
    assert!(err.contains("scanned 2 notes"), "stderr: {err}");
    assert!(err.contains("indexed into"), "stderr: {err}");
    assert!(
        fs::read_dir(index_dir)
            .unwrap_or_else(|e| panic!("{} must exist: {e}", index_dir.display()))
            .next()
            .is_some(),
        "{} must hold the built index",
        index_dir.display()
    );
}

// Positive control for the "nothing created" check in `assert_missing_value`:
// a fresh sandbox reports no strays, and directories a misparsed flag would
// create, at the top level or inside the vault, are reported by name.
#[test]
fn strays_reports_only_entries_outside_the_vault_notes() {
    let s = Sandbox::new();
    assert_eq!(s.strays(), Vec::<PathBuf>::new());
    fs::create_dir(s.cwd.join("--help")).unwrap();
    fs::create_dir(s.vault.join(".okilum-index")).unwrap();
    assert_eq!(
        s.strays(),
        vec![
            PathBuf::from("--help"),
            Path::new("vault").join(".okilum-index"),
        ]
    );
}

#[test]
fn trailing_index_dir_is_rejected_even_with_environment_index_dir() {
    let s = Sandbox::new();
    let env_index = s.cwd.join("env-index");
    let vault = s.vault.to_str().unwrap();
    let out = s.run(
        &["index", "--vault", vault, "--index-dir"],
        &[("OKILUM_INDEX_DIR", &env_index)],
    );
    assert_missing_value(
        &s,
        &out,
        "--index-dir",
        &[&env_index, &s.vault.join(".okilum-index")],
    );
}

#[test]
fn index_dir_followed_by_help_is_rejected() {
    let s = Sandbox::new();
    let vault = s.vault.to_str().unwrap();
    let out = s.run(&["index", "--vault", vault, "--index-dir", "--help"], &[]);
    assert_missing_value(&s, &out, "--index-dir", &[&s.cwd.join("--help")]);
}

#[test]
fn index_dir_followed_by_short_flag_is_rejected() {
    let s = Sandbox::new();
    let vault = s.vault.to_str().unwrap();
    let out = s.run(&["index", "--vault", vault, "--index-dir", "-h"], &[]);
    assert_missing_value(&s, &out, "--index-dir", &[&s.cwd.join("-h")]);
}

#[test]
fn trailing_vault_is_rejected_without_environment_vault() {
    let s = Sandbox::new();
    let out = s.run(&["index", "--vault"], &[]);
    assert_missing_value(&s, &out, "--vault", &[]);
}

#[test]
fn trailing_vault_is_rejected_with_environment_vault() {
    let s = Sandbox::new();
    let out = s.run(&["index", "--vault"], &[("OKILUM_VAULT", &s.vault)]);
    assert_missing_value(&s, &out, "--vault", &[&s.vault.join(".okilum-index")]);
}

#[test]
fn vault_followed_by_flag_is_rejected() {
    let s = Sandbox::new();
    let index = s.cwd.join("explicit-index");
    let out = s.run(
        &["index", "--vault", "--index-dir", index.to_str().unwrap()],
        &[("OKILUM_VAULT", &s.vault)],
    );
    assert_missing_value(
        &s,
        &out,
        "--vault",
        &[&index, &s.vault.join(".okilum-index")],
    );
}

// Positive control for every rejection above: the same sandbox and binary do
// index when the arguments are valid, and explicit values beat the
// environment.
#[test]
fn explicit_values_index_and_override_environment() {
    let s = Sandbox::new();
    let index = s.cwd.join("explicit-index");
    let env_vault = s.cwd.join("env-vault-absent");
    let env_index = s.cwd.join("env-index");
    let out = s.run(
        &[
            "index",
            "--vault",
            s.vault.to_str().unwrap(),
            "--index-dir",
            index.to_str().unwrap(),
        ],
        &[
            ("OKILUM_VAULT", &env_vault),
            ("OKILUM_INDEX_DIR", &env_index),
        ],
    );
    assert_indexed(&out, &index);
    assert!(!env_vault.exists() && !env_index.exists());
    assert!(!s.vault.join(".okilum-index").exists());
}

#[test]
fn environment_only_invocation_indexes() {
    let s = Sandbox::new();
    let index = s.cwd.join("env-index");
    let out = s.run(
        &["index"],
        &[("OKILUM_VAULT", &s.vault), ("OKILUM_INDEX_DIR", &index)],
    );
    assert_indexed(&out, &index);
    assert!(!s.vault.join(".okilum-index").exists());
}

#[test]
fn default_index_dir_lives_in_the_vault() {
    let s = Sandbox::new();
    let out = s.run(&["index", "--vault", s.vault.to_str().unwrap()], &[]);
    assert_indexed(&out, &s.vault.join(".okilum-index"));
}

#[test]
fn dash_leading_names_stay_usable_through_dot_slash() {
    let s = Sandbox::new();
    let dash_vault = s.cwd.join("-vault");
    write_vault(&dash_vault);
    let out = s.run(
        &["index", "--vault", "./-vault", "--index-dir", "./-index"],
        &[],
    );
    assert_indexed(&out, &s.cwd.join("-index"));
    assert!(!dash_vault.join(".okilum-index").exists());
}
