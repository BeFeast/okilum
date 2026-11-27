//! Human attention is a distinct group in the same virtual stream as stages.
use super::*;
use okilum_t3::board::waiting::Kind;

impl Board {
    pub(super) fn waiting_list_group(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let collapsed = self.list.preferences.waiting_collapsed;
        let count = self.waiting.items.len();
        Button::new("board-list-waiting-group")
            .debug_selector(|| "board-list-waiting-group".into())
            .ghost()
            .w_full()
            .h(px(HEIGHT))
            .px(px(18.))
            .rounded_none()
            .bg(p.surface)
            .accessibility_label(format!(
                "Ждёт тебя {count}, {}",
                if collapsed { "collapsed" } else { "expanded" }
            ))
            .child(
                h_flex()
                    .w_full()
                    .gap(px(9.))
                    .text_size(px(13.))
                    .line_height(px(20.))
                    .child(appearance::icon(
                        if collapsed {
                            "chevron-right"
                        } else {
                            "chevron-down"
                        },
                        p.text_muted,
                    ))
                    .child(div().size(px(8.)).rounded_full().bg(p.warning))
                    .child(
                        div()
                            .debug_selector(|| "board-list-waiting-label".into())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Ждёт тебя"),
                    )
                    .child(div().text_color(p.text_muted).child(count.to_string())),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.list.preferences.waiting_collapsed = !collapsed;
                this.save_presentation();
                this.focus.focus(window, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    pub(super) fn waiting_list_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let item = &self.waiting.items[index];
        let tone = match item.kind {
            Kind::Decision => p.warning,
            Kind::Try => p.success,
            Kind::New => p.danger,
        };
        let target = item.target.clone();
        let mut row = h_flex()
            .debug_selector(move || format!("board-list-waiting-row-{index}"))
            .w_full()
            .min_w_0()
            .h(px(HEIGHT))
            .px(px(18.))
            .pl(px(37.))
            .gap(px(10.))
            .text_size(px(13.))
            .line_height(px(20.))
            .bg(p.canvas.blend(p.warning.opacity(0.045)))
            .border_b_1()
            .border_color(p.border_subtle.opacity(0.5))
            .child(
                div()
                    .debug_selector(move || format!("board-list-waiting-kind-{index}"))
                    .flex_none()
                    .px(px(7.))
                    .py(px(1.))
                    .rounded(px(5.))
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tone)
                    .bg(appearance::tint(tone))
                    .child(item.kind.label()),
            )
            .child(
                div()
                    .debug_selector(move || format!("board-list-waiting-title-{index}"))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(item.title.clone()),
            )
            .child(
                div()
                    .debug_selector(move || format!("board-list-waiting-source-{index}"))
                    .max_w(px(200.))
                    .min_w_0()
                    .text_size(px(12.))
                    .text_color(p.text_muted)
                    .truncate()
                    .child(item.source.clone()),
            );
        if let Some(video) = &item.video {
            let video = video.clone();
            row = row.child(
                // A contextual link leaves one primary action on each row.
                gpui_base::Link::new(("waiting-list-video", index))
                    .debug_selector(move || format!("board-list-waiting-video-{index}"))
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(p.info)
                    .cursor_pointer()
                    .hover(|link| link.underline())
                    .child("Видео")
                    .href(video)
                    .open_with(|url, _, _, cx| cx.open_url(url)),
            );
        }
        row = row.child(
            Button::new(("waiting-list-open", index))
                .debug_selector(move || format!("board-list-waiting-open-{index}"))
                .ghost()
                .small()
                .flex_none()
                .accessibility_label(item.kind.action())
                .child(
                    div()
                        .debug_selector(move || format!("board-list-waiting-action-{index}"))
                        .text_size(px(12.))
                        .line_height(px(20.))
                        .child(item.kind.action()),
                )
                .tooltip(item.target.clone())
                .on_click(cx.listener(move |this, _, _, cx| this.note(&target, cx))),
        );
        if let Some(age) = item.at.and_then(|at| now().checked_sub(at)) {
            row = row.child(
                div()
                    .debug_selector(move || format!("board-list-waiting-time-{index}"))
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(p.text_muted)
                    .child(super::super::waiting::relative_age(age)),
            );
        }
        row.into_any_element()
    }
}
