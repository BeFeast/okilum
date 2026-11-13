//! Real Reader exercise; no GPUI test-support or alternate editor/save path.
use super::*;

impl Reader {
    pub(crate) fn file_editor_native_step(
        &mut self,
        stage: usize,
        original: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let input = self.source_input().expect("native file Edit mounted");
        let file = self
            .editing
            .as_ref()
            .unwrap()
            .file_chrome
            .clone()
            .expect("file chrome mounted");
        let target = file.read(cx).find.clone();
        match stage {
            0 => {
                let state = file.read(cx);
                if state.current.is_none() {
                    return false;
                }
                assert!(matches!(
                    &state.current.as_ref().unwrap().verdict,
                    Ok(Ok(()))
                ));
                let chrome = self.file_editor_chrome(cx, true).unwrap();
                assert!(chrome.find.is_some());
                assert!(chrome
                    .actions
                    .iter()
                    .any(|a| a.id == "file-edit-format" && a.never_dropped));
                assert!(chrome
                    .views
                    .iter()
                    .all(|v| v.action.enabled == (v.action.id == "code-view-raw")));
                let find = EditorTarget::target(&target, cx);
                find.set_query("gateway", window, cx);
            }
            1 => {
                if target.read(cx).count(cx).1 == 0 {
                    return false;
                }
                assert!(input.read(cx).search_session().external_presentation);
                let len = input.read(cx).text().len();
                input.update(cx, |i, cx| {
                    i.replace_source_range(0..len, "{ service: \"gateway\" }", window, cx)
                });
            }
            2 => {
                let state = file.read(cx);
                if state
                    .current
                    .as_ref()
                    .is_none_or(|s| s.source.stamp != input.read(cx).source_stamp())
                {
                    return false;
                }
                assert!(
                    state.current.as_ref().unwrap().diagnostic.is_some(),
                    "live strict verdict"
                );
                self.code_operation(Action::Repair, window, cx);
            }
            3 => {
                if self.editing.as_ref().unwrap().operations.busy {
                    return false;
                }
                let preview = self
                    .editing
                    .as_ref()
                    .unwrap()
                    .operations
                    .preview
                    .as_ref()
                    .expect("real lower Repair sheet prepared");
                assert_eq!(preview.proposed, "{ \"service\": \"gateway\" }");
                assert_eq!(input.read(cx).value().as_ref(), "{ service: \"gateway\" }");
                assert_eq!(
                    std::fs::read_to_string(self.vault_root.join(self.selected_file())).unwrap(),
                    original,
                    "Repair preview did not write"
                );
                self.apply_code_repair(window, cx);
                assert_eq!(
                    input.read(cx).value().as_ref(),
                    "{ \"service\": \"gateway\" }"
                );
                window.dispatch_action(Box::new(gpui_component::input::Undo), cx);
            }
            4 => {
                assert_eq!(
                    input.read(cx).value().as_ref(),
                    "{ service: \"gateway\" }",
                    "one Undo retires the whole repair"
                );
                window.dispatch_action(Box::new(gpui_component::input::Undo), cx);
            }
            5 => {
                assert_eq!(
                    input.read(cx).value().as_ref(),
                    original,
                    "second Undo restores the synthetic fixture"
                );
                assert!(self.save_source(cx));
                self.toggle_source(window, cx);
                assert!(self.editing.is_none());
            }
            _ => unreachable!(),
        }
        diagnostic_println!("FILE_EDIT_NATIVE stage={stage} mounted=Reader");
        true
    }
}
