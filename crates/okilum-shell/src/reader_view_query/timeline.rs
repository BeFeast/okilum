use super::{
    actions::{ActionRow, DisplayGroup, RowRenderer},
    *,
};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};
use serde_json::Value;
use std::cmp::Reverse;

pub(super) struct Date {
    instant: NaiveDateTime,
    label: String,
}

impl Date {
    pub(super) fn from_value(value: &Value) -> Option<Self> {
        let value = value.as_str()?;
        if let Ok(stamp) = DateTime::parse_from_rfc3339(value) {
            return Some(Self {
                instant: stamp.naive_utc(),
                label: stamp.format("%F\n%T %:z").to_string(),
            });
        }
        let day = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
        Some(Self {
            instant: day.and_time(NaiveTime::MIN),
            label: day.to_string(),
        })
    }
}

pub(super) fn sort(rows: &mut [ActionRow]) {
    // Keep the query order for ties and show undated rows at the end of their lane.
    rows.sort_by_key(|row| {
        let date = row.date.as_ref();
        (date.is_none(), date.map(|date| Reverse(date.instant)))
    });
}

pub(super) fn render(groups: &[DisplayGroup], rows: &[ActionRow], render: &RowRenderer) -> Div {
    let theme = render.cx.theme();
    v_flex()
        .w_full()
        .gap_3()
        .children(groups.iter().map(|group| {
            group
                .heading(render.cx)
                .child(v_flex().w_full().children(group.rows.clone().map(|index| {
                    let row = &rows[index];
                    let label = row.date.as_ref().map_or("No date", |date| &date.label);
                    h_flex()
                        .w_full()
                        .items_start()
                        .gap_2()
                        .child(
                            div()
                                .w(px(112.))
                                .flex_shrink_0()
                                .py_1()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(label.to_owned()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .border_l_1()
                                .border_color(theme.border)
                                .pl_2()
                                .child(render.row(index, row, true)),
                        )
                })))
        }))
}
