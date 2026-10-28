use super::*;
impl Feed {
    pub(crate) fn apply_native(
        &mut self,
        changes: NativeChanges,
        records: &[MessageRecord],
        cx: &mut Context<Self>,
    ) {
        let first = self.native_order.is_none();
        if first {
            self.messages.clear();
            self.ids.clear();
            self.projected.clear();
            self.native_order = Some(Vec::new());
        }
        let mut topology =
            first || changes.reset || changes.order.is_some() || !changes.removed.is_empty();
        if changes.reset {
            let keep: HashSet<_> = changes.rows.iter().map(|row| row.key.clone()).collect();
            let removed: Vec<_> = self
                .native_positions
                .keys()
                .filter(|key| !keep.contains(*key))
                .cloned()
                .collect();
            for key in removed {
                self.remove_native(&key);
            }
        }
        for key in &changes.removed {
            self.remove_native(key);
        }
        let mut items = Vec::new();
        for row in changes.rows {
            let position = if let Some(position) = self.native_positions.get(&row.key) {
                *position
            } else {
                let next = self.next_native_position;
                self.next_native_position += 1;
                self.native_positions.insert(row.key.clone(), next);
                next
            };
            let (kind, title, text) = match presentation::body(&row.item) {
                Body::Message { user, text } => (
                    if user {
                        "user_message"
                    } else {
                        "assistant_message"
                    },
                    None,
                    text,
                ),
                Body::Reasoning(text) => ("reasoning", None, text),
                Body::Compact { label, detail } => (
                    row.item["type"].as_str().unwrap_or("system_notice"),
                    Some(label),
                    detail,
                ),
            };
            items.push(TimelineItem {
                position,
                item_id: row.key.element_id(),
                run_id: None,
                message_id: row.item["messageId"].as_str().map(str::to_owned),
                created_by: None,
                kind: kind.into(),
                status: row.item["status"].as_str().unwrap_or("").into(),
                title,
                text: Some(text),
                text_truncated: false,
                next_text_offset: None,
                updated_at: String::new(),
            });
        }
        if let Some(order) = changes.order {
            self.native_order = Some(
                order
                    .iter()
                    .filter_map(|key| self.native_positions.get(key).copied())
                    .collect(),
            );
            topology = true;
        }
        self.apply_rows(items.iter(), Some(records), topology, cx);
    }
    fn remove_native(&mut self, key: &RowKey) {
        if let Some(position) = self.native_positions.remove(key) {
            self.messages.remove(&position);
            self.expanded.remove(&position);
            if let Some(id) = self.ids.remove(&position) {
                if let Some(count) = self.projected.get_mut(&id) {
                    *count -= 1;
                    if *count == 0 {
                        self.projected.remove(&id);
                    }
                }
            }
        }
    }
    pub(super) fn activity(&self, position: u64, message: &Message, cx: &mut App) -> AnyElement {
        if message.kind == MessageKind::Reasoning {
            let expanded = self.expanded.contains(&position);
            let parent = self.parent.clone();
            return v_flex()
                .id(SharedString::from(format!("project-reasoning-{position}")))
                .debug_selector(move || format!("project-reasoning-{position}"))
                .child(
                    Button::new(("project-reasoning-toggle", position as usize))
                        .ghost()
                        .label(if expanded {
                            "Hide reasoning"
                        } else {
                            "Reasoning"
                        })
                        .debug_selector(move || format!("project-reasoning-toggle-{position}"))
                        .on_click(move |_, _, cx| {
                            let _ = parent.update(cx, |view, cx| {
                                view.feed.update(cx, |feed, cx| {
                                    if !feed.expanded.remove(&position) {
                                        feed.expanded.insert(position);
                                    }
                                    if let Some(index) = feed.indices.get(&position).copied() {
                                        feed.scroller.update(cx, |state, cx| {
                                            state.remeasure_items(index..index + 1, cx)
                                        });
                                    }
                                    cx.notify();
                                })
                            });
                        }),
                )
                .when(expanded, |row| {
                    row.child(
                        TextView::markdown(
                            ("project-reasoning", position as usize),
                            message.text.clone(),
                        )
                        .selectable(true)
                        .style(reader_text_style(cx.theme())),
                    )
                })
                .into_any_element();
        }
        let MessageKind::Compact(label) = &message.kind else {
            return div().into_any_element();
        };
        v_flex()
            .id(SharedString::from(format!("project-tool-{position}")))
            .debug_selector(move || format!("project-tool-{position}"))
            .child(
                div()
                    .text_color(brand::palette(cx).text_muted)
                    .child(label.clone()),
            )
            .when(!message.text.is_empty(), |row| {
                row.child(
                    TextView::markdown(("project-tool", position as usize), message.text.clone())
                        .selectable(true)
                        .style(reader_text_style(cx.theme())),
                )
            })
            .into_any_element()
    }
}
