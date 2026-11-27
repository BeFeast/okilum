//! Reader glue for offline Excalidraw editor (#478, #3238).
use super::*;
pub(crate) use okilum_shell_viewers::reader_drawing_edit::*;

impl Reader {
    /// The explicit Edit drawing command. A drawing that is already being
    /// edited gets its existing page again instead of a second session.
    pub(super) fn edit_drawing(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_edit_drawing(
            self.vault_root.clone(),
            self.session_directory.clone(),
            path,
            window,
            cx,
        );
    }
}
