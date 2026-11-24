use super::{
    actions::{ActionRow, DisplayGroup, RowRenderer},
    cards, *,
};

pub(super) fn render(
    groups: &[DisplayGroup],
    rows: &[ActionRow],
    render: &RowRenderer,
) -> impl IntoElement {
    div()
        .id("view-board-scroll")
        .w_full()
        .min_w_0()
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .child(
            h_flex()
                .items_start()
                .gap_2()
                .children(groups.iter().map(|group| {
                    v_flex()
                        .w(px(192.))
                        .flex_shrink_0()
                        .gap_2()
                        .child(group.heading(render.cx))
                        .children(group.rows.clone().map(|index| {
                            cards::card(render.row(index, &rows[index], true), render.cx).w_full()
                        }))
                })),
        )
}
