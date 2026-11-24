//! Read-only view commands shared by the CLI and MCP.

use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Utc};
use okilum_core::{
    agent_imports::{RootedSource, Source as _},
    records::{FieldSchema, Projection, Source},
    vault::note_path,
    view::{self, Finding, RunContext, Status, ViewDoc, ViewResult},
    Vault,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub const USAGE: &str = "okilum-cored view <file> [--arg KEY=VALUE]... [--as-of RFC3339] [--format json|md] [--all-fields]
okilum-cored view --validate <file|dir> [--coverage]

Both forms accept --vault <PATH> or use OKILUM_VAULT. View paths are vault-relative.
";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRequest {
    pub path: String,
    #[serde(default)]
    args: BTreeMap<String, Value>,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    all_fields: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidateRequest {
    pub path: String,
}

#[derive(Serialize)]
pub struct Validation {
    pub valid: bool,
    pub findings: Vec<Finding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<Vec<view::Coverage>>,
}

fn checked_path(path: &str) -> Result<PathBuf> {
    let relative = Path::new(path);
    ensure!(
        !path.is_empty()
            && !path.contains(['\\', ':', '\0'])
            && relative
                .components()
                .all(|part| matches!(part, Component::CurDir | Component::Normal(_))),
        "expected a vault-relative path: {path}"
    );
    Ok(relative
        .components()
        .filter(|part| matches!(part, Component::Normal(_)))
        .collect())
}

fn load_doc(root: &RootedSource, path: &Path) -> Result<Result<ViewDoc, Vec<Finding>>> {
    let path = note_path(path);
    if path == view::builtin::DOMAIN_PATH {
        let source = view::builtin::domain_source(root.root()).map_err(anyhow::Error::msg)?;
        return Ok(view::parse(&path, &source));
    }
    let source = root
        .read(&root.root().join(&path), usize::MAX - 1)
        .map_err(|error| anyhow::anyhow!("cannot read view '{path}': {error:?}"))?;
    Ok(view::parse(&path, &source))
}

struct Publication {
    vault: Vault,
    sources: HashMap<String, Vec<u8>>,
    projection: Projection,
}

impl Publication {
    fn open(root: &Path, schema: Arc<FieldSchema>) -> Result<Self> {
        let (vault, sources) = Vault::scan_snapshot_with(root, &mut |_, _| Ok(()))?;
        let projection = Projection::build(
            schema,
            sources.iter().map(|(path, bytes)| Source {
                path,
                bytes,
                mtime: fs::symlink_metadata(root.join(path))
                    .ok()
                    .filter(|metadata| metadata.is_file())
                    .and_then(|metadata| metadata.modified().ok())
                    .map(|time| DateTime::<Utc>::from(time).fixed_offset()),
            }),
        );
        Ok(Self {
            vault,
            sources,
            projection,
        })
    }

    fn read(&self, path: &str) -> Option<String> {
        Some(
            std::str::from_utf8(self.sources.get(path)?)
                .ok()?
                .to_owned(),
        )
    }
}

pub fn run(root: &Path, request: RunRequest) -> Result<Result<ViewResult, Vec<Finding>>> {
    let as_of = request
        .as_of
        .as_deref()
        .map(DateTime::parse_from_rfc3339)
        .transpose()
        .context("invalid as_of timestamp (expected RFC 3339)")?
        .unwrap_or_else(|| Utc::now().fixed_offset());
    let path = checked_path(&request.path)?;
    let root = open_root(root)?;
    let doc = match load_doc(&root, &path)? {
        Ok(doc) => doc,
        Err(findings) => return Ok(Err(findings)),
    };
    let schema = Arc::new(FieldSchema::load(root.root())?);
    let publication = Publication::open(root.root(), schema)?;
    let read = |path: &str| publication.read(path);
    let mut context =
        RunContext::new(&publication.projection, &publication.vault, as_of).with_vault_snapshots();
    context.args = request.args;
    context.all_fields = request.all_fields;
    context.read_source = Some(&read);
    Ok(Ok(view::run(&doc, &context)))
}

fn open_root(root: &Path) -> Result<RootedSource> {
    RootedSource::new(root)
        .map_err(|error| anyhow::anyhow!("cannot open vault '{}': {error:?}", root.display()))
}

fn collect_views(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root.join(directory))? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = directory.join(entry.file_name());
        if kind.is_dir() {
            collect_views(root, &path, paths)?;
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn validation_paths(root: &Path, target: &Path) -> Result<Vec<PathBuf>> {
    let mut absolute = root.to_path_buf();
    for part in target.components() {
        absolute.push(part);
        ensure!(
            !fs::symlink_metadata(&absolute)?.is_symlink(),
            "view path crosses a symlink: {}",
            target.display()
        );
    }
    if !absolute.is_dir() {
        return Ok(vec![target.to_owned()]);
    }
    let mut paths = Vec::new();
    collect_views(root, target, &mut paths)?;
    paths.sort();
    Ok(paths)
}

pub fn validate(root: &Path, path: &str, include_coverage: bool) -> Result<Validation> {
    let target = checked_path(path)?;
    let root = open_root(root)?;
    let paths = if target == Path::new(view::builtin::DOMAIN_PATH) {
        vec![target]
    } else {
        validation_paths(root.root(), &target)?
    };
    let schema = Arc::new(FieldSchema::load(root.root())?);
    let publication = include_coverage
        .then(|| Publication::open(root.root(), schema.clone()))
        .transpose()?;
    let mut findings = Vec::new();
    let mut coverage = include_coverage.then(Vec::new);
    let as_of = Utc::now().fixed_offset();
    for path in paths {
        match load_doc(&root, &path)? {
            Err(errors) => findings.extend(errors),
            Ok(doc) => {
                let checks = view::validate(&doc, &schema);
                if let (Some(publication), Some(coverage)) = (&publication, &mut coverage) {
                    let read = |path: &str| publication.read(path);
                    let mut context =
                        RunContext::new(&publication.projection, &publication.vault, as_of);
                    context.read_source = Some(&read);
                    coverage.extend(view::coverage(&doc, &context));
                }
                findings.extend(checks);
            }
        }
    }
    Ok(Validation {
        valid: !findings.iter().any(Finding::is_error),
        findings,
        coverage,
    })
}

#[derive(Clone, Copy)]
enum Format {
    Json,
    Markdown,
}

fn value<'a>(args: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<&'a str> {
    args.next()
        .filter(|value| !value.starts_with('-'))
        .map(String::as_str)
        .with_context(|| format!("missing value for {flag}"))
}

fn cli(args: &[String]) -> Result<u8> {
    let mut root = std::env::var_os("OKILUM_VAULT").map(PathBuf::from);
    let mut options = RunRequest {
        path: String::new(),
        args: BTreeMap::new(),
        as_of: None,
        all_fields: false,
    };
    let mut validate_only = false;
    let mut coverage = false;
    let mut format = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                io::stdout().write_all(USAGE.as_bytes())?;
                return Ok(0);
            }
            "--vault" => root = Some(value(&mut args, arg)?.into()),
            "--validate" => validate_only = true,
            "--coverage" => coverage = true,
            "--format" => {
                format = Some(match value(&mut args, arg)? {
                    "json" => Format::Json,
                    "md" => Format::Markdown,
                    other => bail!("unknown view format '{other}'"),
                });
            }
            "--as-of" => options.as_of = Some(value(&mut args, arg)?.into()),
            "--all-fields" => options.all_fields = true,
            "--arg" => {
                let arg = value(&mut args, arg)?;
                let (key, value) = arg
                    .split_once('=')
                    .filter(|(key, _)| !key.is_empty())
                    .context("--arg requires KEY=VALUE")?;
                options.args.insert(key.into(), Value::String(value.into()));
            }
            path if !path.starts_with('-') && options.path.is_empty() => options.path = path.into(),
            other => bail!("unexpected view argument '{other}'"),
        }
    }
    let root = root.context("--vault is required (or set OKILUM_VAULT)")?;
    ensure!(!options.path.is_empty(), "a view path is required");
    let mut stdout = io::stdout().lock();
    if validate_only {
        ensure!(
            options.args.is_empty()
                && options.as_of.is_none()
                && !options.all_fields
                && format.is_none(),
            "run options cannot be used with --validate"
        );
        let validation = validate(&root, &options.path, coverage)?;
        writeln!(stdout, "{}", serde_json::to_string_pretty(&validation)?)?;
        return Ok(u8::from(!validation.valid));
    }
    ensure!(!coverage, "--coverage requires --validate");
    match run(&root, options)? {
        Err(findings) => {
            writeln!(io::stderr(), "{}", serde_json::to_string_pretty(&findings)?)?;
            Ok(1)
        }
        Ok(result) => {
            let rendered = match format.unwrap_or(Format::Json) {
                Format::Json => view::render_json(&result)?,
                Format::Markdown => view::render_markdown(&result),
            };
            stdout.write_all(rendered.as_bytes())?;
            let errors = result.findings.iter().any(Finding::is_error)
                || result
                    .sections
                    .iter()
                    .any(|section| section.status == Status::Error);
            Ok(u8::from(errors))
        }
    }
}

pub fn run_cli(args: &[String]) -> u8 {
    match cli(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            2
        }
    }
}
