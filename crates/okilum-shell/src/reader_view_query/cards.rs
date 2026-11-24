use super::{
    actions::{ActionRow, DisplayGroup, RowRenderer},
    *,
};

pub(super) fn card(content: AnyElement, cx: &App) -> Div {
    let colors = brand::palette(cx);
    v_flex()
        .min_w_0()
        .p_3()
        .border_1()
        .border_color(colors.border_subtle)
        .rounded_md()
        .bg(colors.surface_raised)
        .child(content)
}

pub(super) fn render(groups: &[DisplayGroup], rows: &[ActionRow], render: &RowRenderer) -> Div {
    v_flex()
        .w_full()
        .gap_3()
        .children(groups.iter().map(|group| {
            group.heading(render.cx).child(
                h_flex()
                    .w_full()
                    .items_stretch()
                    .flex_wrap()
                    .gap_2()
                    .children(group.rows.clone().map(|index| {
                        card(render.row(index, &rows[index], true), render.cx)
                            .flex_1()
                            .flex_basis(px(180.))
                    })),
            )
        }))
}
