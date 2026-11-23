//! Compiled definitions from the vault's field contract and its references.
use super::{is_slug, layout::Layout};
use anyhow::{bail, ensure, Context, Result};
use chrono_tz::Tz;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::Deserialize;
use serde_yaml::Value as Yaml;
use std::{collections::BTreeMap, fs, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldType {
    String,
    Number,
    Bool,
    Date,
    DateTime,
    Enum,
    Link,
    List(Box<FieldType>),
}

impl FieldType {
    fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "string" => Self::String,
            "number" => Self::Number,
            "bool" => Self::Bool,
            "date" => Self::Date,
            "datetime" => Self::DateTime,
            "enum" => Self::Enum,
            "link" => Self::Link,
            name if name.starts_with("list<") && name.ends_with('>') => {
                Self::List(Box::new(Self::parse(&name[5..name.len() - 1])?))
            }
            _ => bail!("Unknown field type {name:?}"),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Field {
    pub field_type: FieldType,
    pub writable_by_actions: bool,
    pub closed: bool,
    pub values: Vec<String>,
    pub values_by_layer: BTreeMap<String, Vec<String>>,
    pub(super) required: Required,
    pub(super) legacy_keys: Vec<String>,
    pub(super) legacy_repo_keys: Vec<String>,
    pub(super) aliases: BTreeMap<String, String>,
    pub(super) stored_aliases: BTreeMap<String, String>,
    pub(super) pattern: Option<Regex>,
    pub(super) slug: bool,
    pub(crate) allow_http_urls: bool,
}

impl Field {
    pub fn valid_string(&self, value: &str) -> bool {
        (!self.slug || is_slug(value)) && self.pattern.as_ref().is_none_or(|p| p.is_match(value))
    }

    pub(crate) fn enum_value(
        &self,
        value: &str,
        layer: Option<&str>,
        stored: bool,
    ) -> Option<String> {
        let lower = value.to_lowercase();
        let canonical = self
            .aliases
            .get(&lower)
            .map(String::as_str)
            .unwrap_or(&lower);
        let canonical = if stored {
            self.stored_aliases
                .get(canonical)
                .or_else(|| {
                    let head = [" (", " —", ";", ","]
                        .iter()
                        .filter_map(|delimiter| canonical.find(delimiter))
                        .min()?;
                    self.stored_aliases.get(canonical[..head].trim_end())
                })
                .map(String::as_str)
                .unwrap_or(canonical)
        } else {
            canonical
        };
        let options = layer.and_then(|l| self.values_by_layer.get(l));
        options
            .into_iter()
            .flatten()
            .chain(&self.values)
            .chain(
                self.values_by_layer
                    .values()
                    .flatten()
                    .filter(|_| layer.is_none()),
            )
            .find(|s| s.to_lowercase() == canonical)
            .cloned()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Required {
    pub layers: Vec<String>,
    pub types: Vec<String>,
    types_requiring_from: Option<String>,
    pub except_layers: Vec<String>,
    pub except_kinds: Vec<String>,
    pub except_domain_root_state: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawField {
    #[serde(rename = "type")]
    kind: String,
    closed: bool,
    values: Vec<Yaml>,
    values_from: Yaml,
    same_values_as: Option<String>,
    values_by_layer: BTreeMap<String, Yaml>,
    aliases_from: Option<String>,
    legacy: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    pattern: Option<String>,
    pattern_from: Option<String>,
    format: Option<String>,
    allow_http_urls: bool,
    writable_by_actions: bool,
    legacy_keys: Vec<String>,
    legacy_repo_keys: Vec<String>,
    required: Required,
}

#[derive(Deserialize)]
struct Contract {
    version: String,
    timezone: String,
    scope: Scope,
    path_projection: PathProjection,
    fields: BTreeMap<String, RawField>,
    builtins: BTreeMap<String, RawField>,
    task_fields: BTreeMap<String, RawField>,
    section_fields: BTreeMap<String, RawField>,
    #[serde(default)]
    jsonl_fields: BTreeMap<String, BTreeMap<String, RawField>>,
}

#[derive(Deserialize)]
struct Scope {
    include: String,
    exclude: Vec<String>,
}

#[derive(Deserialize)]
struct PathProjection {
    templates_from: String,
}

#[derive(Clone, Debug)]
pub struct FieldSchema {
    pub timezone: Tz,
    pub fields: BTreeMap<String, Field>,
    pub builtins: BTreeMap<String, Field>,
    pub task_fields: BTreeMap<String, Field>,
    pub section_fields: BTreeMap<String, Field>,
    pub jsonl_fields: BTreeMap<String, BTreeMap<String, Field>>,
    include: GlobSet,
    exclude: GlobSet,
    pub(super) layout: Layout,
}

impl FieldSchema {
    pub const FILES: [&str; 4] = [
        "@Schema/fields.yaml",
        "@Schema/layout.yaml",
        "@Schema/frontmatter-schema.yaml",
        "@Schema/tags.yaml",
    ];

    pub fn load(vault_root: &Path) -> Result<Self> {
        let directory = vault_root.join("@Schema");
        ensure!(
            !fs::symlink_metadata(&directory)?.is_symlink(),
            "@Schema is a symlink"
        );
        Self::from_documents(|name| {
            let path = directory.join(name);
            ensure!(
                fs::symlink_metadata(&path)?.is_file(),
                "Expected a regular schema file"
            );
            Ok(fs::read_to_string(path)?)
        })
    }

    /// Load from canonical contract documents supplied by a vault reader.
    /// References are confined to filenames in the same schema directory.
    pub fn from_documents(mut read: impl FnMut(&str) -> Result<String>) -> Result<Self> {
        let schema: Contract =
            serde_yaml::from_str(&read("fields.yaml")?).context("fields.yaml")?;
        ensure!(
            matches!(schema.version.as_str(), "1-draft" | "1"),
            "Unsupported fields.yaml version {:?}",
            schema.version
        );
        let timezone = schema.timezone.parse().context("fields.yaml timezone")?;
        let mut refs = References {
            read,
            docs: BTreeMap::new(),
        };
        let templates = refs.resolve(&schema.path_projection.templates_from)?;
        let layout = Layout::compile(&templates, &refs.resolve("layout.yaml#domains")?)?;
        let include = globset(std::iter::once(schema.scope.include))?;
        let ignore = strings(&refs.resolve("frontmatter-schema.yaml#ignore_globs")?)?;
        let exclude = globset(
            schema
                .scope
                .exclude
                .into_iter()
                .chain(ignore)
                .chain(["@views/out/**".into(), "@Schema/**".into()]),
        )?;
        let patterns = schema
            .fields
            .iter()
            .filter_map(|(key, field)| Some((key.clone(), field.pattern.clone()?)))
            .collect();
        let mut compile = |fields: BTreeMap<String, RawField>| -> Result<BTreeMap<String, Field>> {
            fields
                .into_iter()
                .map(|(name, raw)| {
                    let field = refs
                        .compile(&name, raw, &patterns)
                        .with_context(|| format!("field {name:?}"))?;
                    Ok((name, field))
                })
                .collect()
        };
        Ok(Self {
            timezone,
            fields: compile(schema.fields)?,
            builtins: compile(schema.builtins)?,
            task_fields: compile(schema.task_fields)?,
            section_fields: compile(schema.section_fields)?,
            jsonl_fields: schema
                .jsonl_fields
                .into_iter()
                .map(|(path, fields)| Ok((path, compile(fields)?)))
                .collect::<Result<_>>()?,
            include,
            exclude,
            layout,
        })
    }

    pub fn includes(&self, path: &str) -> bool {
        self.include.is_match(path) && !self.excludes(path)
    }

    pub(crate) fn excludes(&self, path: &str) -> bool {
        self.exclude.is_match(path)
    }

    pub fn note_field(&self, name: &str) -> Option<&Field> {
        self.fields.get(name).or_else(|| self.builtins.get(name))
    }
}

fn globset(patterns: impl IntoIterator<Item = String>) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(GlobBuilder::new(&pattern).literal_separator(true).build()?);
    }
    Ok(builder.build()?)
}

struct References<F> {
    read: F,
    docs: BTreeMap<String, Yaml>,
}

impl<F: FnMut(&str) -> Result<String>> References<F> {
    fn resolve(&mut self, reference: &str) -> Result<Yaml> {
        let (file, key) = reference.split_once('#').unwrap_or((reference, ""));
        ensure!(
            file.ends_with(".yaml")
                && !file.starts_with('.')
                && file
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "Invalid schema reference {reference:?}"
        );
        if !self.docs.contains_key(file) {
            let doc =
                serde_yaml::from_str(&(self.read)(file).with_context(|| format!("Read {file}"))?)
                    .with_context(|| format!("Parse {file}"))?;
            self.docs.insert(file.to_owned(), doc);
        }
        let mut value = &self.docs[file];
        if let Some((list, member)) = key.split_once("[].") {
            let values = value[list]
                .as_sequence()
                .with_context(|| format!("Missing {reference}"))?;
            return Ok(Yaml::Sequence(
                values
                    .iter()
                    .map(|v| {
                        v.get(member)
                            .cloned()
                            .with_context(|| format!("Missing {reference}"))
                    })
                    .collect::<Result<_>>()?,
            ));
        }
        for part in key.split('.').filter(|part| !part.is_empty()) {
            value = value
                .get(part)
                .with_context(|| format!("Missing {reference}"))?;
        }
        Ok(value.clone())
    }

    fn compile(
        &mut self,
        name: &str,
        raw: RawField,
        patterns: &BTreeMap<String, String>,
    ) -> Result<Field> {
        let mut values = strings(&Yaml::Sequence(raw.values))?;
        for reference in references(&raw.values_from)? {
            let source = self.resolve(reference)?;
            if reference == "tags.yaml" {
                collect_tags(&source, &mut values)?;
            } else {
                values.extend(strings(&source)?);
            }
        }
        if let Some(reference) = raw.same_values_as {
            ensure!(
                values == strings(&self.resolve(&reference)?)?,
                "Area and domain lists disagree"
            );
        }
        let values_by_layer = raw
            .values_by_layer
            .into_iter()
            .map(|(layer, source)| {
                let source = match source.as_str() {
                    Some(reference) => self.resolve(reference)?,
                    None => source,
                };
                Ok((layer, strings(&source)?))
            })
            .collect::<Result<_>>()?;
        let aliases = if let Some(reference) = raw.aliases_from {
            let aliases: BTreeMap<String, String> =
                serde_yaml::from_value(self.resolve(&reference)?)?;
            aliases
                .into_iter()
                .map(|(k, v)| (k.to_lowercase(), v.to_lowercase()))
                .collect()
        } else {
            BTreeMap::new()
        };
        let stored_aliases = raw
            .legacy
            .into_values()
            .flatten()
            .flat_map(|(canonical, aliases)| {
                aliases
                    .into_iter()
                    .map(move |alias| (alias.to_lowercase(), canonical.clone()))
            })
            .collect();
        let pattern = match (raw.pattern, raw.pattern_from) {
            (Some(value), _) => Some(Regex::new(&value)?),
            (_, Some(key)) => Some(Regex::new(
                patterns.get(&key).context("Missing inherited pattern")?,
            )?),
            _ => None,
        };
        let mut required = raw.required;
        if let Some(reference) = required.types_requiring_from.take() {
            let types: BTreeMap<String, Yaml> = serde_yaml::from_value(self.resolve(&reference)?)?;
            required
                .types
                .extend(types.into_iter().filter_map(|(kind, value)| {
                    value["requires"]
                        .as_sequence()?
                        .iter()
                        .any(|field| field.as_str() == Some(name))
                        .then_some(kind)
                }));
        }
        Ok(Field {
            field_type: FieldType::parse(&raw.kind)?,
            values,
            values_by_layer,
            aliases,
            closed: raw.closed,
            writable_by_actions: raw.writable_by_actions,
            required,
            legacy_keys: raw.legacy_keys,
            legacy_repo_keys: raw.legacy_repo_keys,
            stored_aliases,
            slug: raw.format.as_deref() == Some("slug"),
            allow_http_urls: raw.allow_http_urls,
            pattern,
        })
    }
}

fn references(value: &Yaml) -> Result<Vec<&str>> {
    match value {
        Yaml::Null => Ok(Vec::new()),
        Yaml::String(value) => Ok(vec![value]),
        Yaml::Sequence(values) => values
            .iter()
            .map(|value| value.as_str().context("Expected a reference string"))
            .collect(),
        _ => bail!("Expected schema references"),
    }
}

pub(super) fn strings(value: &Yaml) -> Result<Vec<String>> {
    let string = |v: &Yaml| match v {
        Yaml::String(v) => Ok(v.clone()),
        Yaml::Number(v) => Ok(v.to_string()),
        _ => bail!("Expected a scalar vocabulary value"),
    };
    match value {
        Yaml::Sequence(items) => items.iter().map(string).collect(),
        Yaml::Mapping(items) => items.keys().map(string).collect(),
        _ => bail!("Expected a vocabulary list or mapping"),
    }
}

fn collect_tags(value: &Yaml, tags: &mut Vec<String>) -> Result<()> {
    match value {
        Yaml::Sequence(_) => tags.extend(strings(value)?),
        Yaml::Mapping(items) => {
            for value in items.values() {
                collect_tags(value, tags)?;
            }
        }
        _ => {}
    }
    Ok(())
}
