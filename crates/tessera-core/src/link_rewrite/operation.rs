//! Durable preimages precede mutations; recovery compares bytes before reverting.
use super::*;
use crate::{
    file_editor::{EditorLock, FileEditor, Save},
    note_move::MovePlan,
};
use std::{fs::File, io::Write, path::PathBuf};

#[derive(Debug, Serialize, Deserialize)]
pub struct Operation {
    pub root: PathBuf,
    pub from: String,
    pub to: String,
    pub files: BTreeMap<String, Versions>,
    pub complete: bool,
    pub reverted: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Versions {
    pub before: String,
    pub after: String,
}
pub struct RecoveryList {
    pub operations: Vec<PathBuf>,
    pub warnings: Vec<String>,
}
pub struct Applied {
    pub journal: PathBuf,
    pub moved: bool,
    pub warning: Option<String>,
}
impl Operation {
    fn persist(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("Missing operation directory")?;
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(&mut file, self)?;
        file.flush()?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|e| e.error)?;
        File::open(parent)?.sync_all()?;
        if let Some(directory) = parent.parent() {
            File::open(directory)?.sync_all()?;
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let operation: Self = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            operation.root.is_absolute()
                && operation.from != operation.to
                && operation.files.contains_key(&operation.from),
            "Invalid recovery operation identity"
        );
        for name in operation
            .files
            .keys()
            .chain([&operation.from, &operation.to])
        {
            let name = Path::new(name);
            ensure!(
                !name.as_os_str().is_empty()
                    && name
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_)))
                    && name
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("md")),
                "Invalid recovery note path"
            );
        }
        Ok(operation)
    }
    /// All retained operations are discoverable, including a crash before the
    /// completion acknowledgement. Preimages are never removed automatically.
    pub fn list(state: &Path, root: &Path) -> Result<RecoveryList> {
        let directory = state.join("link-moves");
        if !directory.exists() {
            return Ok(RecoveryList {
                operations: vec![],
                warnings: vec![],
            });
        }
        let root = root.canonicalize().unwrap_or_else(|_| root.to_owned());
        let mut result = vec![];
        let mut warnings = vec![];
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let operation = match Self::load(&path) {
                    Ok(operation) => operation,
                    Err(error) => {
                        warnings.push(format!(
                            "Unreadable recovery {}: {error:#}. The file is retained.",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ));
                        continue;
                    }
                };
                if operation
                    .root
                    .canonicalize()
                    .unwrap_or_else(|_| operation.root.clone())
                    == root
                    && !operation.reverted
                {
                    result.push(path);
                }
            }
        }
        result.sort();
        Ok(RecoveryList {
            operations: result,
            warnings,
        })
    }
    /// Refuses diverged files; no rollback ever overwrites later user edits.
    /// A second invocation can resume a partially completed revert.
    pub(crate) fn lock(path: &Path) -> Result<EditorLock> {
        EditorLock::acquire(
            &path
                .parent()
                .context("Missing operation folder")?
                .join(".recovery.lock"),
        )
    }
    pub fn revert(path: &Path, state: &Path) -> Result<()> {
        let _guard = Self::lock(path)?;
        let mut operation = Self::load(path)?;
        ensure!(!operation.reverted, "This operation was already reverted");
        let moved = !operation.root.join(&operation.from).exists()
            && operation.root.join(&operation.to).exists();
        ensure!(!(operation.root.join(&operation.from).exists() && operation.root.join(&operation.to).exists()), "Both source and destination exist; recovery will not guess which belongs to the operation");
        let mut editors = BTreeMap::new();
        for (original, versions) in &operation.files {
            let current = if moved && original == &operation.from {
                &operation.to
            } else {
                original
            };
            let mut editor =
                FileEditor::open(&operation.root.join(current), &state.join("editor-drafts"))?;
            ensure!(
                !editor.dirty()
                    || editor.text() == versions.after
                    || editor.text() == versions.before,
                "{current} has an unrelated unsaved draft; resolve it before reverting"
            );
            let bytes = editor.current()?;
            ensure!(bytes==versions.before || bytes==versions.after,"{current} changed since the move; its preimage is retained, but it cannot be reverted automatically");
            if editor.dirty() {
                editor.reload()?;
            }
            editors.insert(original.clone(), editor);
        }
        let _destination = if moved {
            Some(FileEditor::reserve_destination(
                &operation.root.join(&operation.from),
                &state.join("editor-drafts"),
            )?)
        } else {
            None
        };
        // A failed/partial revert is protected from retention until completed.
        operation.complete = false;
        operation.persist(path)?;
        // Restore links before moving back, just as forward application moves last.
        for (original, editor) in &mut editors {
            let versions = &operation.files[original];
            if editor.current()? == versions.before {
                continue;
            }
            editor.set_text(crate::source_history::move_preimage(&operation, original)?)?;
            ensure!(
                editor.save()? == Save::Saved,
                "{original} changed while reverting; recovery remains available"
            );
        }
        if moved {
            let moved = MovePlan::prepare_exact(
                &operation.root,
                Path::new(&operation.to),
                Path::new(&operation.from),
                operation.files[&operation.from].before.as_bytes(),
            )?
            .commit()?;
            ensure!(
                moved.warning.is_none(),
                "Revert moved the file, but verification requires attention: {}",
                moved.warning.unwrap_or_default()
            );
        }
        operation.reverted = true;
        operation.persist(path)
    }
}
impl Preview {
    /// `open` holds clean native editor locks without replacing their in-memory
    /// input until application completes. Other affected paths are locked here.
    pub fn apply(
        &self,
        root: &Path,
        state: &Path,
        open: &mut BTreeMap<String, &mut FileEditor>,
    ) -> Result<Applied> {
        self.apply_with(root, state, open, |_| Ok(()))
    }
    fn apply_with(
        &self,
        root: &Path,
        state: &Path,
        open: &mut BTreeMap<String, &mut FileEditor>,
        mut before_file: impl FnMut(&str) -> Result<()>,
    ) -> Result<Applied> {
        let _destination =
            FileEditor::reserve_destination(&root.join(&self.to), &state.join("editor-drafts"))?;
        let mut owned = BTreeMap::new();
        for path in self.affected_paths() {
            if !open.contains_key(&path) {
                owned.insert(
                    path.clone(),
                    FileEditor::open(&root.join(&path), &state.join("editor-drafts"))?,
                );
            }
        }
        let mut editors: BTreeMap<String, &mut FileEditor> =
            owned.iter_mut().map(|(p, e)| (p.clone(), e)).collect();
        for (path, editor) in open.iter_mut() {
            if self.snapshots.contains_key(path) {
                editors.insert(path.clone(), *editor);
            }
        }
        for (path, editor) in &editors {
            ensure!(
                !editor.dirty() && editor.text() == self.snapshots[path],
                "{path} has unsaved or stale editor text; reload/save and preview again"
            );
        }
        self.validate(root)?;
        let mut operation = Operation {
            root: root.canonicalize()?,
            from: self.from.clone(),
            to: self.to.clone(),
            files: BTreeMap::new(),
            complete: false,
            reverted: false,
        };
        for path in self.affected_paths() {
            operation.files.insert(
                path.clone(),
                Versions {
                    before: self.snapshots[&path].clone(),
                    after: self.rewritten(&path)?,
                },
            );
        }
        let journal = state.join("link-moves").join(format!(
            "{}-{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis(),
            uuid::Uuid::new_v4()
        ));
        operation.persist(&journal)?;
        let result = (|| -> Result<_> {
            for (path, versions) in &operation.files {
                before_file(path)?;
                let editor = editors.get_mut(path).context("Missing affected editor")?;
                ensure!(
                    editor.current()? == versions.before,
                    "{path} changed during application"
                );
                if versions.before != versions.after {
                    editor.set_text(versions.after.clone())?;
                    ensure!(
                        editor.save()? == Save::Saved,
                        "{path} changed during application"
                    );
                }
            }
            // Fresh inode after source rewrite, but still the exact approved bytes.
            ensure!(
                fs::read(root.join(&self.from))? == operation.files[&self.from].after.as_bytes(),
                "Moved note changed during application"
            );
            let moved = MovePlan::prepare_exact(
                root,
                Path::new(&self.from),
                Path::new(&self.to),
                operation.files[&self.from].after.as_bytes(),
            )?
            .commit()?;
            Ok(moved.warning)
        })();
        match result {
            Ok(warning) => {
                operation.complete = warning.is_none();
                let persist = operation.persist(&journal);
                Ok(Applied {
                    journal,
                    moved: true,
                    warning: warning.or_else(|| {
                        persist.err().map(|e| {
                            format!("Move completed; recovery acknowledgement failed: {e}")
                        })
                    }),
                })
            }
            Err(error) => {
                // Do not auto-revert over a racing writer. The retained operation
                // records all before/after bytes even if an acknowledgement failed.
                let applied: Vec<_> = operation
                    .files
                    .iter()
                    .filter(|(p, v)| {
                        fs::read(root.join(p))
                            .ok()
                            .is_some_and(|b| b == v.after.as_bytes() && v.before != v.after)
                    })
                    .map(|(p, _)| p.as_str())
                    .collect();
                Ok(Applied { journal, moved:false, warning:Some(format!("Move interrupted: {error:#}. Applied files: {}. Use Recover link moves to inspect/revert; original bytes are retained.",if applied.is_empty(){"none".into()}else{applied.join(", ")})) })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_apply_is_discoverable_and_revertible_after_restart() {
        let fixture = super::super::tests::fixture();
        let root = fixture.path().canonicalize().unwrap();
        let state = tempfile::tempdir().unwrap();
        let preview = Preview::prepare(&root, "Old/Заметка 🧠.md", "Новое.md").unwrap();
        let mut count = 0;
        let applied = preview
            .apply_with(&root, state.path(), &mut BTreeMap::new(), |_| {
                count += 1;
                if count == 2 {
                    bail!("Injected write failure");
                }
                Ok(())
            })
            .unwrap();
        assert!(!applied.moved);
        assert!(applied.warning.unwrap().contains("Old/Заметка 🧠.md"));
        assert_ne!(
            fs::read_to_string(root.join(&preview.from)).unwrap(),
            preview.snapshots[&preview.from]
        );
        assert_eq!(
            Operation::list(state.path(), &root).unwrap().operations,
            vec![applied.journal.clone()]
        );
        Operation::revert(&applied.journal, state.path()).unwrap();
        for (path, source) in preview.snapshots {
            assert_eq!(fs::read_to_string(root.join(path)).unwrap(), source);
        }
        assert!(Operation::list(state.path(), &root)
            .unwrap()
            .operations
            .is_empty());
    }
    #[test]
    fn discovery_keeps_valid_operations_when_another_journal_is_corrupt() {
        let fixture = super::super::tests::fixture();
        let root = fixture.path().canonicalize().unwrap();
        let state = tempfile::tempdir().unwrap();
        let p = Preview::prepare(&root, "Old/Заметка 🧠.md", "Новое.md").unwrap();
        let applied = p.apply(&root, state.path(), &mut BTreeMap::new()).unwrap();
        assert!(applied.moved);
        fs::write(state.path().join("link-moves/bad.json"), "garbage").unwrap();
        let found = Operation::list(state.path(), &root.join("Old/..")).unwrap();
        assert_eq!(found.operations, vec![applied.journal]);
        assert_eq!(found.warnings.len(), 1);
    }
}
