//! The Apply step of editing a drawing (#478): from the drawing file and the
//! scene an editor hands back to the exact new file bytes, written through the
//! same revision-checked save as notes. No UI lives here.
//!
//! One [`DrawingEdit`] per explicit Edit:
//! 1. [`DrawingEdit::open`] reads the file through a [`FileEditor`];
//!    [`DrawingEdit::editor_scene`] is what the editor loads.
//! 2. The editor reports the scene it holds right after loading
//!    ([`DrawingEdit::loaded`]), so its load normalisation is not an edit.
//! 3. [`DrawingEdit::apply`] writes the edited scene. If the file changed
//!    outside since it was loaded, nothing is written and the edit waits for
//!    the user's choice, as for notes: [`DrawingEdit::reload`],
//!    [`DrawingEdit::keep_mine`] or [`DrawingEdit::save_as_copy`].
//! 4. [`DrawingEdit::undo`] restores the bytes the last save replaced, once.
//!
//! Every call carries the session revision the editor last received. A call
//! with another revision is refused: after Reload or Undo the editor must load
//! the scene again before it can Apply.
//!
//! Links in text: an Obsidian drawing in `parsed` mode shows a text element's
//! links resolved (`See the plan`) and keeps the source (`See [[Project
//! Plan|the plan]]`) in `rawText` and `## Text Elements`. The editor gets the
//! source, so a link survives any edit. An element whose text the user left
//! alone keeps the file's resolved text when it is saved.
use super::write::{is_markdown, vault_file_ids};
use super::{write, Scene, WriteOutcome, MAX_SOURCE_BYTES};
use crate::file_editor::{FileEditor, Save};
use anyhow::{anyhow, ensure, Context, Result};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Image data an editor may save inline. SVG is left out: it can carry script.
const INLINE_IMAGES: &[&str] = &[
    "data:image/png;base64,",
    "data:image/jpeg;base64,",
    "data:image/webp;base64,",
    "data:image/gif;base64,",
];
/// Vault images shown in the editor, in total. More are shown as placeholders.
const DISPLAY_IMAGE_BYTES: usize = MAX_SOURCE_BYTES / 2;
const RESOLVE_FIRST: &str = "Choose Reload, Keep mine or Save as copy first.";

/// Where a drawing session reads and writes the file. Production uses the
/// note [`FileEditor`]: its recovery draft, editor lock and revision check.
pub trait DrawingStore {
    /// The file on disk now.
    fn current(&self) -> Result<String>;
    /// Keep `text` as the recovery draft. The file is not touched.
    fn stage(&mut self, text: String) -> Result<()>;
    /// Write the staged draft, unless the file changed since it was read.
    fn save(&mut self) -> Result<Save>;
    /// Write the staged draft over `reviewed`, if that is still the file.
    fn keep_mine(&mut self, reviewed: &str) -> Result<Save>;
    /// Drop the draft and read the file again; returns it.
    fn reload(&mut self) -> Result<String>;
}

impl DrawingStore for FileEditor {
    fn current(&self) -> Result<String> {
        FileEditor::current(self)
    }
    fn stage(&mut self, text: String) -> Result<()> {
        FileEditor::set_text(self, text)
    }
    fn save(&mut self) -> Result<Save> {
        FileEditor::save(self)
    }
    fn keep_mine(&mut self, reviewed: &str) -> Result<Save> {
        FileEditor::keep_mine(self, reviewed)
    }
    fn reload(&mut self) -> Result<String> {
        FileEditor::reload(self)?;
        Ok(self.text().to_owned())
    }
}

/// What an Apply or Keep mine did.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// The file holds the edit. The editor continues at `revision`.
    Saved { revision: u64 },
    /// The edit changes nothing in the file; nothing was written.
    Unchanged { revision: u64 },
    /// The file changed outside since the editor loaded it. Nothing was
    /// written; `disk` is that version. The edit is kept in recovery until
    /// Reload, Keep mine or Save as copy.
    Conflict { disk: String },
}

/// What an Undo did.
#[derive(Debug, PartialEq, Eq)]
pub enum Undone {
    /// The file holds the bytes from before the save again. The editor must
    /// load the scene at `revision`.
    Restored { revision: u64 },
    /// The file changed outside since the save, so Undo wrote nothing and
    /// still holds its snapshot. `disk` is the file now.
    Blocked { disk: String },
}

/// An Apply the file refused because it changed outside.
struct Pending {
    /// The complete file the edit produced.
    mine: String,
    /// The editor's scene for `mine`.
    edited: Value,
    /// The outside version the user saw when choosing.
    disk: String,
    /// The copy Save as copy made, if any.
    copy: Option<PathBuf>,
}

