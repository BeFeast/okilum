//! Cheap native rendering of the waiting projection, cached on snapshot changes.
use super::*;
use okilum_t3::board::waiting as model;

impl Board {
    pub(super) fn refresh_waiting(&mut self) {
        self.waiting = self
            .binding
            .as_ref()
            .map_or_else(Default::default, |binding| {
                let binding = okilum_t3::board::live::project(
                    binding,
                    &self.snapshot.forgejo.value,
                    self.projection_time(),
                );
                model::project(
                    &self.project_notes,
                    &self.delivery,
                    &binding,
                    &self.snapshot.forgejo.value,
                    now(),
                    self.projection_time(),
                )
            });
    }
    pub(super) fn render_waiting(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.waiting.visible() && self.waiting.problem.is_none() {
            return None;
        }
        let colors = brand::palette(cx);
        let mut view = v_flex().w_full().min_w_0().gap_2();
        if let Some(problem) = &self.waiting.problem {
            view = view.child(
                div()
                    .debug_selector(|| "board-waiting-problem".into())
                    .text_size(px(12.))
                    .text_color(colors.text_muted)
                    .child(format!("Project notes: {problem}")),
            );
        }
        if !self.waiting.visible() {
            return Some(view.into_any_element());
        }
        let mut strip = h_flex()
            .debug_selector(|| "board-waiting".into())
            .w_full()
            .min_w_0()
            .items_start()
            .gap_2();
        strip = strip.child(
            div()
                .flex_none()
                .py_3()
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(format!("Ждёт тебя · {}", self.waiting.items.len())),
        );
        let mut cards = h_flex().flex_1().min_w_0().gap_2().flex_wrap();
        for (index, item) in self
            .waiting
            .items
            .iter()
            .take(if self.waiting_expanded { usize::MAX } else { 4 })
            .enumerate()
        {
            let color = match item.kind {
                model::Kind::Decision => colors.warning,
                model::Kind::Try => colors.success,
                model::Kind::New => colors.danger,
            };
            let target = item.target.clone();
            let mut card = v_flex()
                .debug_selector(move || format!("board-waiting-item-{index}"))
                .flex_1()
                .flex_basis(px(180.))
                .min_w_0()
                .gap_1()
                .p_3()
                .border_1()
                .border_color(color.opacity(0.2))
                .rounded(px(8.))
                .bg(colors.surface.blend(appearance::tint(colors.warning)))
                .child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(color)
                                .child(item.kind.label()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.))
                                .text_color(colors.text_muted)
                                .child(item.source.clone()),
                        ),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(14.))
                        .child(item.title.clone()),
                );
            let mut actions = h_flex().gap_2().flex_wrap().child(
                Button::new(("waiting-open", index))
                    .ghost()
                    .small()
                    .label(item.kind.action())
                    .debug_selector(move || format!("board-waiting-open-{index}"))
                    .tooltip(item.target.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.note(&target, cx))),
            );
            if let Some(video) = &item.video {
                let video = video.clone();
                actions = actions.child(
                    Button::new(("waiting-video", index))
                        .ghost()
                        .small()
                        .label("Видео")
                        .on_click(move |_, _, cx| cx.open_url(&video)),
                );
            }
            if let Some(age) = item.at.and_then(|at| now().checked_sub(at)) {
                actions = actions.child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.text_muted)
                        .child(relative_age(age)),
                );
            }
            card = card.child(actions);
            cards = cards.child(card);
        }
        strip = strip.child(cards);
        view = view.child(strip);
        if self.waiting.items.len() > 4 {
            view = view.child(
                Button::new("waiting-more")
                    .ghost()
                    .small()
                    .label(if self.waiting_expanded {
                        "Свернуть".into()
                    } else {
                        format!("Ещё {}", self.waiting.items.len() - 4)
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.waiting_expanded = !this.waiting_expanded;
                        cx.notify();
                    })),
            );
        }
        Some(view.into_any_element())
    }
}
fn relative_age(age: u64) -> String {
    if age < 60 {
        "только что".into()
    } else if age < 3600 {
        format!("{} мин назад", age / 60)
    } else if age < 86400 {
        format!("{} ч назад", age / 3600)
    } else {
        format!("{} д назад", age / 86400)
    }
}

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "project-ui-harness"))]
pub(in crate::project) mod fixture;
