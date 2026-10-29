//! Explicit human Board writes. The selected file and its revision are fixed
//! when editing starts; another window's changes require a reload.
use super::*;
use crate::area_files::{self, Document, Format};
use serde_json::{json, Value};
use std::{io::Write, path::PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Problems(pub BTreeMap<String, String>);
impl std::fmt::Display for Problems {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (field, detail) in &self.0 {
            writeln!(f, "{field}: {detail}")?;
        }
        Ok(())
    }
}
impl std::error::Error for Problems {}
impl Problems {
    fn require(&mut self, valid: bool, field: &str, detail: &str) {
        if !valid {
            self.0.insert(field.into(), detail.into());
        }
    }
    fn finish(self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(self.into())
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Details {
    pub title: String,
    pub goal: String,
    pub stage_note: String,
}
#[derive(Clone, Debug, Default)]
pub struct Card {
    pub issue: String,
    pub expected: String,
    pub executor: String,
    /// Empty = automatic evidence; only Ready/Verified are human overrides.
    pub stage: String,
}
#[derive(Clone, Debug)]
pub enum Change {
    Details(Details),
    Add(Card),
    Edit(usize, Card),
    Remove(usize),
}

#[derive(Clone, Debug)]
pub struct Session {
    pub area: PathBuf,
    pub document: Option<Document>,
    project: Document,
    pub value: Value,
    pub stage_notes: Vec<String>,
    inactive_legacy: bool,
}

pub fn issue_number(text: &str, repository: &str) -> Result<u64> {
    let text = text.trim();
    if let Ok(number) = text.strip_prefix('#').unwrap_or(text).parse::<u64>() {
        ensure!(number > 0, "Use a positive issue number");
        return Ok(number);
    }
    let url = url::Url::parse(text).context("Use an issue number or its Forgejo URL")?;
    let repo = url::Url::parse(repository)?;
    ensure!(
        url.origin() == repo.origin()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Choose an issue in this Board's repository"
    );
    let prefix = format!("{}/issues/", repo.path().trim_end_matches('/'));
    let number = url
        .path()
        .strip_prefix(&prefix)
        .context("Choose an issue in this Board's repository")?;
    ensure!(
        !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()),
        "Use an exact issue URL"
    );
    let number: u64 = number.parse()?;
    ensure!(number > 0, "Use a positive issue number");
    Ok(number)
}

fn stage_notes(area: &Path) -> Result<Vec<String>> {
    let canonical = area.canonicalize()?;
    let planning = area.join("planning");
    let mut pending = vec![planning];
    let mut notes = Vec::new();
    while let Some(folder) = pending.pop() {
        let meta = match std::fs::symlink_metadata(&folder) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        ensure!(
            folder.canonicalize()?.starts_with(&canonical),
            "Planning folder escapes the Area"
        );
        for entry in std::fs::read_dir(folder)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|e| e == "md") {
                notes.push(
                    path.strip_prefix(area)?
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
        }
    }
    notes.sort();
    Ok(notes)
}

impl Session {
    pub fn load(area: &Path) -> Result<Self> {
        let project = area_files::read(area, "project")?
            .context("Create a project before creating its Board")?;
        let document = area_files::read(area, "board")?;
        let stage_notes = stage_notes(area)?;
        let value = match &document {
            Some(document) => {
                // Root errors cannot be silently rewritten by the editor.
                Binding::read(area)?;
                serde_json::to_value(&document.value)?
            }
            None => {
                let project_name = crate::projects::Project::read(area)?.name;
                let p = &project.value;
                let goal = p
                    .get("goal")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        p.get("product")
                            .and_then(|v| v.get("outcome"))
                            .and_then(|v| v.as_str())
                    })
                    .or_else(|| p.get("description").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let repo = &p["repo"];
                let repository = repo["host"]
                    .as_str()
                    .zip(repo["full_name"].as_str())
                    .map(|(host, name)| format!("https://{host}/{name}"))
                    .unwrap_or_default();
                json!({"version":1,"title":project_name,"goal":goal,"repository":repository,
                    "stage_note":if stage_notes.len()==1 { stage_notes[0].as_str() } else { "" }, "issues":[]})
            }
        };
        let inactive_legacy = document.as_ref().is_some_and(|d| d.format == Format::Toml)
            && std::fs::symlink_metadata(area.join("ops/board.yaml")).is_ok();
        Ok(Self {
            area: area.into(),
            document,
            project,
            value,
            stage_notes,
            inactive_legacy,
        })
    }
    pub fn format_label(&self) -> &'static str {
        if self.inactive_legacy {
            return "Editing board.toml · TOML; board.yaml is inactive";
        }
        match self.document.as_ref().map(|d| d.format) {
            Some(Format::Yaml) => "Editing board.yaml · JSON",
            Some(Format::Toml) => "Editing board.toml · TOML",
            None => "New board.toml · TOML",
        }
    }
    pub fn details(&self) -> Details {
        Details {
            title: self.value["title"].as_str().unwrap_or_default().into(),
            goal: self.value["goal"].as_str().unwrap_or_default().into(),
            stage_note: self.value["stage_note"].as_str().unwrap_or_default().into(),
        }
    }
    pub fn cards(&self) -> &[Value] {
        self.value["issues"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    pub fn card(&self, index: usize) -> Card {
        let value = &self.cards()[index];
        Card {
            issue: value["number"]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_default(),
            expected: value["expected"].as_str().unwrap_or_default().into(),
            executor: value["executor"].as_str().unwrap_or_default().into(),
            stage: value["stage"].as_str().unwrap_or_default().into(),
        }
    }
    fn validate_details(&self, details: &Details) -> Result<()> {
        let mut errors = Problems::default();
        errors.require(
            !details.title.trim().is_empty(),
            "title",
            "Give the Board a title",
        );
        errors.require(
            !details.goal.trim().is_empty(),
            "goal",
            "Describe the expected project outcome",
        );
        let note = &details.stage_note;
        let valid = relative_note(note)
            && self.area.join(note).is_file()
            && self.area.join(note).canonicalize().is_ok_and(|p| {
                self.area
                    .canonicalize()
                    .is_ok_and(|root| p.starts_with(root))
            });
        errors.require(
            valid,
            "stage_note",
            "Choose an existing Markdown stage note inside this project",
        );
        if self.document.is_none() {
            errors.require(
                note.starts_with("planning/"),
                "stage_note",
                "Choose a stage note from Planning",
            );
            let repository = self.value["repository"].as_str().unwrap_or_default();
            errors.require(
                https_link(repository),
                "repository",
                "Set the project's repository before creating its Board",
            );
        }
        errors.finish()
    }
    fn card_value(&self, index: Option<usize>, card: &Card) -> Result<Value> {
        let mut errors = Problems::default();
        let number = issue_number(
            &card.issue,
            self.value["repository"].as_str().unwrap_or_default(),
        );
        if let Err(error) = &number {
            errors.0.insert("issue".into(), error.to_string());
        }
        if let Ok(number) = &number {
            errors.require(
                !self
                    .cards()
                    .iter()
                    .enumerate()
                    .any(|(i, c)| Some(i) != index && c["number"].as_u64() == Some(*number)),
                "issue",
                "This issue already has a card",
            );
        }
        errors.require(
            !card.expected.trim().is_empty(),
            "expected",
            "Describe the expected result",
        );
        errors.require(
            !card.executor.trim().is_empty(),
            "executor",
            "Name the executor",
        );
        errors.require(
            matches!(card.stage.as_str(), "" | "ready" | "verified"),
            "stage",
            "Choose Automatic, Ready or Verified",
        );
        errors.finish()?;
        let mut value = index
            .map(|i| self.cards()[i].clone())
            .unwrap_or_else(|| json!({}));
        // An invalid scalar card may be repaired without replacing healthy cards.
        if !value.is_object() {
            value = json!({});
        }
        value["number"] = json!(number?);
        value["expected"] = json!(card.expected.trim());
        value["executor"] = json!(card.executor.trim());
        if card.stage.is_empty() {
            value.as_object_mut().unwrap().remove("stage");
        } else {
            value["stage"] = json!(card.stage);
        }
        Ok(value)
    }
    pub fn preview(&self, change: &Change) -> Result<String> {
        let mut value = self.value.clone();
        let changed_root = match change {
            Change::Details(details) => {
                self.validate_details(details)?;
                value["title"] = json!(details.title.trim());
                value["goal"] = json!(details.goal.trim());
                value["stage_note"] = json!(details.stage_note);
                "details"
            }
            Change::Add(card) => {
                value["issues"]
                    .as_array_mut()
                    .context("issues must be an array")?
                    .push(self.card_value(None, card)?);
                "issues"
            }
            Change::Edit(index, card) => {
                ensure!(
                    *index < self.cards().len(),
                    "Card changed; reload the Board"
                );
                value["issues"][*index] = self.card_value(Some(*index), card)?;
                "issues"
            }
            Change::Remove(index) => {
                ensure!(
                    *index < self.cards().len(),
                    "Card changed; reload the Board"
                );
                value["issues"].as_array_mut().unwrap().remove(*index);
                "issues"
            }
        };
        let source = match self.document.as_ref().map(|d| d.format) {
            Some(Format::Yaml) => serde_json::to_string_pretty(&value)? + "\n",
            Some(Format::Toml) => {
                let document = self.document.as_ref().unwrap();
                if changed_root == "details" {
                    let mut doc = document.clone();
                    for name in ["title", "goal", "stage_note"] {
                        doc.source =
                            doc.set_toml_field(name, Some(&serde_yaml::to_value(&value[name])?))?;
                        doc.value = area_files::parse(&doc.source, Format::Toml)?;
                    }
                    doc.source
                } else {
                    // Only the changed card fields are replaced. All other
                    // tables, comments and optional metadata retain their text.
                    let mut doc = document.source.parse::<toml_edit::DocumentMut>()?;
                    let mut serialized =
                        toml::to_string_pretty(&value)?.parse::<toml_edit::DocumentMut>()?;
                    if let Some(tables) = doc["issues"].as_array_of_tables_mut() {
                        match change {
                            Change::Remove(i) => {
                                tables.remove(*i);
                            }
                            Change::Add(_) => {
                                let added = serialized["issues"]
                                    .as_array_of_tables_mut()
                                    .context("Missing new card")?
                                    .iter()
                                    .last()
                                    .context("Missing new card")?
                                    .clone();
                                tables.push(added);
                            }
                            Change::Edit(i, _) => {
                                let original = tables.get_mut(*i).context("Card changed")?;
                                let replacement = serialized["issues"]
                                    .as_array_of_tables()
                                    .and_then(|a| a.get(*i))
                                    .context("Missing edited card")?;
                                for name in ["number", "expected", "executor", "stage"] {
                                    if let Some(next) = replacement.get(name) {
                                        let mut next = next.clone();
                                        if let (Some(before), Some(after)) = (
                                            original.get(name).and_then(|v| v.as_value()),
                                            next.as_value_mut(),
                                        ) {
                                            *after.decor_mut() = before.decor().clone();
                                        }
                                        if let Some(key) = original.key(name).cloned() {
                                            original.insert_formatted(&key, next);
                                        } else {
                                            original.insert(name, next);
                                        }
                                    } else {
                                        original.remove(name);
                                    }
                                }
                            }
                            _ => unreachable!(),
                        }
                        if tables.is_empty() {
                            doc["issues"] = toml_edit::value(toml_edit::Array::new());
                        }
                    } else if let Some(array) =
                        doc["issues"].as_array_mut().filter(|a| !a.is_empty())
                    {
                        match change {
                            Change::Remove(i) => {
                                array.remove(*i);
                            }
                            Change::Add(_) | Change::Edit(_, _) => {
                                let index = if let Change::Edit(i, _) = change {
                                    *i
                                } else {
                                    value["issues"].as_array().unwrap().len() - 1
                                };
                                let replacement = toml::to_string_pretty(&value["issues"][index])?
                                    .parse::<toml_edit::DocumentMut>()?
                                    .into_table()
                                    .into_inline_table();
                                if let Change::Edit(i, _) = change {
                                    let item = array.get_mut(*i).context("Card changed")?;
                                    if let Some(original) = item.as_inline_table_mut() {
                                        for name in ["number", "expected", "executor", "stage"] {
                                            if let Some(next) = replacement.get(name) {
                                                let mut next = next.clone();
                                                if let Some(before) = original.get(name) {
                                                    *next.decor_mut() = before.decor().clone();
                                                }
                                                if let Some(key) = original.key(name).cloned() {
                                                    original.insert_formatted(&key, next);
                                                } else {
                                                    original.insert(name, next);
                                                }
                                            } else {
                                                original.remove(name);
                                            }
                                        }
                                    } else {
                                        let decor = item.decor().clone();
                                        *item = toml_edit::Value::InlineTable(replacement);
                                        *item.decor_mut() = decor;
                                    }
                                } else {
                                    array.push(replacement);
                                }
                            }
                            _ => unreachable!(),
                        }
                    } else {
                        ensure!(self.cards().is_empty(), "Unsupported issues layout");
                        doc.insert(
                            "issues",
                            serialized.remove("issues").context("Missing cards")?,
                        );
                    }
                    doc.to_string()
                }
            }
            None => toml::to_string_pretty(&value)?,
        };
        // Root validity is retained. Existing per-card errors must not suppress
        // an unrelated repair; they remain visible in the Board projection.
        ensure!(
            source.len() as u64 <= area_files::MAX_BYTES,
            "Board exceeds the reader's 512 KiB limit; shorten the draft before saving"
        );
        let parsed = if self
            .document
            .as_ref()
            .is_some_and(|d| d.format == Format::Yaml)
        {
            Binding::parse(&source)?
        } else {
            Binding::parse_toml(&source)?
        };
        ensure!(
            parsed.repository == self.value["repository"].as_str().unwrap_or_default(),
            "Repository changed"
        );
        Ok(source)
    }
    fn check_project(&self) -> Result<()> {
        let project =
            area_files::read(&self.area, "project")?.context("Project manifest disappeared")?;
        ensure!(
            project.path == self.project.path && project.source == self.project.source,
            "Project changed; reload before saving"
        );
        Ok(())
    }
    pub fn undo(&self, previous: &str) -> Result<Self> {
        self.check_project()?;
        let document = self.document.as_ref().context("No saved Board to undo")?;
        let parsed = match document.format {
            Format::Toml => Binding::parse_toml(previous)?,
            Format::Yaml => Binding::parse(previous)?,
        };
        ensure!(
            Some(parsed.repository.as_str()) == self.value["repository"].as_str(),
            "Undo belongs to a different repository"
        );
        document.replace(previous)?;
        Self::load(&self.area)
    }
    pub fn save(&self, change: &Change) -> Result<Self> {
        let source = self.preview(change)?;
        self.check_project()?;
        if let Some(document) = &self.document {
            document.replace(&source)?;
        } else {
            ensure!(
                matches!(change, Change::Details(_)),
                "Create the Board before adding cards"
            );
            ensure!(
                area_files::preferred_path(&self.area, "board")?.is_none(),
                "Board appeared; reload before saving"
            );
            let ops = self.area.join("ops");
            let temporary = ops.join(format!(".board.{}.tmp", uuid::Uuid::new_v4()));
            let result = (|| -> Result<()> {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?;
                file.write_all(source.as_bytes())?;
                file.sync_all()?;
                drop(file);
                ensure!(
                    area_files::preferred_path(&self.area, "board")?.is_none(),
                    "Board appeared; reload before saving"
                );
                std::fs::hard_link(&temporary, ops.join("board.toml"))?;
                #[cfg(unix)]
                std::fs::File::open(&ops)?.sync_all()?;
                Ok(())
            })();
            let _ = std::fs::remove_file(temporary);
            result?;
        }
        Self::load(&self.area)
    }
}

#[cfg(test)]
mod tests;
