//! Reader glue for «Open in ▸» (#873, #1400, #3234).
use super::*;
pub(crate) use okilum_shell_platform::open_in::*;

/// The caret line (1-based) of a Reader editing this note, after saving it.
pub(crate) fn prepare_open_note(
    root: &Path,
    rel: &str,
    cx: &mut App,
) -> anyhow::Result<Option<usize>> {
    for reader in reader_open::readers(cx) {
        let line = reader.update(cx, |this, cx| this.prepare_open_in(root, rel, cx))?;
        if line.is_some() {
            return Ok(line);
        }
    }
    Ok(None)
}

impl Reader {
    /// [`OpenInDefaultEditor`]: the open file in the default editor. Deferred,
    /// because saving the draft first updates this Reader.
    pub(crate) fn open_in_default_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rel = self.selected_file().to_owned();
        if rel.is_empty() {
            return;
        }
        let Some(editor) = preferred(cx) else {
            reader_toast::error("No code editor is installed", window, cx);
            return;
        };
        let root = self.vault_root.clone();
        window.defer(cx, move |window, cx| open(editor, &root, &rel, window, cx));
    }

    /// Save a dirty draft of `rel` and report its caret line; `Ok(None)` when
    /// this Reader is not editing that note.
    pub(super) fn prepare_open_in(
        &mut self,
        root: &Path,
        rel: &str,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Option<usize>> {
        if self.vault_root != root || self.pane.current_rel != rel {
            return Ok(None);
        }
        let Some(line) = self.editing_caret_line(cx) else {
            return Ok(None);
        };
        anyhow::ensure!(
            self.save_source(cx),
            "the note could not be saved; it was not opened"
        );
        Ok(Some(line))
    }
}
