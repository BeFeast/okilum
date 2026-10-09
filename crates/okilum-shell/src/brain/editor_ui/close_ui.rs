//! Normal managed BrainView close waits for the latest acknowledged local draft.
//! The application-owned Quit coordinator reuses the same exact-generation barrier.
use super::*;

pub(super) struct CloseIntent {
    workspace: Value,
    base: Value,
    text: String,
    draft_id: Option<String>,
    allowed_save: Option<Value>,
    allowed_adoption: bool,
}

impl BrainView {
    pub(in crate::brain) fn editor_closing(&self) -> bool {
        self.editor.closing.is_some() || self.app_quit_token.is_some()
    }

    fn close_matches(&self, intent: &CloseIntent, cx: &App) -> bool {
        self.expected_workspace.as_ref() == Some(&intent.workspace)
            && self.source_snapshot.as_ref() == Some(&intent.base)
            && self.source.value(cx).as_ref() == intent.text
            && match (&intent.draft_id, &self.editor.active) {
                (Some(expected), Some(active)) => active.id == *expected,
                (Some(_), None) => self.source_original == intent.text,
                (None, _) => true,
            }
    }
    fn close_durable(&self, cx: &App) -> bool {
        self.editor.guarded_retry.is_none()
            && !self.editor.flight
            && self.editor.save.is_none()
            && self.editor.sending.is_none()
            && self.editor.error.is_none()
            && match &self.editor.active {
                Some(draft) => {
                    self.editor.confirmed
                        && self.source_snapshot.as_ref() == Some(&draft.base)
                        && draft.text == self.source.value(cx).as_ref()
                }
                None => self.source.value(cx).as_ref() == self.source_original,
            }
    }
    pub(in crate::brain) fn editor_request_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.app_quit_token.is_some() {
            return false;
        }
        if self.note_request_close(cx) {
            return false;
        }
        self.editor_begin_close(window, cx)
    }

    pub(in crate::brain) fn editor_request_app_quit(
        &mut self,
        token: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.app_quit_token = Some(token);
        self.sync_source_policy(cx);
        self.app_quit_ready = false;
        if !self.note_request_close(cx) && self.editor_begin_close(window, cx) {
            self.app_quit_ready = true;
        }
        cx.notify();
    }

    pub(in crate::brain) fn editor_app_quit_ready(&self, token: u64, cx: &App) -> bool {
        !self.discussion_note.active()
            && !self.goal_criteria.active
            && !self.decision_reuse.active
            && self.app_quit_token == Some(token)
            && self.app_quit_ready
            && match &self.editor.closing {
                Some(intent) => self.close_matches(intent, cx) && self.close_durable(cx),
                None => {
                    !self.editor_enabled()
                        || !self.source_editable
                        || self.editor.discarding
                        || self.close_durable(cx)
                }
            }
    }

    fn editor_begin_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // A pending explicit discard has already chosen to retire this local text.
        // It freezes typing; closing may leave the old record, but cannot lose new typing.
        if !self.editor_enabled() || !self.source_editable || self.editor.discarding {
            return true;
        }
        if self.editor.closing.is_none() {
            // A prior acknowledgement is not a lease on a shared recovery record.
            // Reconfirm an active generation even when its visible text is unchanged.
            if self.editor.active.is_none() && self.close_durable(cx) {
                return true;
            }
            let (Some(workspace), Some(base)) = (
                self.expected_workspace.clone(),
                self.source_snapshot.clone(),
            ) else {
                return true;
            };
            self.editor.closing = Some(CloseIntent {
                workspace,
                base,
                text: self.source.value(cx).to_string(),
                draft_id: self.editor.active.as_ref().map(|draft| draft.id.clone()),
                allowed_save: self
                    .editor
                    .sending
                    .clone()
                    .or_else(|| self.editor.save.clone()),
                allowed_adoption: self.merge_adoption_pending(),
            });
            self.editor.confirmed = false;
            self.surface = Surface::Source;
            self.show_capture = false;
        }
        self.sync_source_policy(cx);
        self.editor_protect(window, cx);
        cx.notify();
        // Completion removes only this exact window; repeated requests retain one intent.
        false
    }
    pub(super) fn editor_finish_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(intent) = self.editor.closing.as_ref() else {
            return false;
        };
        if !self.close_matches(intent, cx) {
            self.editor.error=Some("The editor changed while closing. Keep editing to inspect the retained draft; this window stays open.".into());
            return false;
        }
        if !self.close_durable(cx) {
            return false;
        }
        if let Some(token) = self.app_quit_token {
            self.app_quit_ready = true;
            super::super::app_quit::queue_finish(token, cx);
        } else {
            window.remove_window();
        }
        true
    }
    pub(super) fn editor_close_saved(&mut self, request: &Value, base: &Value, cx: &App) {
        let Some(intent) = self.editor.closing.as_ref() else {
            return;
        };
        if intent.allowed_save.as_ref() != Some(request) || !self.close_matches(intent, cx) {
            self.editor.error=Some("Save completion did not match the pending close. Keep editing to inspect recovery.".into());
            return;
        }
        let intent = self.editor.closing.as_mut().unwrap();
        intent.base = base.clone();
        intent.allowed_save = None;
    }
    pub(super) fn editor_close_auto_saved(
        &mut self,
        original: &Value,
        child: &Value,
        base: &Value,
        merged: &str,
        cx: &App,
    ) -> bool {
        let Some(intent) = self.editor.closing.as_ref() else {
            return true;
        };
        if !self.close_matches(intent, cx)
            || (intent.allowed_save.as_ref() != Some(original)
                && intent.allowed_save.as_ref() != Some(child))
        {
            self.editor.error = Some(
                "Automatic Save did not match the pending close; recovery remains retained.".into(),
            );
            return false;
        }
        let intent = self.editor.closing.as_mut().unwrap();
        intent.base = base.clone();
        intent.text = merged.into();
        intent.allowed_save = None;
        true
    }
    pub(super) fn editor_close_adopted(&mut self, expected: &Draft, adopted: &Draft, cx: &App) {
        let Some(intent) = self.editor.closing.as_ref() else {
            return;
        };
        if !intent.allowed_adoption
            || !self.close_matches(intent, cx)
            || intent.base != expected.base
            || intent.text != expected.text
            || expected.id != adopted.id
        {
            self.editor.error = Some(
                "Merge adoption did not match the pending close. Keep editing to inspect recovery."
                    .into(),
            );
            return;
        }
        let intent = self.editor.closing.as_mut().unwrap();
        intent.text = adopted.text.clone();
        intent.allowed_adoption = false;
    }
    pub(super) fn editor_close_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel =
            v_flex()
                .gap_2()
                .child(div().text_sm().child(if self.app_quit_token.is_some() {
                    "All windows stay open until every latest draft is protected for Quit."
                } else if self.editor.error.is_some() {
                    "This window stays open until the latest draft is protected."
                } else {
                    "Protecting the latest draft before closing this window…"
                }));
        if let Some(error) = &self.editor.error {
            panel = panel.child(div().text_sm().child(error.clone()));
        }
        panel
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        super::super::super::brand::control("editor-close-keep-editing", cx)
                            .label("Keep editing")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editor_keep_editing(cx);
                            })),
                    )
                    .child(
                        super::super::super::brand::control("editor-close-retry", cx)
                            .label("Retry protection")
                            .disabled(self.editor.flight || self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.editor_retry_close_protection(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
    pub(in crate::brain) fn editor_cancel_app_quit(&mut self, token: u64, cx: &mut Context<Self>) {
        if self.app_quit_token == Some(token) {
            self.app_quit_token = None;
            self.discussion_note.closing = false;
            self.discussion_note.destination = None;
            self.app_quit_ready = false;
            self.editor.closing = None;
            self.sync_source_policy(cx);
            cx.notify();
        }
    }
    fn editor_keep_editing(&mut self, cx: &mut Context<Self>) {
        if let Some(token) = self.app_quit_token {
            super::super::app_quit::queue_cancel(token, cx);
        } else {
            self.editor.closing = None;
            self.sync_source_policy(cx);
            cx.notify();
        }
    }
    fn editor_retry_close_protection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editor_closing() || self.editor.flight || self.busy {
            return;
        }
        self.editor.error = None;
        self.editor.confirmed = false;
        // Re-list and compare the same generation. Never replay an uncertain Save.
        self.editor_list(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;
    fn fixture() -> (PathBuf, Value, Value, EditorRecovery) {
        let dir = std::env::temp_dir().join(format!("okilum-close-ui-{}", uuid()));
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/close-ui","records_dir":"records","managed":true});
        let bytes = "\u{feff}Original\r\n[[Source|Alias]]\r\n";
        let base = json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"notes/source.md","revision":format!("sha256:{:x}",Sha256::digest(bytes.as_bytes())),"content_base64":STANDARD.encode(bytes.as_bytes()),"media_type":"text/markdown"});
        let store = EditorRecovery::at(dir.clone(), &workspace).unwrap();
        (dir, workspace, base, store)
    }
    fn install(
        v: &mut BrainView,
        workspace: &Value,
        base: &Value,
        store: &EditorRecovery,
        window: &mut Window,
        cx: &mut Context<BrainView>,
    ) {
        v.expected_workspace = Some(workspace.clone());
        v.editor.store = Some(store.clone());
        v.load_source(base.clone(), window, cx);
        v.busy = false;
    }
    fn managed_window(
        cx: &mut TestAppContext,
        workspace: &Value,
        base: &Value,
        store: &EditorRecovery,
        value: &str,
    ) -> (Entity<BrainView>, AnyWindowHandle) {
        let (view, visual) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_guarded(
                "127.0.0.1:1".parse().unwrap(),
                Some(workspace.clone()),
                window,
                cx,
            );
            v.busy = true;
            v
        });
        let handle = view.update_in(visual, |v, window, cx| {
            install(v, workspace, base, store, window, cx);
            v.source.reset(value.to_owned(), window, cx);
            window.window_handle()
        });
        (view, handle)
    }

    #[gpui::test]
    fn app_quit_waits_for_both_drafts_and_retains_legacy_window(cx: &mut TestAppContext) {
        use super::super::super::app_quit;
        let (dir1, workspace1, base1, store1) = fixture();
        let (dir2, workspace2, base2, store2) = fixture();
        cx.update(gpui_component::init);
        cx.add_empty_window();
        let (first, _) =
            managed_window(cx, &workspace1, &base1, &store1, "\u{feff}First latest\r\n");
        let (second, second_window) = managed_window(
            cx,
            &workspace2,
            &base2,
            &store2,
            "\u{feff}Second latest\r\n",
        );
        // Busy is an explicit positive barrier before the second protection operation.
        second.update(cx, |v, _| v.editor.flight = true);
        cx.update(app_quit::test_request);
        let token = cx.update(|cx| app_quit::test_state(cx).0.unwrap());
        cx.update(app_quit::test_request);
        assert_eq!(
            cx.update(|cx| app_quit::test_state(cx)),
            (Some(token), false)
        );
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 3);
        assert_eq!(
            store1.list().unwrap().drafts[0].text,
            "\u{feff}First latest\r\n"
        );
        assert!(store2.list().unwrap().drafts.is_empty());
        first.update(cx, |v, cx| assert!(v.editor_app_quit_ready(token, cx)));
        cx.update(|cx| {
            second_window
                .update(cx, |_, window, cx| {
                    second.update(cx, |v, cx| {
                        v.editor.flight = false;
                        v.editor_protect(window, cx);
                    })
                })
                .unwrap()
        });
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| app_quit::test_state(cx)), (None, true));
        // TestPlatform::quit is a no-op; retained windows prove the coordinator did not remove them individually.
        assert_eq!(cx.windows().len(), 3);
        assert_eq!(
            store2.list().unwrap().drafts[0].text,
            "\u{feff}Second latest\r\n"
        );
        cx.update(app_quit::test_request);
        assert_eq!(store1.list().unwrap().drafts.len(), 1);
        for dir in [dir1, dir2] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[gpui::test]
    fn app_quit_lock_failure_cancels_all_windows_and_retry_preserves_uncertain_save(
        cx: &mut TestAppContext,
    ) {
        use super::super::super::app_quit;
        let (dir1, workspace1, base1, store1) = fixture();
        let (dir2, workspace2, base2, store2) = fixture();
        let uncertain = store1
            .create(&base1, "\u{feff}Uncertain latest\r\n")
            .unwrap();
        let request = source_write(&base1, &uncertain.text, &uuid());
        let uncertain = store1.retain_save(&uncertain, &request).unwrap();
        let old = store2.create(&base2, "Old second\r\n").unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(
                dir2.join(text(&workspace2["brain_id"]))
                    .join("recovery.lock"),
            )
            .unwrap();
        lock.lock().unwrap();
        cx.update(gpui_component::init);
        cx.add_empty_window();
        let (first, _) = managed_window(cx, &workspace1, &base1, &store1, &uncertain.text);
        first.update(cx, |v, _| {
            v.editor.active = Some(uncertain.clone());
            v.editor.confirmed = false;
        });
        let (second, second_window) = managed_window(
            cx,
            &workspace2,
            &base2,
            &store2,
            "\u{feff}Second latest\r\n",
        );
        second.update(cx, |v, _| {
            v.editor.active = Some(old.clone());
            v.editor.confirmed = false;
        });
        cx.update(app_quit::test_request);
        cx.run_until_parked();
        let token = cx.update(|cx| app_quit::test_state(cx).0.unwrap());
        assert_eq!(cx.windows().len(), 3);
        second.update(cx, |v, cx| {
            assert!(
                v.editor.error.is_some(),
                "actual filesystem lock prevented protection"
            );
            assert!(!v.editor_app_quit_ready(token, cx));
            v.editor_keep_editing(cx);
        });
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| app_quit::test_state(cx)), (None, false));
        for view in [&first, &second] {
            view.update(cx, |v, _| assert!(!v.editor_closing()));
        }
        lock.unlock().unwrap();
        assert_eq!(store2.list().unwrap().drafts[0], old);
        assert_eq!(
            store1.list().unwrap().drafts[0].pending_save,
            Some(request.clone())
        );
        cx.update(app_quit::test_request);
        cx.run_until_parked();
        // Failure is actionable and not silently reset by another Quit.
        assert!(!cx.update(|cx| app_quit::test_state(cx).1));
        cx.update(|cx| {
            second_window
                .update(cx, |_, window, cx| {
                    second.update(cx, |v, cx| {
                        v.editor_retry_close_protection(window, cx);
                    })
                })
                .unwrap()
        });
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| app_quit::test_state(cx)), (None, true));
        assert_eq!(
            store2.list().unwrap().drafts[0].text,
            "\u{feff}Second latest\r\n"
        );
        assert_eq!(store1.list().unwrap().drafts[0].pending_save, Some(request));
        first.update(cx, |v, _| {
            assert!(v.editor.save.is_none() && v.editor.sending.is_none())
        });
        for dir in [dir1, dir2] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[gpui::test]
    fn close_waits_for_latest_coalesced_text_and_preserves_another_window(cx: &mut TestAppContext) {
        let (dir, workspace, base, store) = fixture();
        cx.update(gpui_component::init);
        cx.add_empty_window();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&workspace, &store, window, cx);
            v.busy = true;
            v
        });
        view.update_in(cx, |v, window, cx| {
            install(v, &workspace, &base, &store, window, cx);
            v.source.reset("First typing\r\n", window, cx);
            v.editor_protect(window, cx);
            v.source
                .reset("\u{feff}Newest typing\r\n[[Source|Alias]]\r\n", window, cx);
            assert!(!v.editor_request_close(window, cx));
            assert!(v.editor_closing() && v.editor_blocks_navigation() && v.editor.flight);
            assert!(!v.editor_request_close(window, cx));
        });
        assert_eq!(
            cx.windows().len(),
            2,
            "close is vetoed while durability is pending"
        );
        cx.run_until_parked();
        assert_eq!(
            cx.windows().len(),
            1,
            "only the exact requested window closes"
        );
        let durable = store.list().unwrap().drafts.pop().unwrap();
        assert_eq!(
            durable.text,
            "\u{feff}Newest typing\r\n[[Source|Alias]]\r\n"
        );
        assert_eq!(durable.base, base);
        assert!(durable.pending_save.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn failed_protection_keeps_window_and_retry_confirms_latest_text(cx: &mut TestAppContext) {
        let (dir, workspace, base, store) = fixture();
        let old = store.create(&base, "Prior protected\r\n").unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(text(&workspace["brain_id"])).join("recovery.lock"))
            .unwrap();
        lock.lock().unwrap();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&workspace, &store, window, cx);
            v.busy = true;
            v
        });
        view.update_in(cx, |v, window, cx| {
            install(v, &workspace, &base, &store, window, cx);
            v.editor.active = Some(old.clone());
            v.editor.confirmed = true;
            v.source
                .reset("Newest protected after retry\r\n", window, cx);
            assert!(!v.editor_request_close(window, cx));
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1);
        view.update_in(cx, |v, _, cx| {
            assert!(v.editor.error.is_some());
            assert!(v.editor_closing());
            assert_eq!(
                v.source.value(cx).as_ref(),
                "Newest protected after retry\r\n"
            );
        });
        lock.unlock().unwrap();
        assert_eq!(store.list().unwrap().drafts[0], old);
        view.update_in(cx, |v, window, cx| {
            v.editor_retry_close_protection(window, cx)
        });
        cx.run_until_parked();
        assert!(cx.windows().is_empty());
        assert_eq!(
            store.list().unwrap().drafts[0].text,
            "Newest protected after retry\r\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn divergent_record_cannot_be_erased_by_retry_and_keep_editing_retains_error(
        cx: &mut TestAppContext,
    ) {
        let (dir, workspace, base, store) = fixture();
        let old = store.create(&base, "Old generation\r\n").unwrap();
        let newer = store.update(&old, "Another window\r\n").unwrap();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&workspace, &store, window, cx);
            v.busy = true;
            v
        });
        view.update_in(cx, |v, window, cx| {
            install(v, &workspace, &base, &store, window, cx);
            v.editor.active = Some(old.clone());
            v.editor.confirmed = true;
            v.source.reset("Visible local text\r\n", window, cx);
            assert!(!v.editor_request_close(window, cx));
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            v.editor_retry_close_protection(window, cx)
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1);
        assert_eq!(store.list().unwrap().drafts[0], newer);
        view.update_in(cx, |v, _, cx| {
            assert!(v.editor.error.is_some());
            v.editor.closing = None;
            assert!(!v.editor_closing());
            assert!(v.editor.error.is_some());
            assert_eq!(v.source.value(cx).as_ref(), "Visible local text\r\n");
        });
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn legacy_view_keeps_unmanaged_dirty_close_behavior(cx: &mut TestAppContext) {
        let (_, _, base, _) = fixture();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx)
        });
        view.update_in(cx, |v, window, cx| {
            assert!(v.source.managed().is_none());
            v.load_source(base.clone(), window, cx);
            v.source.reset("Legacy dirty text", window, cx);
            assert!(v.editor_request_close(window, cx));
            assert!(!v.editor_closing());
        });
    }
    #[gpui::test]
    fn uncertain_save_is_protected_without_replay(cx: &mut TestAppContext) {
        let (dir, workspace, base, store) = fixture();
        let old = store.create(&base, "Uncertain draft\r\n").unwrap();
        let request = source_write(&base, &old.text, &uuid());
        let pending = store.retain_save(&old, &request).unwrap();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&workspace, &store, window, cx);
            v.busy = true;
            v
        });
        view.update_in(cx, |v, window, cx| {
            install(v, &workspace, &base, &store, window, cx);
            v.editor.active = Some(pending.clone());
            v.editor.confirmed = false;
            v.source.reset(pending.text.clone(), window, cx);
            assert!(!v.editor_request_close(window, cx));
            assert!(v.editor.save.is_none() && v.editor.sending.is_none());
        });
        cx.run_until_parked();
        assert!(cx.windows().is_empty());
        assert_eq!(store.list().unwrap().drafts[0].pending_save, Some(request));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn already_explicit_save_finishes_and_advances_only_its_close_binding(cx: &mut TestAppContext) {
        let (dir, workspace, base, store) = fixture();
        let draft = store.create(&base, "\u{feff}Explicitly saved\r\n").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let wire: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(wire["op"], "source_write");
            let request = &wire["request"];
            let bytes = STANDARD.decode(text(&request["content_base64"])).unwrap();
            let receipt = json!({"operation_id":request["operation_id"],"path":request["path"],"previous_revision":request["expected_revision"],"revision":format!("sha256:{:x}",Sha256::digest(&bytes)),"outcome":"written"});
            let response =
                json!({"schema":wire["schema"],"id":wire["id"],"ok":true,"data":receipt});
            writeln!(stream, "{response}").unwrap();
            wire
        });
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&workspace, &store, window, cx);
            v.busy = true;
            v
        });
        let request = source_write(&base, &draft.text, &uuid());
        view.update_in(cx, |v, window, cx| {
            install(v, &workspace, &base, &store, window, cx);
            v.source.reset(draft.text.clone(), window, cx);
            v.editor.active = Some(draft.clone());
            v.editor.confirmed = true;
            v.endpoint = endpoint;
            v.editor_save(request.clone(), window, cx);
            assert!(v.editor.flight && v.editor.sending.as_ref() == Some(&request));
            assert!(!v.editor_request_close(window, cx));
            assert!(v.editor_closing());
        });
        assert_eq!(cx.windows().len(), 1);
        cx.run_until_parked();
        assert!(cx.windows().is_empty());
        assert!(store.list().unwrap().drafts.is_empty());
        let wire = server.join().unwrap();
        assert_eq!(wire["request"], request["request"]);
        assert_eq!(wire["expected_workspace"], workspace);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn previously_acknowledged_unchanged_text_is_reconfirmed_before_close(cx: &mut TestAppContext) {
        let (dir, workspace, base, store) = fixture();
        let old = store
            .create(&base, "Acknowledged visible text\r\n")
            .unwrap();
        let newer = store
            .update(&old, "Another window advanced this record\r\n")
            .unwrap();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&workspace, &store, window, cx);
            v.busy = true;
            v
        });
        view.update_in(cx, |v, window, cx| {
            install(v, &workspace, &base, &store, window, cx);
            v.source.reset(old.text.clone(), window, cx);
            v.editor.active = Some(old.clone());
            v.editor.confirmed = true;
            assert!(!v.editor_request_close(window, cx));
            assert!(v.editor.flight);
            assert!(!v.editor.confirmed);
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1);
        assert_eq!(store.list().unwrap().drafts[0], newer);
        view.update_in(cx, |v, window, cx| {
            assert!(v.editor.error.is_some());
            assert_eq!(v.source.value(cx).as_ref(), old.text);
            v.editor_retry_close_protection(window, cx);
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1);
        view.update_in(cx, |v, _, cx| {
            assert!(v.editor.error.is_some());
            assert_eq!(v.source.value(cx).as_ref(), old.text);
        });
        assert_eq!(store.list().unwrap().drafts[0], newer);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