/// One explicit edit of one drawing file.
pub struct DrawingEdit<S: DrawingStore = FileEditor> {
    store: S,
    /// Canonical vault root.
    root: PathBuf,
    /// The drawing, relative to `root`.
    rel: PathBuf,
    /// The file as the editor's scene was made from it.
    opened: String,
    /// Vault images inlined for display only; never written to the JSON.
    display_files: Map<String, Value>,
    /// The scene the editor reported after loading `opened`.
    baseline: Option<Value>,
    revision: u64,
    /// The file before the last save, for one Undo.
    undo: Option<String>,
    conflict: Option<Pending>,
}

impl DrawingEdit<FileEditor> {
    /// Open `path` for editing. `state` is the editor recovery folder notes use.
    /// A drawing with an unsaved recovery draft is refused: that draft must be
    /// resolved through recovery, never replaced by a new edit.
    pub fn open(root: &Path, path: &Path, state: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        let path = path.canonicalize()?;
        ensure!(path.starts_with(&root), "Drawing is outside the vault");
        ensure!(
            super::is_drawing(&path.to_string_lossy()),
            "Not an Excalidraw file"
        );
        ensure!(
            std::fs::metadata(&path)?.len() <= MAX_SOURCE_BYTES as u64,
            "Drawing file is too large"
        );
        let store = FileEditor::open(&path, state)?;
        ensure!(
            !store.dirty(),
            "This drawing has an unsaved recovery draft. Resolve it before editing the drawing."
        );
        let opened = store.text().to_owned();
        Self::with_store(store, &root, &path, opened)
    }
}

impl<S: DrawingStore> DrawingEdit<S> {
    fn with_store(store: S, root: &Path, path: &Path, opened: String) -> Result<Self> {
        let rel = path
            .strip_prefix(root)
            .context("Drawing is outside the vault")?
            .to_owned();
        let scene = Scene::parse(&opened)?;
        Ok(Self {
            store,
            display_files: display_images(root, path, &scene),
            root: root.to_owned(),
            rel,
            opened,
            baseline: None,
            revision: 0,
            undo: None,
            conflict: None,
        })
    }

    /// The file as the editor's current scene was made from it.
    pub fn opened(&self) -> &str {
        &self.opened
    }

