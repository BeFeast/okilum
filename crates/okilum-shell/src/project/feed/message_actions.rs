//! Whole-message copy uses retained authored text, independently of rendered
//! Markdown selection, code-block copy and the virtualized viewport.
use super::*;
use std::time::Duration;

const COPIED_FOR: Duration = Duration::from_millis(1200);

impl Feed {
    /// Called with the synchronized projection's unsettled run identities.
    /// A completed commentary item is not a final response while its run works.
    pub(in crate::project) fn set_copy_active_runs(
        &mut self,
        runs: impl IntoIterator<Item = String>,
        cx: &mut Context<Self>,
    ) {
        let next: HashSet<_> = runs.into_iter().collect();
        if self.copy_active_runs == next {
            return;
        }
        let changed: HashSet<_> = self
            .copy_active_runs
            .symmetric_difference(&next)
            .cloned()
            .collect();
        let working_changed = self.copy_active_runs.is_empty() != next.is_empty();
        for position in &self.copy_terminals {
            if self
                .messages
                .get(position)
                .and_then(|message| message.run_id.as_ref())
                .is_some_and(|run| {
                    changed.contains(run)
                        || (working_changed && self.copy_response_runs.contains(run))
                })
            {
                if let Some(index) = self.indices.get(position).copied() {
                    self.scroller
                        .update(cx, |state, cx| state.remeasure_items(index..index + 1, cx));
                }
            }
        }
        self.copy_active_runs = next;
        cx.notify();
    }

    /// Recomputed on topology/group changes only, never by row rendering or a
    /// plain streaming text flush. Like T3, keep the last assistant per run;
    /// messages without run identity are delimited by authored user messages.
    pub(super) fn refresh_copy_terminals(&mut self) -> Vec<u64> {
        let mut next = HashSet::new();
        let mut runs = HashSet::new();
        let mut unkeyed_seen = HashSet::new();
        let mut response_runs = HashSet::new();
        let mut seen_users = HashSet::new();
        // Native history pages acquire new local positions, so numeric position
        // order is not timeline order. Follow the retained virtual-list order.
        for (position, message) in self.canonical_rows.iter().rev().filter_map(|position| {
            self.messages
                .get(position)
                .map(|message| (*position, message))
        }) {
            let source = self.groups.source_thread(position).unwrap_or("");
            if message.kind == MessageKind::Chat && message.user {
                seen_users.insert(source);
            }
            if !seen_users.contains(source) {
                if let Some(run) = &message.run_id {
                    response_runs.insert(run.clone());
                }
            }
            if message.kind != MessageKind::Chat {
                continue;
            }
            if message.user {
                unkeyed_seen.remove(source);
            } else if let Some(run) = &message.run_id {
                if runs.insert((source, run.as_str())) {
                    next.insert(position);
                }
            } else if unkeyed_seen.insert(source) {
                next.insert(position);
            }
        }
        let mut changed: Vec<_> = self
            .copy_terminals
            .symmetric_difference(&next)
            .copied()
            .collect();
        if !self.copy_active_runs.is_empty() {
            let response_changes: HashSet<_> = self
                .copy_response_runs
                .symmetric_difference(&response_runs)
                .collect();
            changed.extend(
                next.iter()
                    .filter(|position| {
                        self.messages
                            .get(position)
                            .and_then(|message| message.run_id.as_ref())
                            .is_some_and(|run| response_changes.contains(run))
                    })
                    .copied(),
            );
        }
        self.copy_terminals = next;
        self.copy_response_runs = response_runs;
        changed
    }

    pub(super) fn copy_text(&self, position: u64) -> Option<SharedString> {
        let message = self.messages.get(&position)?;
        if message.kind != MessageKind::Chat || !message.complete_text {
            return None;
        }
        let visible = if message.user {
            // T3's deriveDisplayedUserMessageState.copyText is the complete
            // authored prompt, including terminal/element context suffixes.
            !message.text.is_empty()
        } else {
            self.copy_terminals.contains(&position)
                && message.nonblank_text
                && !self.groups.streaming_message(position)
                && matches!(
                    message.status.as_ref(),
                    "completed" | "interrupted" | "cancelled" | "failed"
                )
                && !message.run_id.as_ref().is_some_and(|run| {
                    if self
                        .groups
                        .bound_thread
                        .as_deref()
                        .zip(self.groups.source_thread(position))
                        .is_some_and(|(bound, source)| bound != source)
                    {
                        return false;
                    }
                    self.copy_active_runs.contains(run)
                        || (!self.copy_active_runs.is_empty()
                            && self.copy_response_runs.contains(run))
                })
        };
        visible.then(|| message.text.clone())
    }
}

