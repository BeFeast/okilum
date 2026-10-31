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
        let mut previews = HashMap::new();
        for row in changes.rows {
            let position = if let Some(position) = self.native_positions.get(&row.key) {
                *position
            } else {
                let next = self.next_native_position;
                self.next_native_position += 1;
                self.native_positions.insert(row.key.clone(), next);
                next
            };
            self.retain_detail_source(position, &row, cx);
            previews.insert(
                position,
                activity_preview(&presentation::preview(&row.item)),
            );
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
        self.apply_rows(items.iter(), Some(records), topology, Some(&previews), cx);
    }
    fn remove_native(&mut self, key: &RowKey) {
        if let Some(position) = self.native_positions.remove(key) {
            self.messages.remove(&position);
            self.expanded.remove(&position);
            self.details.remove(&position);
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
    fn toggle_activity(&mut self, position: u64, cx: &mut Context<Self>) {
        if !self.expanded.remove(&position) {
            self.expanded.insert(position);
            self.load_detail(position, cx);
        }
        if let Some(index) = self.indices.get(&position).copied() {
            self.scroller
                .update(cx, |state, cx| state.remeasure_items(index..index + 1, cx));
        }
        cx.notify();
    }

    pub(super) fn activity(&self, position: u64, message: &Message, cx: &mut App) -> AnyElement {
        let reasoning = message.kind == MessageKind::Reasoning;
        let (label, collapsible) = match &message.kind {
            MessageKind::Reasoning => (SharedString::from("Reasoning"), true),
            MessageKind::Compact { label, collapsible } => (label.clone(), *collapsible),
            MessageKind::Chat => return div().into_any_element(),
        };
        let expanded = !collapsible || self.expanded.contains(&position);
        let parent = self.parent.clone();
        let kind = if reasoning { "reasoning" } else { "tool" };
        v_flex()
            .id(SharedString::from(format!("project-{kind}-{position}")))
            .debug_selector(move || format!("project-{kind}-{position}"))
            .mb_2()
            .pl_3()
            .border_l_1()
            .border_color(brand::palette(cx).border_subtle)
            .when(collapsible, |row| {
                row.child(
                    Button::new(("project-activity-toggle", position as usize))
                        .ghost()
                        .icon(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .label(label.clone())
                        .tooltip(if expanded {
                            "Hide details"
                        } else {
                            "Show details"
                        })
                        .debug_selector(move || format!("project-{kind}-toggle-{position}"))
                        .on_click(move |_, _, cx| {
                            let _ = parent.update(cx, |view, cx| {
                                view.feed
                                    .update(cx, |feed, cx| feed.toggle_activity(position, cx))
                            });
                        }),
                )
            })
            .when(!collapsible, |row| {
                row.child(div().text_color(brand::palette(cx).text_muted).child(label))
            })
            .when(
                collapsible && !expanded && !message.preview.is_empty(),
                |row| {
                    row.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(brand::palette(cx).text_muted)
                            .debug_selector(move || format!("project-{kind}-preview-{position}"))
                            .child(message.preview.clone()),
                    )
                },
            )
            .when(expanded && !message.text.is_empty(), |row| {
                row.child(
                    div()
                        .debug_selector(move || format!("project-{kind}-detail-{position}"))
                        .child(self.markdown(
                            ("project-activity", position as usize),
                            self.detail_text(position, &message.text),
                            cx,
                        )),
                )
            })
            .when(expanded, |row| row.child(self.detail_status(position, cx)))
            .into_any_element()
    }
}
