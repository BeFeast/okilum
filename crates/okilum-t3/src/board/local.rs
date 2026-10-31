//! Local cards are canonical project files, independent of Forgejo/executor facts.
//! An optional sidecar preserves existing Board files and older readers.
use super::{Column, Source};
use crate::area_files::{self, Document, Format};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Todo,
    Doing,
    Done,
}
impl State {
    pub const ALL: [Self; 3] = [Self::Todo, Self::Doing, Self::Done];
    pub fn label(self) -> &'static str {
        match self {
            Self::Todo => "Todo",
            Self::Doing => "Doing",
            Self::Done => "Done",
        }
    }
    pub fn column(self) -> Column {
        match self {
            Self::Todo => Column::Queue,
            Self::Doing => Column::Working,
            Self::Done => Column::Ready,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub title: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub state: State,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub state: State,
}
impl Card {
    fn from_draft(id: &str, draft: &Draft) -> Result<Self> {
        let card = Self {
            id: id.into(),
            title: draft.title.trim().into(),
            details: draft.details.clone(),
            state: draft.state,
        };
        card.validate()?;
        Ok(card)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(&self.id).is_ok_and(|id| id.to_string() == self.id),
            "Local card id must be a canonical UUID"
        );
        ensure!(
            !self.title.trim().is_empty() && !self.title.chars().any(char::is_control),
            "Give the card a title on one line"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Default)]
pub struct Projection {
    pub cards: Vec<Card>,
    pub problems: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    cards: Vec<toml::Value>,
}
fn parse(source: &str) -> Result<(File, Projection)> {
    let file: File = toml::from_str(source).context("Invalid local Board TOML")?;
    ensure!(file.version == 1, "Unsupported local Board version");
    let mut counts = BTreeMap::<&str, usize>::new();
    for value in &file.cards {
        if let Some(id) = value.get("id").and_then(toml::Value::as_str) {
            *counts.entry(id).or_default() += 1;
        }
    }
    let mut projection = Projection::default();
    for (index, value) in file.cards.iter().enumerate() {
        let read = (|| -> Result<Card> {
            let card: Card = value.clone().try_into()?;
            card.validate()?;
            ensure!(
                counts.get(card.id.as_str()) == Some(&1),
                "Duplicate local card identity; no card selected"
            );
            Ok(card)
        })();
        match read {
            Ok(card) => projection.cards.push(card),
            Err(e) => projection
                .problems
                .push(format!("ops/board-local.toml: cards[{index}]: {e:#}")),
        }
    }
    Ok((file, projection))
}
fn document(area: &Path) -> Result<Option<Document>> {
    let doc = area_files::read(area, "board-local")?;
    ensure!(
        doc.as_ref().is_none_or(|d| d.format == Format::Toml),
        "Local cards use ops/board-local.toml; legacy YAML is not converted"
    );
    Ok(doc)
}
pub fn read(area: &Path) -> Result<Projection> {
    document(area)?.map_or(Ok(Projection::default()), |doc| {
        parse(&doc.source).map(|(_, p)| p)
    })
}
#[derive(Clone, Debug)]
pub enum Change {
    Add(String, Draft),
    Edit(String, Draft),
    Remove(String),
}
#[derive(Clone, Debug)]
pub struct Session {
    area: PathBuf,
    project: Document,
    board: Document,
    pub document: Option<Document>,
    pub projection: Projection,
}
impl Session {
    pub fn load(area: &Path) -> Result<Self> {
        let project = area_files::read(area, "project")?.context("Create the project first")?;
        let board = area_files::read(area, "board")?.context("Create the Board first")?;
        let document = document(area)?;
        let projection = document.as_ref().map_or(Ok(Projection::default()), |d| {
            parse(&d.source).map(|(_, p)| p)
        })?;
        Ok(Self {
            area: area.into(),
            project,
            board,
            document,
            projection,
        })
    }
    fn check_sources(&self) -> Result<()> {
        for (name, expected) in [("project", &self.project), ("board", &self.board)] {
            let actual = area_files::read(&self.area, name)?
                .context("Project or Board disappeared; reload before saving")?;
            ensure!(
                actual.path == expected.path && actual.source == expected.source,
                "{name} changed; reload before saving"
            );
        }
        // Document::replace may return early for unchanged output. A stale
        // no-op Save or Undo must still report the external edit to the user.
        let actual = document(&self.area)?;
        ensure!(
            match (&actual, &self.document) {
                (None, None) => true,
                (Some(actual), Some(expected)) => {
                    actual.path == expected.path && actual.source == expected.source
                }
                _ => false,
            },
            "Local Board changed; reload before saving"
        );
        Ok(())
    }
    pub fn preview(&self, change: &Change) -> Result<String> {
        let source = self
            .document
            .as_ref()
            .map_or("version = 1\ncards = []\n", |d| d.source.as_str());
        let (file, _) = parse(source)?;
        let id = match change {
            Change::Add(id, _) | Change::Edit(id, _) | Change::Remove(id) => id,
        };
        let indices: Vec<_> = file
            .cards
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                (v.get("id").and_then(toml::Value::as_str) == Some(id.as_str())).then_some(i)
            })
            .collect();
        let index = match change {
            Change::Add(_, _) => {
                ensure!(
                    indices.is_empty(),
                    "Local card already exists; reload before saving"
                );
                file.cards.len()
            }
            _ => {
                ensure!(
                    indices.len() == 1,
                    "Local card changed or its identity is ambiguous; reload before saving"
                );
                indices[0]
            }
        };
        let replacement = match change {
            Change::Add(_, draft) | Change::Edit(_, draft) => Some(Card::from_draft(id, draft)?),
            Change::Remove(_) => None,
        };
        let mut doc = source.parse::<toml_edit::DocumentMut>()?;
        let table = replacement
            .as_ref()
            .map(|card| -> Result<toml_edit::Table> {
                Ok(toml::to_string(card)?
                    .parse::<toml_edit::DocumentMut>()?
                    .into_table())
            })
            .transpose()?;
        if let Some(tables) = doc["cards"].as_array_of_tables_mut() {
            match change {
                Change::Add(_, _) => tables.push(table.context("Missing card")?),
                Change::Remove(_) => {
                    tables.remove(index);
                }
                Change::Edit(_, _) => {
                    let target = tables.get_mut(index).context("Local card changed")?;
                    let replacement = table.context("Missing card")?;
                    for name in ["title", "details", "state"] {
                        let mut next = replacement.get(name).context("Missing field")?.clone();
                        if let (Some(old), Some(new)) = (
                            target.get(name).and_then(|v| v.as_value()),
                            next.as_value_mut(),
                        ) {
                            *new.decor_mut() = old.decor().clone();
                        }
                        if let Some(key) = target.key(name).cloned() {
                            target.insert_formatted(&key, next);
                        } else {
                            target.insert(name, next);
                        }
                    }
                }
            }
            if tables.is_empty() {
                doc["cards"] = toml_edit::value(toml_edit::Array::new());
            }
        } else if let Some(array) = doc["cards"].as_array_mut() {
            match change {
                Change::Add(_, _) => array.push(table.context("Missing card")?.into_inline_table()),
                Change::Edit(_, _) => {
                    let target = array.get_mut(index).context("Local card changed")?;
                    let replacement = table.context("Missing card")?.into_inline_table();
                    if let Some(original) = target.as_inline_table_mut() {
                        for name in ["title", "details", "state"] {
                            let mut next = replacement.get(name).context("Missing field")?.clone();
                            if let Some(old) = original.get(name) {
                                *next.decor_mut() = old.decor().clone();
                            }
                            if let Some(key) = original.key(name).cloned() {
                                original.insert_formatted(&key, next);
                            } else {
                                original.insert(name, next);
                            }
                        }
                    } else {
                        *target = toml_edit::Value::InlineTable(replacement);
                    }
                }
                Change::Remove(_) => {
                    array.remove(index);
                }
            }
        } else {
            anyhow::bail!("Unsupported local card layout");
        }
        let updated = doc.to_string();
        ensure!(
            updated.len() as u64 <= area_files::MAX_BYTES,
            "Local cards exceed the reader's 512 KiB limit"
        );
        let (_, parsed) = parse(&updated)?;
        if let Some(card) = replacement {
            ensure!(
                parsed.cards.iter().any(|v| v == &card),
                "Local card did not round-trip"
            );
        }
        Ok(updated)
    }
    pub fn save(&self, change: &Change) -> Result<Self> {
        let updated = self.preview(change)?;
        self.check_sources()?;
        if let Some(doc) = &self.document {
            doc.replace(&updated)?;
        } else {
            ensure!(
                document(&self.area)?.is_none(),
                "Local Board appeared; reload before saving"
            );
            let ops = self.area.join("ops");
            let temporary = ops.join(format!(".board-local.{}.tmp", uuid::Uuid::new_v4()));
            let result = (|| -> Result<()> {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?;
                file.write_all(updated.as_bytes())?;
                file.sync_all()?;
                drop(file);
                self.check_sources()?;
                ensure!(
                    document(&self.area)?.is_none(),
                    "Local Board appeared; reload before saving"
                );
                std::fs::hard_link(&temporary, ops.join("board-local.toml"))?;
                #[cfg(unix)]
                std::fs::File::open(&ops)?.sync_all()?;
                Ok(())
            })();
            let _ = std::fs::remove_file(temporary);
            result?;
        }
        Self::load(&self.area)
    }
    pub fn undo(&self, previous: &str) -> Result<Self> {
        self.check_sources()?;
        parse(previous)?;
        self.document
            .as_ref()
            .context("No local card write to undo")?
            .replace(previous)?;
        Self::load(&self.area)
    }
}
/// The Board keeps this source independently: a failed refresh cannot freshen
/// or silently erase the last known local cards.
pub type Snapshot = Source<Projection>;

#[cfg(test)]
mod tests;
