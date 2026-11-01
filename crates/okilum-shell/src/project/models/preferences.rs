//! Local model-picker preferences. These are user choices, separate from the
//! live catalog and the thread's canonical server-confirmed configuration.
use anyhow::{ensure, Context as _, Result};
use okilum_t3::{Environment, ModelOption, ModelSelection};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    #[serde(default, skip_serializing_if = "is_false")]
    favorite: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    options: Option<Vec<ModelOption>>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

type Entries = BTreeMap<String, BTreeMap<String, Entry>>;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    origin: String,
    #[serde(default)]
    instances: Entries,
}

#[derive(Clone, Debug, Default)]
struct Change {
    favorite: Option<bool>,
    options: Option<Vec<ModelOption>>,
}

/// Load once when the connection scope is known. Reading favorites or opening
/// the picker never reads disk or the network. Only explicit local edits save.
#[derive(Clone, Debug)]
pub(super) struct Prefs {
    origin: String,
    path: PathBuf,
    instances: Entries,
    changes: BTreeMap<(String, String), Change>,
}

fn normalized_origin(origin: &str) -> Result<String> {
    Ok(Environment::new("model-preferences", origin)?.origin())
}

fn validate_options(options: &[ModelOption]) -> Result<()> {
    let mut ids = BTreeSet::new();
    ensure!(
        options
            .iter()
            .all(|option| !option.id.trim().is_empty() && ids.insert(&option.id)),
        "Model preferences contain empty or duplicate option identifiers"
    );
    Ok(())
}

fn read(path: &Path, origin: &str) -> Result<Entries> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Entries::new()),
        Err(error) => return Err(error).context("Could not read model preferences"),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let document: Document = serde_json::from_slice(&bytes).context("Invalid model preferences")?;
    ensure!(
        document.version == VERSION,
        "Unsupported model preferences version"
    );
    ensure!(
        document.origin == origin,
        "Model preferences belong to another environment"
    );
    for (instance, models) in &document.instances {
        ensure!(
            !instance.trim().is_empty(),
            "Empty model preference instance"
        );
        for (model, entry) in models {
            ensure!(
                !model.trim().is_empty(),
                "Empty model preference identifier"
            );
            if let Some(options) = &entry.options {
                validate_options(options)?;
            }
        }
    }
    Ok(document.instances)
}

