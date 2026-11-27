//! Reader-coupled glue and Reader-integration tests for `project_terminal`.
use crate::{Context, Reader, Window};
use gpui_component::WindowExt as _;
pub(crate) use okilum_shell_project::project_terminal::*;

#[cfg(test)]
mod tests;

impl Reader {
    pub(super) fn open_project_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(project) = self.project_navigation.active.clone() {
            open(&project, window, cx);
        } else {
            window.push_notification("Choose a project before opening its terminal", cx);
        }
    }
}
