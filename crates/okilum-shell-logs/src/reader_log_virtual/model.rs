//! Desktop configuration over the canonical versioned .okilog definition.
use anyhow::{ensure, Context as _, Result};
use okilum_core::log::virtual_log::{Definition, TimeRange};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TimeMode {
    All,
    Last,
    Absolute,
}
impl TimeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All time",
            Self::Last => "Last seconds",
            Self::Absolute => "Between timestamps",
        }
    }
}
pub(super) struct Loaded {
    pub path: PathBuf,
    pub definition: Definition,
    pub source: String,
    pub source_path: PathBuf,
    pub session: Option<super::save::Session>,
    pub save_error: Option<String>,
}
pub(super) fn load(vault: &Path, path: &Path) -> Result<Loaded> {
    let source_path = if path.is_absolute() {
        path.to_owned()
    } else {
        vault.join(path)
    };
    let path = source_path.canonicalize()?;
    ensure!(
        path.starts_with(vault.canonicalize()?),
        "Definition is outside this vault"
    );
    ensure!(
        path.extension().is_some_and(|ext| ext == "okilog"),
        "Choose a .okilog definition"
    );
    let mut source = String::new();
    std::fs::File::open(&path)?
        .take(64 * 1024 + 1)
        .read_to_string(&mut source)?;
    let definition = Definition::parse(&source)?;
    Ok(Loaded {
        path,
        definition,
        source,
        source_path,
        session: None,
        save_error: None,
    })
}

pub(super) struct Fields {
    pub name: String,
    pub glob: String,
    pub query: String,
    pub rotation: String,
    pub vault_root: bool,
    pub time: TimeMode,
    pub seconds: String,
    pub from: String,
    pub to: String,
}
impl Fields {
    pub fn from_definition(definition: &Definition) -> Self {
        let (time, seconds, from, to) = match definition.time {
            None => (TimeMode::All, "3600".into(), String::new(), String::new()),
            Some(TimeRange::LastSeconds(seconds)) => (
                TimeMode::Last,
                seconds.to_string(),
                String::new(),
                String::new(),
            ),
            Some(TimeRange::Absolute { start, end }) => {
                let format = |at| {
                    chrono::DateTime::from_timestamp_nanos(at)
                        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
                };
                (
                    TimeMode::Absolute,
                    "3600".into(),
                    format(start),
                    format(end),
                )
            }
        };
        Self {
            name: definition.name.clone(),
            glob: definition.glob.clone(),
            query: definition.query.clone().unwrap_or_default(),
            rotation: definition.rotation.clone().unwrap_or_default(),
            vault_root: definition.vault_root,
            time,
            seconds,
            from,
            to,
        }
    }
    pub fn definition(&self) -> Result<Definition> {
        let timestamp = |text: &str| -> Result<i64> {
            chrono::DateTime::parse_from_rfc3339(text.trim())
                .context("Use an RFC3339 timestamp with timezone")?
                .timestamp_nanos_opt()
                .context("Timestamp is outside the supported range")
        };
        let time = match self.time {
            TimeMode::All => None,
            TimeMode::Last => {
                let seconds = self
                    .seconds
                    .trim()
                    .parse::<u32>()
                    .context("Enter a positive number of seconds")?;
                ensure!(seconds > 0, "Enter a positive number of seconds");
                Some(TimeRange::LastSeconds(seconds))
            }
            TimeMode::Absolute => {
                let start = timestamp(&self.from)?;
                let end = timestamp(&self.to)?;
                ensure!(
                    start <= end,
                    "Start timestamp must precede the end timestamp"
                );
                Some(TimeRange::Absolute { start, end })
            }
        };
        let optional = |value: &str| (!value.trim().is_empty()).then(|| value.to_owned());
        let definition = Definition {
            version: 1,
            name: self.name.clone(),
            glob: self.glob.clone(),
            vault_root: self.vault_root,
            query: optional(&self.query),
            rotation: optional(&self.rotation),
            time,
        };
        Definition::parse(&serde_json::to_string(&definition)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn configuration_roundtrips_supported_time_ranges_without_precision_loss() {
        for time in [
            None,
            Some(TimeRange::LastSeconds(3600)),
            Some(TimeRange::Absolute {
                start: 1792238400000000123,
                end: 1792242000000000456,
            }),
        ] {
            let definition = Definition {
                version: 1,
                name: "Checkout".into(),
                vault_root: true,
                glob: "logs/**".into(),
                rotation: Some("app.log".into()),
                query: Some("level>=warn".into()),
                time,
            };
            let fields = Fields::from_definition(&definition);
            assert_eq!(
                serde_json::to_value(fields.definition().unwrap()).unwrap(),
                serde_json::to_value(definition).unwrap()
            );
        }
        let d = Definition::parse(
            r#"{"version":1,"name":"x","glob":"logs/*","time":{"last_seconds":1}}"#,
        )
        .unwrap();
        let mut fields = Fields::from_definition(&d);
        fields.seconds = "0".into();
        assert!(fields.definition().is_err());
    }
    #[test]
    fn definition_load_is_bounded_and_vault_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let source = r#"{"version":1,"name":"x","glob":"logs/*"}"#;
        std::fs::write(vault.join("view.okilog"), source).unwrap();
        assert_eq!(
            load(&vault, Path::new("view.okilog"))
                .unwrap()
                .definition
                .name,
            "x"
        );
        std::fs::write(dir.path().join("outside.okilog"), source).unwrap();
        assert!(load(&vault, &dir.path().join("outside.okilog")).is_err());
        std::fs::write(vault.join("large.okilog"), " ".repeat(64 * 1024 + 1)).unwrap();
        assert!(load(&vault, Path::new("large.okilog")).is_err());
        assert_eq!(
            std::fs::read_to_string(vault.join("view.okilog")).unwrap(),
            source,
            "loading never rewrites canonical configuration"
        );
    }

    #[test]
    fn configuration_preserves_literal_glob_and_rotation_whitespace() {
        let definition = Definition::parse(r#"{"version":1,"name":"Named view ","glob":" logs/**","rotation":" app.log","query":" level>=warn "}"#).unwrap();
        let next = Fields::from_definition(&definition).definition().unwrap();
        assert_eq!(
            serde_json::to_value(next).unwrap(),
            serde_json::to_value(definition).unwrap()
        );
    }
}