fn save_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl Prefs {
    pub(super) fn load(state_directory: &Path, origin: &str) -> Result<Self> {
        let origin = normalized_origin(origin)?;
        // Instance/model strings never enter a path; identical slugs in other
        // accounts or environments are distinct entries rather than aliases.
        let digest = format!("{:x}", Sha256::digest(origin.as_bytes()));
        let path = state_directory
            .join("manager-model-preferences")
            .join(format!("{digest}.json"));
        let instances = read(&path, &origin)?;
        Ok(Self {
            origin,
            path,
            instances,
            changes: BTreeMap::new(),
        })
    }

    pub(super) fn origin(&self) -> &str {
        &self.origin
    }

    pub(super) fn has_favorites(&self) -> bool {
        self.instances
            .values()
            .any(|models| models.values().any(|entry| entry.favorite))
    }

    pub(super) fn is_favorite(&self, instance: &str, model: &str) -> bool {
        self.instances
            .get(instance)
            .and_then(|models| models.get(model))
            .is_some_and(|entry| entry.favorite)
    }

    /// Returns the new local star state. This does not choose a model, alter
    /// options, close the picker or send a server command.
    pub(super) fn toggle_favorite(&mut self, instance: &str, model: &str) -> bool {
        if instance.trim().is_empty() || model.trim().is_empty() {
            return false;
        }
        let entry = self
            .instances
            .entry(instance.into())
            .or_default()
            .entry(model.into())
            .or_default();
        entry.favorite = !entry.favorite;
        self.changes
            .entry((instance.into(), model.into()))
            .or_default()
            .favorite = Some(entry.favorite);
        entry.favorite
    }

    /// Explicit model selection may recall that exact model's prior options.
    /// Never call this to replace a live subscription's current configuration.
    /// The caller validates the recalled values against the current descriptors
    /// before mutation: preferences do not grant support for an old option.
    pub(super) fn remembered(&self, selection: &ModelSelection) -> Option<ModelSelection> {
        let options = self
            .instances
            .get(&selection.instance_id)?
            .get(&selection.model)?
            .options
            .clone()?;
        Some(ModelSelection {
            instance_id: selection.instance_id.clone(),
            model: selection.model.clone(),
            options,
        })
    }

    /// Record only canonical/explicit selections, never a hover or unconfirmed
    /// candidate. Empty options are meaningful and remain distinct from absent.
    pub(super) fn remember(&mut self, selection: &ModelSelection) -> Result<()> {
        ensure!(
            !selection.instance_id.trim().is_empty() && !selection.model.trim().is_empty(),
            "Missing model preference target"
        );
        validate_options(&selection.options)?;
        let entry = self
            .instances
            .entry(selection.instance_id.clone())
            .or_default()
            .entry(selection.model.clone())
            .or_default();
        if entry.options.as_ref() != Some(&selection.options) {
            entry.options = Some(selection.options.clone());
            self.changes
                .entry((selection.instance_id.clone(), selection.model.clone()))
                .or_default()
                .options = Some(selection.options.clone());
        }
        Ok(())
    }

    /// Re-read and merge only touched fields so independent panels cannot
    /// discard another model's new favorite or remembered options. Atomic
    /// replacement preserves the previous file if serialization/writing fails.
    pub(super) fn save(&mut self) -> Result<()> {
        if self.changes.is_empty() {
            return Ok(());
        }
        let _lock = save_lock()
            .lock()
            .map_err(|_| anyhow::anyhow!("Model preference writer is unavailable"))?;
        let mut latest = read(&self.path, &self.origin)?;
        for ((instance, model), change) in &self.changes {
            let entry = latest
                .entry(instance.clone())
                .or_default()
                .entry(model.clone())
                .or_default();
            if let Some(favorite) = change.favorite {
                entry.favorite = favorite;
            }
            if let Some(options) = &change.options {
                entry.options = Some(options.clone());
            }
        }
        latest.retain(|_, models| {
            models.retain(|_, entry| entry.favorite || entry.options.is_some());
            !models.is_empty()
        });
        let document = Document {
            version: VERSION,
            origin: self.origin.clone(),
            instances: latest.clone(),
        };
        let bytes = serde_json::to_vec(&document)?;
        let directory = self.path.parent().expect("preference path has a directory");
        std::fs::create_dir_all(directory)?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path)?;
        #[cfg(unix)]
        File::open(directory)?.sync_all()?;
        self.instances = latest;
        self.changes.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use serde_json::json;

    fn selection(instance: &str, model: &str, effort: &str) -> ModelSelection {
        ModelSelection {
            instance_id: instance.into(),
            model: model.into(),
            options: vec![ModelOption {
                id: "effort".into(),
                value: json!(effort),
            }],
        }
    }

    #[test]
    fn favorites_restore_with_normalized_origin_and_exact_instance_model_identity() {
        let state = tempfile::tempdir().unwrap();
        let mut prefs = Prefs::load(state.path(), "https://T3.EXAMPLE:443/").unwrap();
        assert_eq!(prefs.origin(), "https://t3.example");
        assert!(!prefs.has_favorites());
        assert!(prefs.toggle_favorite("work-account", "shared-model"));
        prefs.save().unwrap();
        let mut restored = Prefs::load(state.path(), "https://t3.example").unwrap();
        assert!(restored.has_favorites());
        assert!(restored.is_favorite("work-account", "shared-model"));
        assert!(!restored.is_favorite("personal-account", "shared-model"));
        assert!(!restored.is_favorite("work-account", "different-model"));
        assert!(!Prefs::load(state.path(), "https://other.example")
            .unwrap()
            .has_favorites());
        assert!(!restored.toggle_favorite("work-account", "shared-model"));
        restored.save().unwrap();
        assert!(!Prefs::load(state.path(), "https://t3.example")
            .unwrap()
            .has_favorites());
    }

    #[test]
    fn remembered_options_are_scoped_and_do_not_modify_the_callers_selection() {
        let state = tempfile::tempdir().unwrap();
        let mut prefs = Prefs::load(state.path(), "https://t3.example").unwrap();
        let a = selection("work", "model-a", "high");
        prefs.remember(&a).unwrap();
        prefs.save().unwrap();
        let prefs = Prefs::load(state.path(), "https://t3.example").unwrap();
        let current = selection("work", "model-a", "low");
        assert_eq!(prefs.remembered(&current), Some(a));
        assert_eq!(
            current.options[0].value, "low",
            "recall is an explicit new selection, never canonical state mutation"
        );
        assert!(prefs
            .remembered(&selection("personal", "model-a", "low"))
            .is_none());
        assert!(prefs
            .remembered(&selection("work", "model-b", "low"))
            .is_none());
        assert!(Prefs::load(state.path(), "https://other.example")
            .unwrap()
            .remembered(&current)
            .is_none());
    }

    #[test]
    fn independent_panels_merge_favorites_and_options_without_losing_other_edits() {
        let state = tempfile::tempdir().unwrap();
        let mut left = Prefs::load(state.path(), "https://t3.example").unwrap();
        let mut right = Prefs::load(state.path(), "https://t3.example").unwrap();
        left.toggle_favorite("work", "model-a");
        right.toggle_favorite("personal", "model-b");
        right
            .remember(&selection("work", "model-a", "high"))
            .unwrap();
        left.save().unwrap();
        right.save().unwrap();
        left.toggle_favorite("work", "model-a");
        left.save().unwrap();
        let restored = Prefs::load(state.path(), "https://t3.example").unwrap();
        assert!(!restored.is_favorite("work", "model-a"));
        assert!(restored.is_favorite("personal", "model-b"));
        assert_eq!(
            restored
                .remembered(&selection("work", "model-a", "low"))
                .unwrap()
                .options[0]
                .value,
            "high"
        );
    }

    #[test]
    fn empty_options_are_remembered_and_invalid_values_do_not_replace_prior_data() {
        let state = tempfile::tempdir().unwrap();
        let mut prefs = Prefs::load(state.path(), "https://t3.example").unwrap();
        let mut empty = selection("work", "model-a", "high");
        empty.options.clear();
        prefs.remember(&empty).unwrap();
        prefs.save().unwrap();
        assert_eq!(
            Prefs::load(state.path(), "https://t3.example")
                .unwrap()
                .remembered(&empty),
            Some(empty.clone())
        );
        let mut bad = empty;
        bad.options = vec![
            ModelOption {
                id: "effort".into(),
                value: json!("high")
            };
            2
        ];
        assert!(prefs.remember(&bad).is_err());
        assert!(prefs.remembered(&bad).unwrap().options.is_empty());
    }

    #[test]
    fn malformed_or_foreign_document_is_not_silently_overwritten() {
        let state = tempfile::tempdir().unwrap();
        let mut prefs = Prefs::load(state.path(), "https://t3.example").unwrap();
        prefs.toggle_favorite("work", "model-a");
        prefs.save().unwrap();
        let foreign = br#"{"version":1,"origin":"https://other.example","instances":{}}"#;
        std::fs::write(&prefs.path, foreign).unwrap();
        prefs.toggle_favorite("work", "model-b");
        assert!(prefs.save().is_err());
        assert_eq!(std::fs::read(&prefs.path).unwrap().as_slice(), foreign);
        assert!(Prefs::load(state.path(), "https://t3.example").is_err());
        std::fs::write(&prefs.path, b"not json").unwrap();
        assert!(Prefs::load(state.path(), "https://t3.example").is_err());
    }
}