struct CopyState {
    lease: Arc<()>,
    source_id: String,
    copied: bool,
    generation: u64,
}

#[derive(IntoElement)]
pub(super) struct MessageActions {
    pub(super) feed: WeakEntity<Feed>,
    pub(super) position: u64,
    pub(super) lease: Arc<()>,
    pub(super) user: bool,
    pub(super) source_id: String,
}
impl RenderOnce for MessageActions {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            feed,
            position,
            lease,
            user,
            source_id,
        } = self;
        let state = window.use_keyed_state("project-message-copy-state", cx, |_, _| CopyState {
            lease: lease.clone(),
            source_id: source_id.clone(),
            copied: false,
            generation: 0,
        });
        if !Arc::ptr_eq(&state.read(cx).lease, &lease) || state.read(cx).source_id != source_id {
            state.update(cx, |state, _| {
                state.lease = lease.clone();
                state.source_id = source_id.clone();
                state.copied = false;
                state.generation = state.generation.wrapping_add(1);
            });
        }
        let copied = state.read(cx).copied;
        h_flex()
            .w_full()
            .when(user, |row| row.justify_end())
            .tab_group()
            .child(
                Button::new("project-message-copy")
                    .xsmall()
                    .ghost()
                    .tab_stop(true)
                    .icon(if copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .disabled(copied)
                    .tooltip(if copied { "Copied" } else { "Copy message" })
                    .accessibility_label(if copied {
                        "Message copied"
                    } else {
                        "Copy message"
                    })
                    .debug_selector(move || {
                        format!(
                            "project-message-{}-{position}",
                            if copied { "copied" } else { "copy" }
                        )
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        if state.read(cx).copied {
                            return;
                        }
                        // Re-read current authority and text at activation. A late
                        // click must not copy a stale streaming closure or another
                        // conversation which reused the same list position.
                        let text = feed.upgrade().and_then(|feed| {
                            let feed = feed.read(cx);
                            (Arc::ptr_eq(&feed.copy_lease, &lease)
                                && feed
                                    .messages
                                    .get(&position)
                                    .is_some_and(|message| message.source_id == source_id))
                            .then(|| feed.copy_text(position))
                            .flatten()
                        });
                        let Some(text) = text else {
                            return;
                        };
                        // GPUI returns void here: no platform error/result is
                        // available, so this control invents no failure claims.
                        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                        let generation = state.update(cx, |state, cx| {
                            state.copied = true;
                            state.generation = state.generation.wrapping_add(1);
                            cx.notify();
                            state.generation
                        });
                        let state = state.downgrade();
                        cx.spawn(async move |cx| {
                            cx.background_executor().timer(COPIED_FOR).await;
                            let _ = state.update(cx, |state, cx| {
                                if state.generation == generation {
                                    state.copied = false;
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use serde_json::json;

    fn message(
        position: u64,
        user: bool,
        text: &str,
        status: &str,
        run: Option<&str>,
    ) -> TimelineItem {
        serde_json::from_value(json!({
            "position":position, "itemId":format!("item-{position}"), "messageId":format!("message-{position}"),
            "runId":run, "type":if user {"user_message"} else {"assistant_message"},
            "text":text, "status":status, "updatedAt":"now"
        })).unwrap()
    }

    struct Fixture {
        feed: Entity<Feed>,
        _parent: Entity<ProjectView>,
    }
    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                self.feed
                    .clone()
                    .cached(StyleRefinement::default().size_full()),
            )
        }
    }
    fn fixture(
        cx: &mut TestAppContext,
        items: Vec<TimelineItem>,
    ) -> (Entity<Feed>, &mut VisualTestContext) {
        cx.update(crate::test_init);
        let (root, visual) = cx.add_window_view(|window, cx| {
            let parent = cx.new(|cx| ProjectView::empty(None, window, cx));
            parent.update(cx, |parent, _| parent.active = false);
            let feed = parent.read(cx).feed.clone();
            feed.update(cx, |feed, cx| feed.apply(items.iter(), None, cx));
            let view = cx.new(|_| Fixture {
                feed,
                _parent: parent,
            });
            Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(760.), px(900.)));
        visual.run_until_parked();
        let feed = root.read_with(visual, |root, cx| {
            root.view()
                .clone()
                .downcast::<Fixture>()
                .unwrap()
                .read(cx)
                .feed
                .clone()
        });
        (feed, visual)
    }
    fn clipboard(visual: &mut VisualTestContext) -> String {
        visual.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap())
    }
    fn activate_keyboard(visual: &mut VisualTestContext, copied_selector: &'static str) {
        // Pointer activation intentionally prevents focus in the shared Button.
        // Give the isolated window an initial dispatch path, as the vendor's
        // keyboard fixture does, before exercising real Tab and Enter events.
        visual.update(|window, cx| {
            window.focus_next(cx);
            assert!(
                window.focused(cx).is_some(),
                "positive control: keyboard focus is established"
            );
            window.draw(cx).clear(cx);
        });
        for _ in 0..8 {
            visual.simulate_keystrokes("tab");
            visual.run_until_parked();
            visual.update(|window, cx| {
                assert!(
                    window.focused(cx).is_some(),
                    "Tab preserves a keyboard dispatch target"
                );
                window.draw(cx).clear(cx);
            });
            let keystroke = Keystroke::parse("enter").unwrap();
            visual.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            visual.simulate_event(KeyUpEvent { keystroke });
            visual.run_until_parked();
            if visual.debug_bounds(copied_selector).is_some() {
                break;
            }
        }
    }

    #[gpui::test]
    fn actual_click_and_keyboard_copy_the_full_authored_message_with_feedback(
        cx: &mut TestAppContext,
    ) {
        let full = format!(
            "# Report\n\n| Name | Result |\n| --- | --- |\n| Build | Passed |\n\n```rust\nlet brain = \"🧠\";\n```\n\n{}\nAUTHORED END\r\n",
            "Long retained paragraph. ".repeat(5000)
        );
        assert!(full.len() > 100_000);
        for user in [true, false] {
            let (_feed, visual) =
                fixture(cx, vec![message(1, user, &full, "completed", Some("run"))]);
            let button = visual
                .debug_bounds("project-message-copy-1")
                .expect("copy footer at live edge");
            visual.simulate_click(button.center(), Modifiers::default());
            visual.run_until_parked();
            assert_eq!(clipboard(visual), full);
            assert!(visual.debug_bounds("project-message-copied-1").is_some());
            visual.update(|_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into()))
            });
            visual.simulate_click(button.center(), Modifiers::default());
            visual.run_until_parked();
            assert_eq!(
                clipboard(visual),
                "sentinel",
                "success feedback disables repeated activation"
            );
            // Initial feedback and disabled activation were observed above.
            // Expire it without assuming the event helpers consumed no time.
            visual.executor().advance_clock(Duration::from_millis(2000));
            visual.run_until_parked();
            assert!(visual.debug_bounds("project-message-copy-1").is_some());
            activate_keyboard(visual, "project-message-copied-1");
            assert!(
                clipboard(visual) == full,
                "Tab and Enter activate whole-message copy with exact authored bytes"
            );
        }
    }

    #[gpui::test]
    fn streaming_flush_and_terminal_response_copy_use_latest_text(cx: &mut TestAppContext) {
        let (feed, visual) = fixture(
            cx,
            vec![
                message(
                    1,
                    true,
                    "Prompt\n<terminal_context>\nraw context\n</terminal_context>",
                    "completed",
                    None,
                ),
                message(2, false, "Interim commentary", "completed", Some("run")),
                message(3, false, "First streaming text", "streaming", Some("run")),
            ],
        );
        assert!(visual.debug_bounds("project-message-copy-1").is_some());
        assert!(visual.debug_bounds("project-message-copy-2").is_none());
        assert!(visual.debug_bounds("project-message-copy-3").is_none());
        let user_copy = visual.debug_bounds("project-message-copy-1").unwrap();
        visual.simulate_click(user_copy.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            clipboard(visual),
            "Prompt\n<terminal_context>\nraw context\n</terminal_context>",
            "whole user copy keeps authored context rather than reconstructed Markdown"
        );
        feed.update(visual, |feed, cx| {
            feed.set_copy_active_runs(["run".into()], cx);
            feed.apply(
                std::iter::once(&message(
                    3,
                    false,
                    "Last complete body",
                    "completed",
                    Some("run"),
                )),
                None,
                cx,
            );
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("project-message-copy-3").is_none(),
            "completed commentary stays hidden while run works"
        );
        feed.update(visual, |feed, cx| feed.set_copy_active_runs([], cx));
        visual.run_until_parked();
        let button = visual.debug_bounds("project-message-copy-3").unwrap();
        // Inject a same-geometry source flush without notifying/drawing: the
        // existing rendered callback must read retained text at activation.
        feed.update(visual, |feed, _| {
            feed.messages.get_mut(&3).unwrap().text = "Latest source body".into();
        });
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(clipboard(visual), "Latest source body");
        visual.executor().advance_clock(Duration::from_millis(1300));
        visual.run_until_parked();
        // A real multiline flush remeasures the footer. Activate its current
        // geometry rather than clicking the old one-line footer location.
        feed.update(visual, |feed, cx| {
            feed.apply(
                std::iter::once(&message(
                    3,
                    false,
                    "Final flush\n\n```txt\nexact bytes\n```",
                    "completed",
                    Some("run"),
                )),
                None,
                cx,
            )
        });
        visual.run_until_parked();
        let button = visual.debug_bounds("project-message-copy-3").unwrap();
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(clipboard(visual), "Final flush\n\n```txt\nexact bytes\n```");
    }

    #[gpui::test]
    fn empty_partial_and_offscreen_rows_do_not_gain_copy_authority(cx: &mut TestAppContext) {
        let mut partial = message(1001, false, "Only a prefix", "completed", Some("partial"));
        partial.text_truncated = true;
        let mut items: Vec<_> = (0..1000)
            .map(|i| message(i, true, "Retained older prompt", "completed", None))
            .collect();
        items.extend([
            message(1000, false, "   ", "completed", Some("blank")),
            partial,
        ]);
        let (feed, visual) = fixture(cx, items);
        assert!(
            visual.debug_bounds("project-message-copy-0").is_none(),
            "positive control: old row is offscreen"
        );
        assert!(visual.debug_bounds("project-message-copy-1000").is_none());
        assert!(visual.debug_bounds("project-message-copy-1001").is_none());
        feed.read_with(visual, |feed, _| {
            assert_eq!(feed.copy_text(0).as_deref(), Some("Retained older prompt"));
            assert!(feed.copy_text(1000).is_none());
            assert!(feed.copy_text(1001).is_none());
            let positions: HashSet<_> = feed.rendered_positions.borrow().iter().copied().collect();
            assert!(
                positions.len() <= 32,
                "only viewport rows may be rendered: {positions:?}"
            );
        });
    }

    #[gpui::test]
    fn native_history_prepend_keeps_copy_on_the_final_assistant_and_run_authority(
        cx: &mut TestAppContext,
    ) {
        use okilum_t3::projection::{Changes, TurnRow};
        let (feed, visual) = fixture(cx, Vec::new());
        let row = |id: &str, ordinal: u64, text: &str| {
            Arc::new(TurnRow {
                key: RowKey {
                    thread: "thread".into(),
                    item: id.into(),
                },
                local: true,
                item: Arc::new(
                    json!({"id":id,"threadId":"thread","type":"assistant_message", "ordinal":ordinal,
                "messageId":id,"runId":"run","status":"completed","text":text}),
                ),
            })
        };
        let final_row = row("final", 20, "Final answer");
        let earlier = row("earlier", 10, "Earlier commentary");
        feed.update(visual, |feed, cx| {
            feed.apply_native(
                Changes {
                    rows: vec![final_row.clone()],
                    order: Some(vec![final_row.key.clone()]),
                    ..Default::default()
                },
                &[],
                cx,
            )
        });
        feed.update(visual, |feed, cx| {
            feed.apply_native(
                Changes {
                    rows: vec![earlier.clone()],
                    order: Some(vec![earlier.key.clone(), final_row.key.clone()]),
                    ..Default::default()
                },
                &[],
                cx,
            )
        });
        visual.run_until_parked();
        feed.read_with(visual, |feed, _| {
            let final_position = feed.native_positions[&final_row.key];
            let earlier_position = feed.native_positions[&earlier.key];
            assert!(
                earlier_position > final_position,
                "positive control: page prepend allocates positions out of order"
            );
            assert_eq!(
                feed.copy_text(final_position).as_deref(),
                Some("Final answer")
            );
            assert!(feed.copy_text(earlier_position).is_none());
        });
        feed.update(visual, |feed, cx| {
            let next = row("next-run", 30, "A later provisional response");
            let mut item = next.item.as_ref().clone();
            item["runId"] = json!("another-run");
            let next = Arc::new(TurnRow {
                item: Arc::new(item),
                ..next.as_ref().clone()
            });
            feed.apply_native(
                Changes {
                    rows: vec![next.clone()],
                    order: Some(vec![
                        earlier.key.clone(),
                        final_row.key.clone(),
                        next.key.clone(),
                    ]),
                    ..Default::default()
                },
                &[],
                cx,
            );
            feed.set_copy_active_runs(["another-run".into()], cx);
        });
        visual.run_until_parked();
        feed.read_with(visual, |feed, _| assert!(feed.copy_text(feed.native_positions[&final_row.key]).is_none(),
            "T3 hides provisional completed runs in the same visual response while a later run still works"));
        feed.update(visual, |feed, cx| {
            feed.set_copy_active_runs(["run".into()], cx)
        });
        visual.run_until_parked();
        feed.read_with(visual, |feed, _| {
            assert!(feed
                .copy_text(feed.native_positions[&final_row.key])
                .is_none())
        });
        feed.update(visual, |feed, cx| feed.set_copy_active_runs([], cx));
        visual.run_until_parked();
        feed.read_with(visual, |feed, _| {
            assert_eq!(
                feed.copy_text(feed.native_positions[&final_row.key])
                    .as_deref(),
                Some("Final answer")
            )
        });
    }

    #[gpui::test]
    fn inherited_same_named_runs_keep_source_qualified_copy_authority(cx: &mut TestAppContext) {
        use okilum_t3::projection::{Changes, TurnRow};
        let (feed, visual) = fixture(cx, Vec::new());
        let make = |thread: &str| {
            Arc::new(TurnRow {
                key: RowKey {
                    thread: thread.into(),
                    item: "answer".into(),
                },
                local: thread == "thread",
                item: Arc::new(
                    serde_json::json!({"id":"answer","threadId":thread,"type":"assistant_message","ordinal":1,
                "runId":"same-run","status":"completed","text":format!("Answer from {thread}")}),
                ),
            })
        };
        let own = make("thread");
        let inherited = make("ancestor");
        feed.update(visual, |feed, cx| {
            feed.set_timeline_runs("thread", &[], None, cx);
            feed.apply_native(
                Changes {
                    rows: vec![inherited.clone(), own.clone()],
                    order: Some(vec![inherited.key.clone(), own.key.clone()]),
                    ..Default::default()
                },
                &[],
                cx,
            );
        });
        visual.run_until_parked();
        feed.read_with(visual, |feed, _| {
            assert_eq!(
                feed.copy_text(feed.native_positions[&own.key]).as_deref(),
                Some("Answer from thread")
            );
            assert_eq!(
                feed.copy_text(feed.native_positions[&inherited.key])
                    .as_deref(),
                Some("Answer from ancestor")
            );
        });
        feed.update(visual, |feed, cx| {
            feed.set_copy_active_runs(["same-run".into()], cx)
        });
        visual.run_until_parked();
        feed.read_with(visual, |feed, _| {
            assert!(feed.copy_text(feed.native_positions[&own.key]).is_none());
            assert_eq!(
                feed.copy_text(feed.native_positions[&inherited.key])
                    .as_deref(),
                Some("Answer from ancestor")
            );
        });
    }
}
