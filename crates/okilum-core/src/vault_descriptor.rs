//! Read-only access to the vault's self-description.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

pub use okilum_write_authority::vault_descriptor::{parse, read_bytes, Lifecycle, DIRECTORY, FILE};

pub fn path(root: &Path) -> PathBuf {
    root.join(DIRECTORY).join(FILE)
}

pub fn read(root: &Path) -> Option<Value> {
    parse(&read_bytes(root).ok()??).ok()
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Layout {
    pub schema_version: u32,
    pub managed_by: String,
    pub contract: String,
    pub roots: Roots,
    pub domains: Vec<Domain>,
    pub generated_at: String,
    /// Optional explicit placement for pasted attachments.
    pub attachments: Option<String>,
    #[serde(default, deserialize_with = "deserialize_nested_roots")]
    pub nested_roots: Vec<NestedRoot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct NestedRoot {
    pub path: String,
    pub kind: NestedKind,
    pub tree_sha256: String,
    pub files: u64,
    pub name: Option<String>,
}

fn deserialize_nested_roots<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<NestedRoot>, D::Error> {
    let Value::Array(entries) = Value::deserialize(deserializer)? else {
        eprintln!("Ignoring vault descriptor nested_roots: expected an array");
        return Ok(Vec::new());
    };
    let mut paths = std::collections::BTreeSet::new();
    let mut roots = Vec::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let Ok(entry) = serde_json::from_value::<NestedRoot>(entry) else {
            eprintln!("Ignoring malformed vault descriptor nested_roots[{index}]");
            continue;
        };
        if !entry.path.contains(['\\', '\0'])
            && entry
                .path
                .split('/')
                .all(|part| !part.is_empty() && !matches!(part, "." | ".."))
            && Path::new(&entry.path)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && entry.tree_sha256.len() == 64
            && entry
                .tree_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && paths.insert(entry.path.clone())
        {
            roots.push(entry);
        } else {
            eprintln!("Ignoring invalid or duplicate vault descriptor nested_roots[{index}]");
        }
    }
    Ok(roots)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NestedKind {
    Vault,
    Unit,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum NestedBoundary {
    Vault(String),
    Unit,
}

pub type NestedBoundaries = BTreeMap<String, NestedBoundary>;

/// Match a case variant only when both spellings reach the same folder.
/// Ordinary names and vaults with no declarations need no filesystem probe.
pub(crate) fn boundary_at<'a>(
    root: &Path,
    boundaries: &'a NestedBoundaries,
    relative: &str,
) -> Option<(&'a String, &'a NestedBoundary)> {
    boundaries.get_key_value(relative).or_else(|| {
        if boundaries.is_empty() {
            return None;
        }
        let lower = relative.to_lowercase();
        boundaries.iter().find(|(path, _)| {
            path.to_lowercase() == lower && same_directory(&root.join(path), &root.join(relative))
        })
    })
}

fn same_directory(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (Ok(left), Ok(right)) = (std::fs::metadata(left), std::fs::metadata(right)) else {
            return false;
        };
        left.is_dir() && right.is_dir() && (left.dev(), left.ino()) == (right.dev(), right.ino())
    }
    #[cfg(not(unix))]
    {
        left.canonicalize().ok().is_some_and(|left| {
            left.is_dir() && right.canonicalize().is_ok_and(|right| left == right)
        })
    }
}

pub(crate) fn declared_boundaries(root: &Path) -> NestedBoundaries {
    let LayoutState::Managed(layout) = LayoutState::load(root) else {
        return BTreeMap::new();
    };
    layout
        .nested_roots
        .iter()
        .map(|entry| {
            let boundary = match entry.kind {
                NestedKind::Vault => NestedBoundary::Vault(
                    entry
                        .name
                        .clone()
                        .unwrap_or_else(|| folder_name(Path::new(&entry.path))),
                ),
                NestedKind::Unit => NestedBoundary::Unit,
            };
            (entry.path.clone(), boundary)
        })
        .collect()
}

pub fn boundary_marker(directory: &Path) -> bool {
    path(directory).is_file() || directory.join(".obsidian").is_dir()
}

pub fn marked_boundary(directory: &Path) -> Option<NestedBoundary> {
    boundary_marker(directory).then(|| {
        let name = read(directory)
            .and_then(|descriptor| descriptor.get("name")?.as_str().map(str::to_owned))
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| folder_name(directory));
        NestedBoundary::Vault(name)
    })
}

