//! Read-only access to the vault's self-description.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

pub const DIRECTORY: &str = ".okilum";
pub const FILE: &str = "vault.json";

pub fn path(root: &Path) -> PathBuf {
    root.join(DIRECTORY).join(FILE)
}

pub fn read(root: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path(root)).ok()?).ok()
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Layout {
    pub schema_version: u32,
    pub managed_by: String,
    pub contract: String,
    pub roots: Roots,
    #[serde(default)]
    pub domains: Vec<Domain>,
    #[serde(default)]
    pub generated_at: Option<String>,
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
    Managed(Layout),
    Unsupported,
}

impl LayoutState {
    pub fn load(root: &Path) -> Self {
        let Some(mut descriptor) = read(root) else {
            return Self::Legacy;
        };
        let Some(value) = descriptor
            .as_object_mut()
            .and_then(|object| object.remove("layout"))
        else {
            return Self::Legacy;
        };
        serde_json::from_value::<Layout>(value)
            .ok()
            .filter(Layout::valid)
            .map_or(Self::Unsupported, Self::Managed)
    }
}

impl Layout {
    fn valid(&self) -> bool {
        if self.schema_version != 1
            || self.managed_by != "gardener"
            || !self.roots.all().into_iter().all(relative)
        {
            return false;
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        self.domains.iter().all(|domain| {
            !domain.id.trim().is_empty()
                && !domain.id.contains('/')
                && !domain.name.trim().is_empty()
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
