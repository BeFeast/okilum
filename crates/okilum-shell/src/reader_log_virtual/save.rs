//! Explicit canonical publication through the Reader's revision-aware editor.
use super::model;
use anyhow::{ensure, Result};
use okilum_core::{
    file_editor::{FileEditor, Save},
    log::virtual_log::Definition,
};
use std::path::Path;

pub(super) struct Session {
    editor: FileEditor,
    original: String,
    review_required: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Saved,
    Unchanged,
    Conflict,
}

impl Session {
    pub fn open(loaded: &model::Loaded, state: &Path) -> Result<Self> {
        let editor = FileEditor::open(&loaded.source_path, &state.join("editor-drafts"))?;
        ensure!(!editor.dirty(), "A recovery draft already exists. Review it in Source before saving criteria; it has been preserved.");
        ensure!(
            editor.text() == loaded.source && editor.current()? == loaded.source,
            "Definition changed while loading. Load it again before saving."
        );
        Ok(Self {
            editor,
            original: loaded.source.clone(),
            review_required: false,
        })
    }

    pub fn save(&mut self, vault: &Path, definition: &Definition) -> Result<Outcome> {
        ensure!(!self.review_required, "The previous save needs review. Inspect the original and recovery in Source before retrying.");
        // Compare typed meaning, retaining every original byte for a no-op,
        // including whitespace, CRLF, JSON escapes and omitted optional fields.
        let unchanged = serde_json::to_value(Definition::parse(&self.original)?)?
            == serde_json::to_value(definition)?;
        let proposed = if unchanged {
            self.original.clone()
        } else {
            format!("{}\n", serde_json::to_string_pretty(definition)?)
        };
        Definition::parse(&proposed)?;
        // A hold refusal is certain non-publication. Leave the criteria in the
        // UI and allow retry, without manufacturing a blocking recovery draft.
        let _admission = if !unchanged {
            Some(okilum_write_authority::operation_for_path(
                self.editor.path(),
            )?)
        } else {
            None
        };
        if !unchanged || self.editor.dirty() {
            // Journal before validation/publication: conflict, missing source or
            // invalid query/glob must not discard the attempted criteria.
            self.editor.set_text(proposed.clone())?;
        }
        if self.editor.current()? != self.original {
            return Ok(Outcome::Conflict);
        }
        // Compile criteria only. Never enumerate files, start a watch or replace
        // the retained source-locked result as a side effect of Save.
        definition.plan(vault, self.editor.path(), 0)?;
        if unchanged {
            return Ok(Outcome::Unchanged);
        }
        match self.editor.save() {
            Ok(Save::Conflict) => Ok(Outcome::Conflict),
            Ok(Save::Saved) => {
                self.original = proposed;
                Ok(Outcome::Saved)
            }
            Err(error) => {
                // Some filesystem errors occur after publication. Do not claim
                // rollback or automatically retry an unconfirmed publication.
                self.review_required = true;
                Err(error.context("Save needs review; criteria and recovery retained"))
            }
        }
    }
}

#[cfg(test)]
#[path = "save/tests.rs"]
mod tests;