fn folder_name(directory: &Path) -> String {
    directory
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Roots {
    pub inbox: String,
    pub sources: String,
    pub knowledge: String,
    pub state: String,
    pub log: String,
    pub views: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Domain {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContextId<'a> {
    pub domain: &'a str,
    pub name: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextLayer {
    State,
    Log,
}

#[derive(Clone, Copy, Debug)]
pub struct ContextPath<'a> {
    pub id: ContextId<'a>,
    pub layer: ContextLayer,
    pub kind: Option<&'a str>,
}

impl ContextPath<'_> {
    pub fn is_final(&self) -> bool {
        self.layer == ContextLayer::Log && self.kind == Some("final")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub enum LayoutState {
    #[default]
    Legacy,
    Managed(Box<Layout>),
    Unsupported,
}

impl LayoutState {
    pub fn load(root: &Path) -> Self {
        let mut descriptor = match read_bytes(root) {
            Ok(None) => return Self::Legacy,
            Ok(Some(bytes)) => match parse(&bytes) {
                Ok(Value::Object(descriptor)) => descriptor,
                _ => return Self::Unsupported,
            },
            Err(_) => return Self::Unsupported,
        };
        let Some(value) = descriptor.remove("layout") else {
            return Self::Legacy;
        };
        serde_json::from_value::<Layout>(value)
            .ok()
            .filter(Layout::valid)
            .map_or(Self::Unsupported, |layout| Self::Managed(Box::new(layout)))
    }
}

impl Layout {
    fn valid(&self) -> bool {
        if self.schema_version != 1
            || self.managed_by != "gardener"
            || self.contract != "@Schema/layout.yaml"
            || !self.generated_at.ends_with('Z')
            || !matches!(self.generated_at.as_bytes().get(10), Some(b'T' | b't'))
            || chrono::DateTime::parse_from_rfc3339(&self.generated_at).is_err()
            || !self.roots.all().into_iter().all(relative)
        {
            return false;
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        self.domains.iter().all(|domain| {
            domain.id.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            }) && !domain.name.trim().is_empty()
                && relative(&domain.path)
                && suffix(&domain.path, &self.roots.state).is_some_and(relative)
                && ids.insert(&domain.id)
                && paths.insert(&domain.path)
        })
    }

    pub fn context<'a>(&'a self, path: &'a str) -> Option<ContextPath<'a>> {
        for domain in &self.domains {
            if let Some(relative) = suffix(path, &domain.path) {
                let (name, rest) = relative.split_once('/').unwrap_or((relative, ""));
                let name = if rest.is_empty() {
                    name.strip_suffix(".md").unwrap_or(name)
                } else {
                    name
                };
                if !name.is_empty() && !name.starts_with('@') {
                    return Some(ContextPath {
                        id: ContextId {
                            domain: &domain.id,
                            name,
                        },
                        layer: ContextLayer::State,
                        kind: None,
                    });
                }
            }
        }
        let relative = suffix(path, &self.roots.log)?;
        let (domain, relative) = relative.split_once('/')?;
        let id = self.domains.iter().find(|entry| entry.id == domain)?;
        let (name, rest) = relative.split_once('/').unwrap_or((relative, ""));
        if name.is_empty() {
            return None;
        }
        Some(ContextPath {
            id: ContextId {
                domain: &id.id,
                name,
            },
            layer: ContextLayer::Log,
            kind: rest.split('/').next().filter(|kind| !kind.is_empty()),
        })
    }

    pub fn area_for_path(&self, path: &str) -> Option<&str> {
        let mut matches = self.domains.iter().filter(|domain| {
            within(path, &domain.path)
                || suffix(path, &self.roots.log).and_then(|path| path.split('/').next())
                    == Some(domain.id.as_str())
        });
        let domain = matches.next()?;
        matches.next().is_none().then_some(domain.name.as_str())
    }
}

impl Roots {
    fn all(&self) -> [&str; 6] {
        [
            &self.inbox,
            &self.sources,
            &self.knowledge,
            &self.state,
            &self.log,
            &self.views,
        ]
    }
}

pub(crate) fn within(path: &str, folder: &str) -> bool {
    path == folder || suffix(path, folder).is_some()
}

pub(crate) fn suffix<'a>(path: &'a str, folder: &str) -> Option<&'a str> {
    path.strip_prefix(folder)?.strip_prefix('/')
}

