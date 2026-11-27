//! Reader glue, native harnesses, and Reader-integration tests for code files (#998, #3238).
use super::*;
use gpui_component::input::EditorState;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use okilum_shell_viewers::reader_code_file::Surface;
pub(crate) use okilum_shell_viewers::reader_code_file::*;
#[allow(unused_imports)]
pub(crate) use okilum_shell_viewers::reader_code_file::{
    bind_keys, structure, Availability, TreeDown, TreeLeft, TreeRight, TreeUp, View,
};
use std::time::Instant;

#[cfg(test)]
mod find_review_tests;
#[cfg(test)]
mod find_tests;
#[cfg(test)]
mod fold_all_scroll_tests;
#[cfg(test)]
mod loading_tests;
#[cfg(feature = "data-view-native-harness")]
pub(crate) mod native_harness;
#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod review_tests;
#[cfg(test)]
mod schema_tests;
#[cfg(test)]
mod structure_tests;
#[cfg(feature = "data-view-native-harness")]
mod toolbar_native;
#[cfg(test)]
mod view_tests;

impl Reader {
    #[inline(never)]
    pub(crate) fn code_action_listeners(&self, view: Div, cx: &mut Context<Self>) -> Div {
        if self.read_code_view().is_none() {
            return view;
        }
        view.on_action(cx.listener(|this, _: &ShowTable, window, cx| {
            this.change_code_view(View::Table, window, cx)
        }))
        .on_action(cx.listener(|this, _: &ShowTree, window, cx| {
            this.change_code_view(View::Tree, window, cx)
        }))
        .on_action(cx.listener(|this, _: &ShowFormatted, window, cx| {
            this.change_code_view(View::Formatted, window, cx)
        }))
        .on_action(
            cx.listener(|this, _: &ShowRaw, window, cx| {
                this.change_code_view(View::Raw, window, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &FoldAll, _, cx| this.fold_code_view(true, cx)))
        .on_action(cx.listener(|this, _: &UnfoldAll, _, cx| this.fold_code_view(false, cx)))
        .on_action(cx.listener(|this, _: &CopyNode, window, cx| {
            if let Some(code) = this.read_code_view() {
                code.update(cx, |code, cx| code.copy_node(window, cx));
            }
        }))
        .on_action(cx.listener(|this, _: &CopySelection, _, cx| {
            if let Some(code) = this.read_code_view() {
                code.update(cx, |code, cx| code.copy_selection(cx));
            }
        }))
        .on_action(cx.listener(|this, _: &GoToError, window, cx| {
            if this.pane.editing.is_some() {
                this.file_editor_go_to_error(window, cx);
                return;
            }
            if let Some(code) = this.read_code_view() {
                code.update(cx, |code, cx| code.go_to_error(window, cx));
            }
        }))
        .on_action(cx.listener(|this, _: &PreviousPart, window, cx| {
            if let Some(code) = this.read_code_view() {
                code.update(cx, |code, cx| code.step_part(false, window, cx));
            }
        }))
        .on_action(cx.listener(|this, _: &NextPart, window, cx| {
            if let Some(code) = this.read_code_view() {
                code.update(cx, |code, cx| code.step_part(true, window, cx));
            }
        }))
    }
    pub(super) fn resume_code_presentation(&self, cx: &mut App) {
        if let Some(code) = self
            .pane
            .file_preview
            .as_ref()
            .and_then(|preview| preview.code.as_ref())
        {
            let resume_find = code.update(cx, |code, cx| {
                let suspended = !code.read_active;
                code.read_active = true;
                if suspended && code.view == View::Table {
                    if let Some(table) = &code.table {
                        table.update(cx, |table, cx| table.resume_work(cx));
                    }
                }
                if let Some(input) = code.input() {
                    code.apply_diagnostics(&input, cx);
                }
                if suspended && !code.whole_find() && code.view != View::Table {
                    if let Some(editor) = &code.editor_find {
                        editor.update(cx, |editor, cx| editor.resume(cx));
                    }
                }
                cx.notify();
                let query = code.find.read(cx).value();
                let current = code
                    .search_key
                    .as_ref()
                    .is_some_and(|(revision, old, case)| {
                        *revision == code.document_revision
                            && old.as_str() == query.as_ref()
                            && *case == code.case_sensitive.get()
                    });
                suspended && code.whole_find() && !query.is_empty() && !current
            });
            if resume_find {
                let code = code.clone();
                let window = self.reader_window;
                // Edit exits without a Window also use this path. Re-enter
                // the owning window only after its current lease is released.
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, cx| {
                        code.update(cx, |code, cx| {
                            if code.read_active {
                                code.search(window, cx);
                            }
                        });
                    });
                });
            }
        }
    }
    fn read_code_view(&self) -> Option<Entity<CodePreview>> {
        if self.pane.editing.is_some() {
            return None;
        }
        self.pane.file_preview.as_ref()?.code.clone()
    }
    fn change_code_view(&self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(code) = self.read_code_view() {
            code.update(cx, |code, cx| code.set_view(view, window, cx));
        }
    }
    fn fold_code_view(&self, folded: bool, cx: &mut Context<Self>) {
        if let Some(code) = self.read_code_view() {
            code.update(cx, |code, cx| code.fold_all(folded, cx));
        }
    }
}

impl Reader {
    /// Native regression tests inspect the loaded read-only editor directly.
    #[cfg(test)]
    pub(crate) fn code_view_input(&self, cx: &App) -> Option<Entity<EditorState>> {
        self.pane
            .file_preview
            .as_ref()?
            .code
            .as_ref()?
            .read(cx)
            .input()
    }

    pub(crate) fn file_is_source(&self) -> bool {
        self.pane
            .file_preview
            .as_ref()
            .is_some_and(|p| reader_delimited::editable(&p.rel) || p.code.is_some())
    }

    pub(crate) fn selected_json_association(&self, cx: &App) -> bool {
        self.pane
            .file_preview
            .as_ref()
            .and_then(|p| p.code.as_ref())
            .is_some_and(|code| code.read(cx).json_routed)
    }

    pub(crate) fn selected_code_language(&self, cx: &App) -> Option<&'static str> {
        self.pane
            .file_preview
            .as_ref()
            .and_then(|p| p.code.as_ref())
            .map(|code| code.read(cx).source_language())
            .or_else(|| language(self.selected_file()))
    }

    /// Code files open in the editor only within the size cap.
    pub(crate) fn code_file_editable(&self, cx: &App) -> bool {
        self.pane
            .file_preview
            .as_ref()
            .and_then(|preview| preview.code.as_ref())
            .is_none_or(|code| code.read(cx).editable())
    }

    /// ⌥Z / Alt+Z (#998): remembered for every code file and window.
    pub(crate) fn toggle_code_soft_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wrap = !reader_ui_state::code_soft_wrap(cx);
        reader_ui_state::set_code_soft_wrap(wrap, cx);
        let Some(preview) = &self.pane.file_preview else {
            cx.notify();
            return;
        };
        let shown = preview.code.as_ref().and_then(|code| code.read(cx).input());
        let edited = (eligible(&preview.rel) || preview.code.is_some())
            .then(|| self.source_input())
            .flatten();
        for input in shown.into_iter().chain(edited) {
            input.update(cx, |input, cx| input.set_soft_wrap(wrap, window, cx));
        }
        cx.notify();
    }
}
