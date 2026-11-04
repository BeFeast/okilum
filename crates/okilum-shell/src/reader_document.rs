//! Pane-owned document storage (#773). No vault, watcher or editor is cloned.
#![allow(dead_code, reason = "#773 model, not wired into Reader yet")]
use std::path::{Component, PathBuf};

/// Identity must come from normal Reader resolution, not from a link's raw text.
/// The root is the already canonicalized vault root, shared with FileEditor.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(super) struct Identity {
    root: PathBuf,
    relative: String,
}

impl Identity {
    pub(super) fn new(root: PathBuf, relative: String) -> Option<Self> {
        if !root.is_absolute()
            || relative.is_empty()
            || relative.contains(['\\', '\0'])
            || relative
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !std::path::Path::new(&relative)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return None;
        }
        Some(Self { root, relative })
    }

    pub(super) fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub(super) fn relative(&self) -> &str {
        &self.relative
    }
}

/// These flags describe an offer; the durable draft remains in FileEditor storage.
#[derive(Default)]
pub(super) struct Recovery {
    pub offer: bool,
    pub checked: bool,
    pub error: bool,
    pub dismissed: bool,
    pub startup: bool,
}

/// `S` is the existing Reader document surface, moved, never cloned: rendered
/// entities/subscriptions, reader_navigation::State, mode, Find, caret/selection,
/// surface-specific scroll, prepared links and load/recovery presentation.
/// `E` is the existing reader_editor::Editing handle, with its FileEditor lock.
/// Keeping these generic lets the ownership/exit rules run without a native UI.
pub(super) struct Document<S, E> {
    identity: Identity,
    pub surface: S,
    pub recovery: Recovery,
    editor: Option<E>,
}

impl<S, E> Document<S, E> {
    pub(super) fn new(identity: Identity, surface: S) -> Self {
        Self {
            identity,
            surface,
            recovery: Recovery::default(),
            editor: None,
        }
    }

    pub(super) fn identity(&self) -> &Identity {
        &self.identity
    }

    pub(super) fn editor(&self) -> Option<&E> {
        self.editor.as_ref()
    }

    pub(super) fn editor_mut(&mut self) -> Option<&mut E> {
        self.editor.as_mut()
    }

    // Only the host may install an editor, after checking all pane owners.
    pub(super) fn install_editor(&mut self, editor: E) {
        assert!(self.editor.is_none());
        self.editor = Some(editor);
    }

    /// The caller must use the existing save_source_for_navigation lifecycle.
    /// A boolean "dirty" check or a merely queued recovery write is insufficient.
    /// On error the handle, accepted source and recovery offer remain owned here.
    pub(super) fn leave_editor<G>(
        &mut self,
        guard: impl FnOnce(&mut Self) -> Result<(), G>,
    ) -> Result<(), G> {
        guard(self)?;
        self.editor = None;
        Ok(())
    }

    pub(super) fn accept(&mut self, identity: Identity, apply: impl FnOnce(&mut S)) {
        assert!(self.editor.is_none());
        apply(&mut self.surface);
        self.identity = identity;
        self.recovery = Recovery::default();
    }
}

/// Prepared note plus the existing lossless editor. This is the non-visual
/// ownership boundary; mounting a GPUI surface must move, rather than copy, its
/// editor into `reader_editor::Editing`.
#[cfg(any(unix, windows))]
pub(super) type Loaded =
    Document<crate::prepared_links::PreparedDocument, okilum_core::file_editor::FileEditor>;

#[cfg(any(unix, windows))]
impl Loaded {
    /// Use the same checked renderer as Reader. A failed read never takes an
    /// accepted document or an editor from a pane. The caller publishes with its
    /// LoadTicket only after this worker completes.
    pub(super) fn read(vault: &okilum_core::Vault, identity: Identity) -> anyhow::Result<Self> {
        anyhow::ensure!(
            vault.root == identity.root,
            "Document preparation belongs to another vault"
        );
        crate::reader_files::checked_path(identity.root(), identity.relative())?;
        let surface = okilum_core::render::reader_document(vault, identity.relative())?;
        Ok(Self::new(identity, surface.into()))
    }
}