    /// The drawing, relative to the vault root.
    pub fn path(&self) -> &Path {
        &self.rel
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn can_undo(&self) -> bool {
        self.undo.is_some() && self.conflict.is_none()
    }

    /// The outside version of a pending conflict, if any.
    pub fn conflict(&self) -> Option<&str> {
        self.conflict.as_ref().map(|pending| pending.disk.as_str())
    }

    /// The scene the editor loads: the file's scene with linked text as its
    /// `[[…]]` source and vault images inlined for display.
    pub fn editor_scene(&self) -> Result<Value> {
        let mut scene = Scene::parse(&self.opened)?.json;
        if is_markdown(&self.opened) {
            for element in elements_mut(&mut scene) {
                if let Some(source) = linked_source(element).map(str::to_owned) {
                    element["text"] = Value::String(source.clone());
                    element["originalText"] = Value::String(source);
                }
            }
        }
        if !self.display_files.is_empty() {
            let object = scene.as_object_mut().context("Scene is not an object")?;
            let files = object
                .entry("files")
                .or_insert_with(|| Value::Object(Map::new()));
            if !files.is_object() {
                *files = Value::Object(Map::new());
            }
            let files = files.as_object_mut().expect("files object");
            files.extend(self.display_files.clone());
        }
        Ok(scene)
    }

    /// Record the scene the editor holds right after loading, once per load.
    pub fn loaded(&mut self, revision: u64, baseline: Value) -> Result<()> {
        self.check_revision(revision)?;
        ensure!(
            self.baseline.is_none(),
            "The editor already loaded this drawing"
        );
        // Shape, ids and bounds are checked now, not at the first Apply.
        let (file_baseline, _) = self.file_terms(&baseline, &baseline)?;
        write(&self.opened, Some(&file_baseline), &file_baseline)?;
        self.baseline = Some(baseline);
        Ok(())
    }

    /// The file `edited` would produce, without writing anything.
    pub fn prepare(&self, edited: &Value) -> Result<WriteOutcome> {
        let baseline = self
            .baseline
            .as_ref()
            .ok_or_else(|| anyhow!("The editor has not loaded this drawing"))?;
        self.check_images(edited)?;
        let (baseline, edited) = self.file_terms(baseline, edited)?;
        write(&self.opened, Some(&baseline), &edited)
    }

    /// Save `edited`. A scene that changes nothing writes nothing.
    pub fn apply(&mut self, revision: u64, edited: &Value) -> Result<Applied> {
        self.check_revision(revision)?;
        ensure!(self.conflict.is_none(), "{RESOLVE_FIRST}");
        let WriteOutcome::Updated(mine) = self.prepare(edited)? else {
            return Ok(Applied::Unchanged {
                revision: self.revision,
            });
        };
        // Recovery first: a refused or failed write keeps the authored drawing.
        self.store.stage(mine.clone())?;
        let disk = self.store.current()?;
        if disk != self.opened {
            return Ok(self.conflicted(mine, edited, disk));
        }
        if self.store.save()? == Save::Conflict {
            let disk = self.store.current()?;
            return Ok(self.conflicted(mine, edited, disk));
        }
        let previous = std::mem::replace(&mut self.opened, mine);
        Ok(self.saved(previous, edited.clone()))
    }

    /// Conflict choice: write this edit over `reviewed`, the outside version
    /// the user saw in the conflict. Consent covers that version only: if the
    /// file is no longer `reviewed`, nothing is written and the conflict shows
    /// the version now on disk, which needs its own Keep mine.
    pub fn keep_mine(&mut self, revision: u64, reviewed: &str) -> Result<Applied> {
        self.check_revision(revision)?;
        let pending = self
            .conflict
            .as_ref()
            .ok_or_else(|| anyhow!("There is no conflict to resolve"))?;
        if pending.disk != reviewed {
            return Ok(Applied::Conflict {
                disk: pending.disk.clone(),
            });
        }
        self.store.stage(pending.mine.clone())?;
        if self.store.keep_mine(reviewed)? == Save::Conflict {
            let disk = self.store.current()?;
            let pending = self.conflict.as_mut().expect("checked conflict");
            pending.disk = disk.clone();
            return Ok(Applied::Conflict { disk });
        }
        let pending = self.conflict.take().expect("checked conflict");
        self.opened = pending.mine;
        Ok(self.saved(pending.disk, pending.edited))
    }

    /// Conflict choice, or any time: drop the edit and take the file on disk.
    /// The editor must load [`Self::editor_scene`] at the returned revision.
    pub fn reload(&mut self, revision: u64) -> Result<u64> {
        self.check_revision(revision)?;
        self.opened = self.store.reload()?;
        self.conflict = None;
        self.undo = None;
        self.baseline = None;
        self.revision += 1;
        self.refresh_display_files();
        Ok(self.revision)
    }

    /// Conflict choice: keep this edit as a new drawing next to the original,
    /// which stays as it is. The conflict stays open until Reload or Keep mine.
    /// Returns the copy's path relative to the vault root.
    pub fn save_as_copy(&mut self, revision: u64) -> Result<PathBuf> {
        self.check_revision(revision)?;
        let pending = self
            .conflict
            .as_mut()
            .ok_or_else(|| anyhow!("There is no conflict to resolve"))?;
        if let Some(copy) = &pending.copy {
            if std::fs::read(self.root.join(copy)).ok().as_deref() == Some(pending.mine.as_bytes())
            {
                return Ok(copy.clone());
            }
        }
        let copy = conflict_copy(&self.rel);
        crate::note_files::create_attachment_with_source(
            &self.root,
            &copy,
            pending.mine.as_bytes(),
        )?;
        pending.copy = Some(copy.clone());
        Ok(copy)
    }

    /// Restore the exact bytes the last save replaced. Only once, and never
    /// over a file that changed outside since.
    pub fn undo(&mut self, revision: u64) -> Result<Undone> {
        self.check_revision(revision)?;
        ensure!(self.conflict.is_none(), "{RESOLVE_FIRST}");
        let previous = self
            .undo
            .clone()
            .ok_or_else(|| anyhow!("There is no drawing save to undo"))?;
        let disk = self.store.current()?;
        if disk != self.opened {
            return Ok(Undone::Blocked { disk });
        }
        self.store.stage(previous.clone())?;
        if self.store.save()? == Save::Conflict {
            // The draft goes back to the saved drawing: Undo was not done.
            self.store.stage(self.opened.clone())?;
            return Ok(Undone::Blocked {
                disk: self.store.current()?,
            });
        }
        self.opened = previous;
        self.undo = None;
        self.baseline = None;
        self.revision += 1;
        // The restored file may embed other vault images than the edit did.
        self.refresh_display_files();
        Ok(Undone::Restored {
            revision: self.revision,
        })
    }

    /// Vault images for the file the editor will load next.
    fn refresh_display_files(&mut self) {
        let path = self.root.join(&self.rel);
        self.display_files = Scene::parse(&self.opened)
            .map(|scene| display_images(&self.root, &path, &scene))
            .unwrap_or_default();
    }

    fn saved(&mut self, previous: String, edited: Value) -> Applied {
        self.undo = Some(previous);
        self.baseline = Some(edited);
        self.revision += 1;
        Applied::Saved {
            revision: self.revision,
        }
    }

    fn conflicted(&mut self, mine: String, edited: &Value, disk: String) -> Applied {
        self.conflict = Some(Pending {
            mine,
            edited: edited.clone(),
            disk: disk.clone(),
            copy: None,
        });
        Applied::Conflict { disk }
    }

    fn check_revision(&self, revision: u64) -> Result<()> {
        ensure!(
            revision == self.revision,
            "The drawing changed since the editor loaded it. Load it again."
        );
        Ok(())
    }

    /// Images an Apply could not save are refused instead of written blank:
    /// a new image must carry inline raster data, and an inline image of the
    /// file cannot get new data under its old id (the writer keeps the file's).
    /// Vault images and formulas are written from `## Embedded Files`, never
    /// from the scene, so the editor's copy of them does not matter.
    fn check_images(&self, scene: &Value) -> Result<()> {
        let file = Scene::parse(&self.opened)?.json;
        let vault = vault_file_ids(&self.opened)?;
        let known: HashSet<&str> = elements(&file)
            .filter(|element| element["type"] == "image")
            .filter_map(|element| element["fileId"].as_str())
            .collect();
        for element in elements(scene)
            .filter(|element| element["type"] == "image" && element["isDeleted"] != true)
        {
            let id = element["fileId"]
                .as_str()
                .ok_or_else(|| anyhow!("An image is still loading. Wait, then Apply again."))?;
            if vault.contains(id) {
                continue;
            }
            let data = scene["files"][id]["dataURL"].as_str();
            if let Some(old) = file["files"][id]["dataURL"].as_str() {
                ensure!(
                    data.is_none_or(|data| data == old),
                    "An image changed under its old id. Insert it as a new image instead."
                );
                continue;
            }
            if known.contains(id) {
                // The file already shows this image without data; the edit
                // does not make that worse.
                continue;
            }
            let data = data
                .ok_or_else(|| anyhow!("An image is not loaded yet. Apply would save it blank."))?;
            ensure!(
                INLINE_IMAGES.iter().any(|prefix| data.starts_with(prefix)),
                "This image type cannot be saved in the drawing."
            );
        }
        Ok(())
    }

    /// `baseline` and `edited` in the file's terms. Text the editor got as its
    /// link source and still holds unchanged gets the file's resolved text
    /// back, in both scenes, so it is not an edit. Text the user changed keeps
    /// the source against which the change is judged.
    fn file_terms(&self, baseline: &Value, edited: &Value) -> Result<(Value, Value)> {
        let mut baseline = baseline.clone();
        let mut edited = edited.clone();
        if !is_markdown(&self.opened) {
            return Ok((baseline, edited));
        }
        let file = Scene::parse(&self.opened)?.json;
        let resolved: HashMap<&str, (&str, &Value)> = elements(&file)
            .filter_map(|element| {
                Some((element["id"].as_str()?, (linked_source(element)?, element)))
            })
            .collect();
        for (id, (source, file_element)) in resolved {
            let unchanged = elements(&edited)
                .find(|element| element["id"] == id)
                .is_some_and(|element| original_text(element) == Some(source));
            if !unchanged {
                continue;
            }
            for scene in [&mut baseline, &mut edited] {
                let Some(element) = elements_mut(scene).find(|element| element["id"] == id) else {
                    continue;
                };
                if original_text(element) != Some(source) {
                    continue;
                }
                for key in ["text", "originalText"] {
                    match file_element.get(key) {
                        Some(value) => element[key] = value.clone(),
                        None => {
                            if let Some(fields) = element.as_object_mut() {
                                fields.remove(key);
                            }
                        }
                    }
                }
            }
        }
        Ok((baseline, edited))
    }
}

fn elements(scene: &Value) -> impl Iterator<Item = &Value> {
    scene["elements"].as_array().into_iter().flatten()
}

fn elements_mut(scene: &mut Value) -> impl Iterator<Item = &mut Value> {
    scene
        .get_mut("elements")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
}

/// The unwrapped text the user typed (`originalText`, older scenes: `text`).
fn original_text(element: &Value) -> Option<&str> {
    element
        .get("originalText")
        .or_else(|| element.get("text"))
        .and_then(Value::as_str)
}

/// A text element's Markdown source when the file shows it resolved.
fn linked_source(element: &Value) -> Option<&str> {
    if element["type"] != "text" {
        return None;
    }
    let source = element.get("rawText")?.as_str()?;
    (Some(source) != original_text(element)).then_some(source)
}

/// `Board.excalidraw.md` → `Board.okilum-conflict-<time>-<tag>.excalidraw.md`,
/// so the copy is still a drawing. Same scheme as conflict copies of notes.
fn conflict_copy(rel: &Path) -> PathBuf {
    let name = rel
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let extension = [".excalidraw.md", ".excalidraw"]
        .into_iter()
        .find(|extension| lower.ends_with(extension))
        .map_or_else(
            || name.rfind('.').map_or(0, |dot| name.len() - dot),
            str::len,
        );
    let (stem, extension) = name.split_at(name.len() - extension);
    // A random tag, not a device identity.
    let tag: String = uuid::Uuid::new_v4()
        .as_bytes()
        .iter()
        .take(7)
        .map(|byte| b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"[(byte & 31) as usize] as char)
        .collect();
    let name = format!(
        "{stem}.okilum-conflict-{}-{tag}{extension}",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    // Syncthing prunes *.sync-conflict-*; an Okilum copy must not inherit that.
    rel.with_file_name(name.replace(".sync-conflict-", ".syncthing-conflict-"))
}

/// Vault raster images the drawing uses, inlined so the editor can show them.
/// Best effort, as in the renderer: an image that is missing, ambiguous,
/// outside the vault, not a raster or over the budget stays a placeholder.
/// Nothing is fetched remotely and no attachment is changed.
fn display_images(root: &Path, path: &Path, scene: &Scene) -> Map<String, Value> {
    use base64::Engine as _;
    use std::io::Read as _;
    let used: HashSet<&str> = elements(&scene.json)
        .filter(|element| element["type"] == "image" && element["isDeleted"] != true)
        .filter_map(|element| element["fileId"].as_str())
        .collect();
    let mut files = Map::new();
    let mut inventory = None;
    let mut total = 0;
    for (id, target) in &scene.embedded_files {
        if !used.contains(id.as_str()) {
            continue;
        }
        let Some(found) = resolve_image(root, path, target, &mut inventory) else {
            continue;
        };
        let mime = match found
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("webp") => "image/webp",
            Some("gif") => "image/gif",
            _ => continue,
        };
        let mut bytes = Vec::new();
        let budget = DISPLAY_IMAGE_BYTES - total;
        let read = std::fs::File::open(&found)
            .and_then(|file| file.take(budget as u64 + 1).read_to_end(&mut bytes));
        if read.is_err() || bytes.len() > budget {
            continue;
        }
        total += bytes.len();
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        files.insert(
            id.clone(),
            serde_json::json!({
                "id": id,
                "mimeType": mime,
                "dataURL": format!("data:{mime};base64,{data}"),
                "created": 0,
            }),
        );
    }
    files
}

/// The renderer's resolution order: next to the drawing, then from the vault
/// root, then a unique vault path ending in the target. An existing candidate
/// outside the vault is never swapped for another file of the same name.
fn resolve_image(
    root: &Path,
    path: &Path,
    target: &str,
    inventory: &mut Option<Option<crate::Vault>>,
) -> Option<PathBuf> {
    let explicit = target.starts_with("./") || target.starts_with("../");
    let mut candidates = vec![path.parent()?.join(target)];
    if !explicit {
        candidates.push(root.join(target));
    }
    for candidate in candidates {
        if let Ok(found) = candidate.canonicalize() {
            return (found.starts_with(root) && found.is_file()).then_some(found);
        }
    }
    if explicit {
        return None;
    }
    let vault = inventory
        .get_or_insert_with(|| crate::Vault::scan_metadata(root).ok())
        .as_ref()?;
    let suffix = format!("/{target}");
    let matches: Vec<_> = vault
        .entries
        .iter()
        .filter(|entry| entry.path == target || entry.path.ends_with(&suffix))
        .collect();
    let [entry] = matches.as_slice() else {
        return None;
    };
    root.join(&entry.path)
        .canonicalize()
        .ok()
        .filter(|found| found.starts_with(root) && found.is_file())
}

#[cfg(test)]
mod tests;
