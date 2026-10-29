//! Area manifests prefer TOML; absence alone permits the legacy YAML fallback.
//! Existing files keep their format. No reader converts or removes a vault file.
use anyhow::{bail, ensure, Context, Result};
use serde_yaml::Value;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub(crate) const MAX_BYTES: u64 = 512 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Toml,
    Yaml,
}
#[derive(Clone, Debug)]
pub struct Document {
    pub path: PathBuf,
    pub format: Format,
    pub source: String,
    pub value: Value,
}

pub fn preferred_path(area: &Path, name: &str) -> Result<Option<PathBuf>> {
    ensure!(matches!(name, "project" | "board"), "Unknown Area manifest");
    match std::fs::symlink_metadata(area.join("ops")) {
        Ok(meta) => ensure!(meta.is_dir(), "Area ops must be a directory, not a symlink"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("Cannot read Area ops"),
    }
    for extension in ["toml", "yaml"] {
        let path = area.join("ops").join(format!("{name}.{extension}"));
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => {
                ensure!(
                    meta.is_file(),
                    "{} must be a regular file, not a symlink",
                    path.display()
                );
                return Ok(Some(path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("Cannot read {}", path.display())),
        }
    }
    Ok(None)
}
pub fn read(area: &Path, name: &str) -> Result<Option<Document>> {
    let Some(path) = preferred_path(area, name)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    std::fs::File::open(&path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "{} exceeds 512 KiB; not truncated",
        path.display()
    );
    let source = String::from_utf8(bytes).context("Area manifest is not UTF-8")?;
    let format = if path.extension().is_some_and(|e| e == "toml") {
        Format::Toml
    } else {
        Format::Yaml
    };
    let value = parse(&source, format).with_context(|| path.display().to_string())?;
    Ok(Some(Document {
        path,
        format,
        source,
        value,
    }))
}
pub fn parse(source: &str, format: Format) -> Result<Value> {
    let mut value = match format {
        Format::Yaml => serde_yaml::from_str(source).context("Invalid YAML")?,
        Format::Toml => from_toml(toml::from_str(source).context("Invalid TOML")?),
    };
    ensure!(value.is_mapping(), "Area manifest must be a table/mapping");
    // TOML represents an unbound manager by omitted nullable keys. Legacy YAML
    // retains its stricter explicit-null contract.
    if format == Format::Toml {
        if let Some(manager) = value
            .get_mut("t3")
            .and_then(|v| v.get_mut("manager"))
            .and_then(Value::as_mapping_mut)
        {
            for key in ["project_id", "thread_id"] {
                manager.entry(Value::from(key)).or_insert(Value::Null);
            }
        }
    }
    Ok(value)
}
fn from_toml(value: toml::Value) -> Value {
    match value {
        toml::Value::String(v) => Value::String(v),
        toml::Value::Integer(v) => Value::Number(v.into()),
        toml::Value::Float(v) => serde_yaml::to_value(v).expect("TOML float"),
        toml::Value::Boolean(v) => Value::Bool(v),
        toml::Value::Datetime(v) => Value::String(v.to_string()),
        toml::Value::Array(v) => Value::Sequence(v.into_iter().map(from_toml).collect()),
        toml::Value::Table(v) => Value::Mapping(
            v.into_iter()
                .map(|(k, v)| (Value::String(k), from_toml(v)))
                .collect(),
        ),
    }
}
fn omit_nulls(value: &Value) -> Result<Value> {
    Ok(match value {
        Value::Mapping(map) => Value::Mapping(
            map.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| Ok((k.clone(), omit_nulls(v)?)))
                .collect::<Result<_>>()?,
        ),
        Value::Sequence(seq) => {
            ensure!(
                !seq.iter().any(Value::is_null),
                "TOML arrays cannot contain null"
            );
            Value::Sequence(seq.iter().map(omit_nulls).collect::<Result<_>>()?)
        }
        Value::Null => bail!("TOML has no null scalar"),
        value => value.clone(),
    })
}
impl Document {
    /// Edit one TOML root key, retaining unrelated content and comments.
    pub fn set_toml_field(&self, key: &str, value: Option<&Value>) -> Result<String> {
        ensure!(self.format == Format::Toml, "Expected a TOML document");
        let mut document = self.source.parse::<toml_edit::DocumentMut>()?;
        match value {
            Some(value) => {
                let mut wrapper = serde_yaml::Mapping::new();
                wrapper.insert(Value::from("field"), omit_nulls(value)?);
                let serialized = toml::to_string(&wrapper)?;
                let mut item = serialized.parse::<toml_edit::DocumentMut>()?;
                let mut replacement = item.remove("field").context("Missing serialized field")?;
                if let Some(original) = document.get(key) {
                    if let (Some(old), Some(new)) =
                        (original.as_value(), replacement.as_value_mut())
                    {
                        *new.decor_mut() = old.decor().clone();
                    }
                    if let (Some(old), Some(new)) =
                        (original.as_table(), replacement.as_table_mut())
                    {
                        *new.decor_mut() = old.decor().clone();
                    }
                }
                if let Some(original_key) = document.as_table().key(key).cloned() {
                    document
                        .as_table_mut()
                        .insert_formatted(&original_key, replacement);
                } else {
                    document.insert(key, replacement);
                }
            }
            None => {
                document.remove(key);
            }
        }
        let updated = document.to_string();
        let before = self.value.as_mapping().context("Expected a table")?;
        let after = parse(&updated, Format::Toml)?;
        let mut unchanged = before.clone();
        unchanged.remove(Value::from(key));
        let mut actual = after.as_mapping().context("Expected edited table")?.clone();
        actual.remove(Value::from(key));
        ensure!(
            unchanged == actual,
            "Editing {key} would change unrelated manifest fields"
        );
        Ok(updated)
    }
    /// Optimistic revision check and durable same-directory atomic replacement.
    pub fn replace(&self, updated: &str) -> Result<()> {
        if updated == self.source {
            return Ok(());
        }
        let ops = self.path.parent().context("Manifest has no parent")?;
        let area = ops.parent().context("Manifest has no Area")?;
        let name = self
            .path
            .file_stem()
            .and_then(|v| v.to_str())
            .context("Invalid manifest name")?;
        let temporary = ops.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            ensure!(
                preferred_path(area, name)?.as_ref() == Some(&self.path),
                "Manifest format changed; reload before saving"
            );
            let permissions = std::fs::metadata(&self.path)?.permissions();
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(updated.as_bytes())?;
            file.set_permissions(permissions)?;
            file.sync_all()?;
            drop(file);
            ensure!(
                std::fs::read(&self.path)? == self.source.as_bytes(),
                "Manifest changed; reload before saving"
            );
            ensure!(
                preferred_path(area, name)?.as_ref() == Some(&self.path),
                "Manifest format changed; reload before saving"
            );
            std::fs::rename(&temporary, &self.path)?;
            #[cfg(unix)]
            std::fs::File::open(ops)?.sync_all()?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&temporary);
        result
    }
}

#[cfg(test)]
mod tests;
