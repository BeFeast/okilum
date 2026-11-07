//! The visible Manager is the retained native ProjectView on every desktop OS.
use super::*;

impl Reader {
    pub(super) fn render_manager_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = match &self.project {
            Some(project) => div().size_full().child(project.clone()).into_any_element(),
            None => div().p_4().child("Loading manager…").into_any_element(),
        };
        self.wrap_project_peek(content, cx)
    }
}
