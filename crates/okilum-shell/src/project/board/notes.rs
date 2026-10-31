//! Project-note attention. All actions reuse the existing note/draft routes.
use super::*;
use okilum_t3::board::{context as model, delivery as facts};

impl Board {
    pub(super) fn refresh_project_notes(
        &mut self,
        ticket: okilum_t3::board::Ticket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.notes_loading {
            return;
        }
        self.notes_loading = true;
        let area = self.area.clone();
        cx.spawn_in(window, async move |this, cx| {
            let update = cx
                .background_executor()
                .spawn(async move { model::refresh(&area) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.active || !this.snapshot.matches(&ticket) {
                    return;
                }
                this.project_notes.accept(update, now());
                this.notes_loading = false;
                cx.notify();
            });
        })
        .detach();
    }
    fn note_try_row(&self, item: &model::Item, binding: &Binding) -> Option<facts::Row> {
        let note = item.note.as_ref()?;
        if note.meta.kind != model::Kind::Try {
            return None;
        }
        let scope = binding
            .issues
            .iter()
            .find(|scope| Some(scope.number) == note.meta.issue)?;
        self.delivery
            .rows(scope, note.meta.platform?, now(), binding.refresh_seconds)
            .into_iter()
            .find(|row| Some(row.track.1) == note.meta.channel)
    }
    fn note_draft(&self, item: &model::Item, binding: &Binding) -> String {
        let mut text = format!("Project note: {}\n", self.area.join(&item.path).display());
        if let Some(note) = &item.note {
            text.push_str(&format!(
                "{}: {}\nReason: {}\nConsequence: {}\nNote revision: {}\n",
                note.meta.kind.label(),
                note.meta.title,
                note.meta.reason,
                note.meta.consequence,
                note.revision
            ));
            if let Some(issue) = note.meta.issue {
                text.push_str(&format!(
                    "Issue: {}{}\n",
                    binding.issue_url(issue),
                    if binding.issues.iter().any(|scope| scope.number == issue) {
                        ""
                    } else {
                        " (outside this Board's selected stage)"
                    }
                ));
            }
            if let Some(path) = &note.meta.resolution {
                text.push_str(&format!(
                    "Resolution evidence: {}\n",
                    self.area.join(path).display()
                ));
            }
            if let Some(row) = self.note_try_row(item, binding) {
                let available = model::Snapshot::available(item, &row);
                if let Some(build) = row.build {
                    text.push_str(&format!(
                        "Build: {} {} {} · source {}\nArtifact: {}\nAvailability: {}\n",
                        build.platform.label(),
                        build.channel.label(),
                        build.build,
                        build.source,
                        build.artifact,
                        if available {
                            "confirmed"
                        } else {
                            "unconfirmed"
                        }
                    ));
                }
            }
        }
        if let Some(problem) = &item.problem {
            text.push_str(&format!("Source problem: {problem}\n"));
        }
        text.push_str("\nQuestion for Manager: ");
        text
    }
    fn project_note_card(
        &self,
        item: &model::Item,
        key: &str,
        binding: &Binding,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = brand::palette(cx);
        let meta = item.note.as_ref().map(|note| &note.meta);
        let title = meta.map_or_else(|| item.path.clone(), |meta| meta.title.clone());
        let label = meta.map_or("Note unavailable", |meta| meta.kind.label());
        let tooltip = item.note.as_ref().map_or(item.path.clone(), |note| {
            format!("{}\nRevision {}", note.path, note.revision)
        });
        let selector = format!("board-note-card-{key}");
        let mut card = v_flex()
            .debug_selector(move || selector.clone())
            .flex_1()
            .flex_basis(px(180.))
            .min_w_0()
            .gap_1()
            .p_3()
            .border_1()
            .border_color(colors.border_subtle)
            .rounded_md()
            .bg(colors.surface_raised)
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(colors.text_muted)
                    .child(label),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .whitespace_normal()
                    .child(title),
            );
        if let Some(meta) = meta {
            card = card
                .child(
                    div()
                        .text_size(px(12.))
                        .whitespace_normal()
                        .child(short(&meta.reason)),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.text_muted)
                        .whitespace_normal()
                        .child(short(&meta.consequence)),
                );
            if let Some(issue) = meta.issue {
                let outside = !binding.issues.iter().any(|scope| scope.number == issue);
                let url = binding.issue_url(issue);
                card = card.child(
                    Button::new(SharedString::from(format!("note-issue-{key}")))
                        .ghost()
                        .small()
                        .label(format!(
                            "#{issue}{}",
                            if outside {
                                " · outside selected stage"
                            } else {
                                ""
                            }
                        ))
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                );
            }
            if meta.kind == model::Kind::Try {
                let row = self.note_try_row(item, binding);
                let ready = row
                    .as_ref()
                    .is_some_and(|row| model::Snapshot::available(item, row));
                let label = if ready {
                    "Available to try"
                } else {
                    "Try unavailable · build/scenario evidence unconfirmed"
                };
                let selector = format!("board-note-try-state-{key}");
                card = card.child(
                    div()
                        .debug_selector(move || selector.clone())
                        .text_size(px(12.))
                        .text_color(if ready {
                            colors.success
                        } else {
                            colors.text_muted
                        })
                        .whitespace_normal()
                        .child(label),
                );
                if let Some(row) = row {
                    if let Some(build) = row.build {
                        card = card.child(div().text_size(px(12.)).whitespace_normal().child(
                            format!(
                                "{} {} {} · {}",
                                build.platform.label(),
                                build.channel.label(),
                                build.build,
                                build.source
                            ),
                        ));
                        if let Some(evidence) = row.evidence {
                            card = card.child(
                                div()
                                    .text_size(px(12.))
                                    .whitespace_normal()
                                    .child(evidence.scenario),
                            );
                        }
                        if ready {
                            let url = build.artifact;
                            card = card.child(
                                Button::new(SharedString::from(format!("note-build-{key}")))
                                    .ghost()
                                    .small()
                                    .label("Build")
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            );
                        }
                    }
                }
            }
        }
        if let Some(problem) = &item.problem {
            card = card.child(
                div()
                    .text_size(px(12.))
                    .text_color(colors.warning)
                    .whitespace_normal()
                    .child(problem.clone()),
            );
        }
        let path = item.path.clone();
        let draft = self.note_draft(item, binding);
        let selector = format!("board-note-open-{key}");
        let mut actions = h_flex()
            .flex_wrap()
            .gap_1()
            .child(
                Button::new(SharedString::from(format!("note-open-{key}")))
                    .ghost()
                    .small()
                    .label("Open note")
                    .tooltip(tooltip)
                    .debug_selector(move || selector.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.note(&path, cx))),
            )
            .child(
                Button::new(SharedString::from(format!("note-discuss-{key}")))
                    .ghost()
                    .small()
                    .label("Discuss")
                    .tooltip("Prepare a Manager draft; does not send")
                    .debug_selector({
                        let key = key.to_owned();
                        move || format!("board-note-discuss-{key}")
                    })
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(Action::Discuss(draft.clone()))),
                    ),
            );
        if let Some(path) = meta.and_then(|meta| meta.resolution.as_ref()) {
            let path = path.clone();
            actions = actions.child(
                Button::new(SharedString::from(format!("note-resolution-{key}")))
                    .ghost()
                    .small()
                    .label("Resolution note")
                    .on_click(cx.listener(move |this, _, _, cx| this.note(&path, cx))),
            );
        }
        card.child(actions).into_any_element()
    }
    pub(super) fn render_project_notes(
        &self,
        binding: &Binding,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.project_notes.enabled() {
            return None;
        }
        let items = self.project_notes.active(now(), binding.refresh_seconds);
        let next = self.project_notes.next(now(), binding.refresh_seconds);
        let problem =
            model::source_problem(&self.project_notes.config, now(), binding.refresh_seconds);
        if items.is_empty() && next.is_none() && problem.is_none() {
            return None;
        }
        let colors = brand::palette(cx);
        let mut view = v_flex().w_full().min_w_0().gap_2();
        if let Some(error) = problem {
            view = view.child(
                div()
                    .text_size(px(12.))
                    .text_color(colors.warning)
                    .child(format!("Project notes: {error}")),
            );
        }
        if !items.is_empty() {
            view = view.child(
                div()
                    .debug_selector(|| "board-note-attention".into())
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Needs attention · {}", items.len())),
            );
            let mut cards = h_flex().w_full().min_w_0().flex_wrap().gap_2();
            for (index, item) in items
                .iter()
                .take(if self.notes_expanded { items.len() } else { 4 })
                .enumerate()
            {
                cards = cards.child(self.project_note_card(item, &index.to_string(), binding, cx));
            }
            view = view.child(cards);
            if items.len() > 4 {
                let label = if self.notes_expanded {
                    "Show fewer notes".into()
                } else {
                    format!("+{} notes", items.len() - 4)
                };
                view = view.child(
                    Button::new("note-more")
                        .ghost()
                        .small()
                        .label(label)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notes_expanded = !this.notes_expanded;
                            cx.notify();
                        })),
                );
            }
        }
        if let Some(next) = next {
            view = view.child(
                Button::new("board-next-stage")
                    .debug_selector(|| "board-next-stage".into())
                    .ghost()
                    .small()
                    .icon(if self.next_stage_open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .label(if next.problem.is_some() {
                        "Next stage · source problem"
                    } else {
                        "Next stage"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.next_stage_open = !this.next_stage_open;
                        cx.notify();
                    })),
            );
            if self.next_stage_open {
                view = view.child(self.project_note_card(&next, "next", binding, cx));
            }
        }
        Some(view.into_any_element())
    }
}

fn short(text: &str) -> String {
    let mut chars = text.chars();
    let mut shown: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        shown.push('…');
    }
    shown
}