fn relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':', '{', '}'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| {
            !part.is_empty()
                && !matches!(part, "." | "..")
                && matches!(
                    Path::new(part).components().next(),
                    Some(Component::Normal(_))
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vd4_review_malformed_nested_entries_preserve_the_layout_and_valid_roots() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(DIRECTORY)).unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lifecycle-vault");
        let mut descriptor = read(&fixture).unwrap();
        let hash = "0".repeat(64);
        let valid = serde_json::json!({"path":"Vault","kind":"vault","tree_sha256":hash,"files":1,"name":""});
        descriptor["layout"]["nested_roots"] = serde_json::json!([
            {"path":"Unknown","kind":"submodule","tree_sha256":hash,"files":3},
            {"path":"../bad","kind":"unit","tree_sha256":hash,"files":0},
            {"path":"Wrong hash","kind":"vault","tree_sha256":"bad","files":3},
            valid,
            {"path":"Vault","kind":"unit","tree_sha256":hash,"files":2},
            {"path":"Unit","kind":"unit","tree_sha256":hash,"files":0}
        ]);
        std::fs::write(path(root.path()), serde_json::to_vec(&descriptor).unwrap()).unwrap();
        let LayoutState::Managed(layout) = LayoutState::load(root.path()) else {
            panic!("Malformed nested entries must not disable the layout");
        };
        assert_eq!(layout.roots.state, "state");
        assert_eq!(layout.nested_roots.len(), 2);
        assert_eq!(layout.nested_roots[0].name.as_deref(), Some(""));
        assert_eq!(layout.nested_roots[1].kind, NestedKind::Unit);

        descriptor["layout"]["nested_roots"] = serde_json::json!({"wrong":"shape"});
        std::fs::write(path(root.path()), serde_json::to_vec(&descriptor).unwrap()).unwrap();
        let LayoutState::Managed(layout) = LayoutState::load(root.path()) else {
            panic!("A malformed optional field must not disable the layout");
        };
        assert!(layout.nested_roots.is_empty());
    }

    #[test]
    fn migration_review_layout_requires_schema_fields() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(DIRECTORY)).unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lifecycle-vault");
        let valid = read(&fixture).unwrap();
        for (pointer, invalid) in [
            ("/layout/domains", None),
            ("/layout/generated_at", None),
            ("/layout/contract", Some(serde_json::json!("layout.yaml"))),
            (
                "/layout/generated_at",
                Some(serde_json::json!("2026-02-31T08:00:00Z")),
            ),
            (
                "/layout/generated_at",
                Some(serde_json::json!("2026-01-01T08:00:00+00:00")),
            ),
            (
                "/layout/generated_at",
                Some(serde_json::json!("2026-01-01 08:00:00Z")),
            ),
            ("/layout/domains/0/id", Some(serde_json::json!("Dev--team"))),
        ] {
            let mut descriptor = valid.clone();
            if let Some(invalid) = invalid {
                *descriptor.pointer_mut(pointer).unwrap() = invalid;
            } else {
                let key = pointer.rsplit('/').next().unwrap();
                descriptor["layout"].as_object_mut().unwrap().remove(key);
            }
            std::fs::write(path(root.path()), serde_json::to_vec(&descriptor).unwrap()).unwrap();
            assert!(
                matches!(LayoutState::load(root.path()), LayoutState::Unsupported),
                "{pointer}"
            );
        }
        assert!(matches!(
            LayoutState::load(&fixture),
            LayoutState::Managed(_)
        ));
    }

    #[test]
    fn layout_is_independent_of_name_and_unknown_fields() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(DIRECTORY)).unwrap();
        let path = path(root.path());
        std::fs::write(&path, r#"{"name":"Old","other":true}"#).unwrap();
        assert!(matches!(
            LayoutState::load(root.path()),
            LayoutState::Legacy
        ));

        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lifecycle-vault");
        let mut descriptor = read(&fixture).unwrap();
        descriptor["name"] = Value::Bool(false);
        descriptor["layout"]["extension"] = Value::Bool(true);
        let bytes = serde_json::to_vec(&descriptor).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let LayoutState::Managed(layout) = LayoutState::load(root.path()) else {
            panic!("expected a supported layout");
        };
        assert_eq!(
            layout.area_for_path("state/health/sleep/Plan.md"),
            Some("Health")
        );
        assert_eq!(
            layout.area_for_path("log/health/sleep/reports/Plan.md"),
            Some("Health")
        );
        assert_eq!(layout.area_for_path("state/healthier/sleep/Plan.md"), None);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        for roots in [serde_json::json!("../notes"), serde_json::json!(false)] {
            descriptor["layout"]["roots"]["state"] = roots;
            std::fs::write(&path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
            assert!(matches!(
                LayoutState::load(root.path()),
                LayoutState::Unsupported
            ));
        }
    }
}
