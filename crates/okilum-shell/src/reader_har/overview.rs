//! The right panel's Overview of a HAR capture (#2932): what the capture
//! holds at a glance, next to the list. The request list and its details
//! already show one request; the panel shows the whole capture: totals, the
//! split by type, status and host, and the slowest and largest requests.
//!
//! Type and status rows toggle the list's header filters, a host row puts
//! the host into Find, a request row selects that request. Everything is
//! tallied while the index delivers batches, so drawing the panel never
//! walks the requests. Nothing here writes capture content anywhere but the
//! panel.
use super::*;
use okilum_core::har::Kind;
use std::collections::HashMap;

/// Requests listed under Slowest and Largest.
pub(crate) const TOP: usize = 5;
/// Hosts listed under Domains.
pub(crate) const HOSTS_SHOWN: usize = 8;
/// Distinct hosts tallied by name; requests to further hosts are counted
/// together, so a capture of many hosts cannot grow the map without bound.
const HOSTS_KEPT: usize = 10_000;

/// Requests and bytes on the wire (the list's Size column) of a group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally {
    pub requests: usize,
    pub bytes: i64,
}

impl Tally {
    fn add(&mut self, bytes: i64) {
        self.requests += 1;
        self.bytes = self.bytes.saturating_add(bytes.max(0));
    }
}

#[derive(Default)]
pub(crate) struct Overview {
    /// By [`Kind::ALL`] position.
    pub kinds: [Tally; Kind::ALL.len()],
    /// By [`StatusClass::ALL`] position.
    pub statuses: [usize; StatusClass::ALL.len()],
    hosts: HashMap<Box<str>, Tally>,
    /// Requests to hosts beyond [`HOSTS_KEPT`].
    hosts_other: Tally,
    /// The most requested hosts, most first; refreshed by [`Overview::finish`].
    pub top_hosts: Vec<(Box<str>, Tally)>,
    /// `(request, ms)`, slowest first; equal times keep file order.
    pub slowest: Vec<(usize, f64)>,
    /// `(request, bytes)`, largest first; equal sizes keep file order.
    pub largest: Vec<(usize, i64)>,
    /// Changes whenever anything above may have changed.
    pub revision: u64,
}

impl Overview {
    /// Counts request `index` (its position in file order).
    pub(crate) fn add(&mut self, index: usize, entry: &Entry) {
        let size = model::shown_size(entry).unwrap_or(0);
        let kind = entry.kind();
        if let Some(slot) = Kind::ALL.iter().position(|k| *k == kind) {
            self.kinds[slot].add(size);
        }
        if let Some(slot) = StatusClass::ALL.iter().position(|c| c.matches(entry)) {
            self.statuses[slot] += 1;
        }
        let host = entry.host();
        if !host.is_empty() {
            if let Some(tally) = self.hosts.get_mut(host) {
                tally.add(size);
            } else if self.hosts.len() < HOSTS_KEPT {
                self.hosts.entry(host.into()).or_default().add(size);
            } else {
                self.hosts_other.add(size);
            }
        }
        if let Some(ms) = entry.time.filter(|t| t.is_finite() && *t > 0.) {
            keep_top(&mut self.slowest, (index, ms), |a, b| a.total_cmp(b));
        }
        if size > 0 {
            keep_top(&mut self.largest, (index, size), Ord::cmp);
        }
    }

    /// After a set of batches: orders the hosts once, not on every draw.
    pub(crate) fn finish(&mut self) {
        let mut hosts: Vec<(&Box<str>, &Tally)> = self.hosts.iter().collect();
        let order = |a: &(&Box<str>, &Tally), b: &(&Box<str>, &Tally)| {
            b.1.requests
                .cmp(&a.1.requests)
                .then(b.1.bytes.cmp(&a.1.bytes))
                .then(a.0.cmp(b.0))
        };
        if hosts.len() > HOSTS_SHOWN {
            hosts.select_nth_unstable_by(HOSTS_SHOWN - 1, order);
            hosts.truncate(HOSTS_SHOWN);
        }
        hosts.sort_unstable_by(order);
        self.top_hosts = hosts
            .into_iter()
            .map(|(host, tally)| (host.clone(), *tally))
            .collect();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Hosts not listed under Domains, and their requests.
    pub(crate) fn more_hosts(&self) -> (usize, usize) {
        let listed: usize = self.top_hosts.iter().map(|(_, t)| t.requests).sum();
        let total: usize =
            self.hosts.values().map(|t| t.requests).sum::<usize>() + self.hosts_other.requests;
        let hosts =
            self.hosts.len() - self.top_hosts.len() + usize::from(self.hosts_other.requests > 0);
        (hosts, total - listed)
    }

    /// Empty again for a reindex; the revision still moves on.
    pub(crate) fn reset(&mut self) {
        let revision = self.revision.wrapping_add(1);
        *self = Self {
            revision,
            ..Self::default()
        };
    }
}

/// Inserts `(index, value)` into a descending list of at most [`TOP`].
/// Indices arrive in increasing order, so a tie stays behind earlier ones.
fn keep_top<T: Copy>(
    list: &mut Vec<(usize, T)>,
    item: (usize, T),
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
) {
    let at = list.partition_point(|(_, value)| compare(value, &item.1).is_ge());
    if at < TOP {
        list.insert(at, item);
        list.truncate(TOP);
    }
}

/// What the panel shows of the view. The panel redraws only when this
/// changes: typing in Find changes it only when the text becomes or stops
/// being a listed host (#2836 keeps Find typing off the side panels).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Key {
    revision: u64,
    filters: Filters,
    host: Option<usize>,
    selected: Option<usize>,
    done: bool,
}

pub(crate) fn key(view: &HarView, cx: &App) -> Key {
    Key {
        revision: view.overview.revision,
        filters: view.filters.clone(),
        host: active_host(view, cx),
        selected: view.selected,
        done: !view.reading(),
    }
}

/// The listed host Find holds, if any.
fn active_host(view: &HarView, cx: &App) -> Option<usize> {
    let text = view.filter.read(cx).value();
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    view.overview
        .top_hosts
        .iter()
        .position(|(host, _)| host.eq_ignore_ascii_case(text))
}

/// The last path segment without its query, or the path, or the host.
pub(crate) fn request_name(entry: &Entry) -> String {
    let path = entry.path();
    let bare = path.split(['?', '#']).next().unwrap_or(path);
    let tail = bare.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if !tail.is_empty() {
        tail.to_owned()
    } else if !entry.host().is_empty() {
        entry.host().to_owned()
    } else {
        path.to_owned()
    }
}

/// The Overview under the panel's heading. `None` while there is nothing to
/// sum up (no request read yet, or a file with no requests).
pub(crate) fn render(viewer: &Entity<HarView>, cx: &App) -> Option<AnyElement> {
    let view = viewer.read(cx);
    if view.stats.requests == 0 {
        return None;
    }
    let p = brand::palette(cx);
    let faint = brand::reader_palette(cx).text_faint;
    let overview = &view.overview;
    let total = view.stats.requests.max(1) as f32;
    let mut body = v_flex()
        .id("har-overview")
        .debug_selector(|| "har-overview".into())
        .w_full()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .pb_3();

    // Capture: the totals.
    let fact = |id: &'static str, label: &'static str, value: String, tone: Option<Hsla>| {
        h_flex()
            .debug_selector(move || format!("har-overview-{id}"))
            .flex_none()
            .h(px(22.))
            .items_center()
            .gap_2()
            .pl(px(16.))
            .pr_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(p.text_muted)
                    .child(label),
            )
            .child(
                div()
                    .flex_none()
                    .min_w_0()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_color(tone.unwrap_or(p.text))
                    .child(value),
            )
    };
    body = body
        .pt_1()
        .child(fact(
            "requests",
            "Requests",
            model::count(view.stats.requests),
            None,
        ))
        .child(fact(
            "transferred",
            "Transferred",
            model::bytes(view.stats.transferred),
            None,
        ))
        .when_some(view.stats.span_ms(), |body, span| {
            body.child(fact("duration", "Duration", model::millis(span), None))
        })
        .child(fact(
            "errors",
            "Errors",
            model::count(view.stats.errors),
            (view.stats.errors > 0).then_some(p.danger),
        ))
        .when_some(
            view.outcome().and_then(|o| o.creator.clone()),
            |body, creator| body.child(fact("creator", "Created by", creator, None)),
        );

    let heading = |label: &'static str| {
        div()
            .flex_none()
            .px_4()
            .pt_3()
            .pb_1()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(faint)
            .child(label)
    };

    // Types: toggles the Type filter.
    let kinds: Vec<_> = Kind::ALL
        .into_iter()
        .zip(overview.kinds)
        .filter(|(_, tally)| tally.requests > 0)
        .collect();
    if !kinds.is_empty() {
        body = body.child(heading("Types"));
        for (kind, tally) in kinds {
            let active = view.filters.kind == Some(kind);
            let target = viewer.clone();
            body = body.child(
                row(
                    SharedString::from(format!("har-overview-type-{}", kind.label())),
                    RowLabel::plain(kind.label().to_owned()),
                    Some(tally.requests as f32 / total),
                    &[count(tally.requests), size(tally.bytes)],
                    active,
                    cx,
                )
                .on_click(move |_, _, cx| {
                    target.update(cx, |this, cx| {
                        this.set_filters(
                            |f| {
                                f.kind = if f.kind == Some(kind) {
                                    None
                                } else {
                                    Some(kind)
                                }
                            },
                            cx,
                        )
                    })
                }),
            );
        }
    }

    // Status: toggles the Status filter.
    let statuses: Vec<_> = StatusClass::ALL
        .into_iter()
        .zip(overview.statuses)
        .filter(|(_, n)| *n > 0)
        .collect();
    if !statuses.is_empty() {
        body = body.child(heading("Status"));
        for (class, n) in statuses {
            let active = view.filters.status == Some(class);
            let error = matches!(
                class,
                StatusClass::ClientError | StatusClass::ServerError | StatusClass::Failed
            );
            let target = viewer.clone();
            let mut label = RowLabel::plain(class.label().to_owned());
            if error {
                label.tone = Some(p.danger);
            }
            body = body.child(
                row(
                    SharedString::from(format!("har-overview-status-{}", class.label())),
                    label,
                    Some(n as f32 / total),
                    &[count(n)],
                    active,
                    cx,
                )
                .on_click(move |_, _, cx| {
                    target.update(cx, |this, cx| {
                        this.set_filters(
                            |f| {
                                f.status = if f.status == Some(class) {
                                    None
                                } else {
                                    Some(class)
                                }
                            },
                            cx,
                        )
                    })
                }),
            );
        }
    }

    // Domains: a host goes into Find (and comes out again).
    if !overview.top_hosts.is_empty() {
        let active_host = active_host(view, cx);
        body = body.child(heading("Domains"));
        for (position, (host, tally)) in overview.top_hosts.iter().enumerate() {
            let active = active_host == Some(position);
            let target = viewer.clone();
            let text: SharedString = host.to_string().into();
            let mut label = RowLabel::plain(text.to_string());
            label.tooltip = Some(text.clone());
            body = body.child(
                row(
                    SharedString::from(format!("har-overview-host-{position}")),
                    label,
                    Some(tally.requests as f32 / total),
                    &[count(tally.requests), size(tally.bytes)],
                    active,
                    cx,
                )
                .on_click(move |_, window, cx| {
                    let text = text.clone();
                    target.update(cx, |this, cx| {
                        let value = if active { "".into() } else { text };
                        this.filter
                            .update(cx, |input, cx| input.set_value(value, window, cx));
                        this.refresh(cx);
                        cx.notify();
                    })
                }),
            );
        }
        let (hosts, requests) = overview.more_hosts();
        if hosts > 0 {
            body = body.child(
                div()
                    .debug_selector(|| "har-overview-more-hosts".into())
                    .flex_none()
                    .pl(px(16.))
                    .pr_3()
                    .py_1()
                    .text_color(p.text_muted)
                    .child(format!(
                        "{} more {} · {} {}",
                        model::count(hosts),
                        if hosts == 1 { "host" } else { "hosts" },
                        model::count(requests),
                        if requests == 1 { "request" } else { "requests" }
                    )),
            );
        }
    }

    // Slowest and Largest: a click selects the request.
    let requests = |title: &'static str, id: &'static str, items: Vec<(usize, String)>| {
        let mut section = v_flex().flex_none();
        if items.is_empty() {
            return section;
        }
        section = section.child(heading(title));
        for (index, value) in items {
            let Some(entry) = view.entries.get(index) else {
                continue;
            };
            let target = viewer.clone();
            let mut label = RowLabel::plain(request_name(entry));
            label.tooltip = Some(format!("{} {}", entry.method, entry.url).into());
            if entry.failed() {
                label.tone = Some(p.danger);
            }
            section = section.child(
                row(
                    SharedString::from(format!("har-overview-{id}-{index}")),
                    label,
                    None,
                    &[value_cell(value)],
                    view.selected == Some(index),
                    cx,
                )
                .on_click(move |_, _, cx| target.update(cx, |this, cx| this.reveal(index, cx))),
            );
        }
        section
    };
    body = body
        .child(requests(
            "Slowest",
            "slowest",
            overview
                .slowest
                .iter()
                .map(|&(index, ms)| (index, model::millis(ms)))
                .collect(),
        ))
        .child(requests(
            "Largest",
            "largest",
            overview
                .largest
                .iter()
                .map(|&(index, bytes)| (index, model::bytes(bytes)))
                .collect(),
        ));
    Some(body.into_any_element())
}

struct RowLabel {
    text: String,
    tone: Option<Hsla>,
    tooltip: Option<SharedString>,
}

impl RowLabel {
    fn plain(text: String) -> Self {
        Self {
            text,
            tone: None,
            tooltip: None,
        }
    }
}

const COUNT_WIDTH: f32 = 44.;
const SIZE_WIDTH: f32 = 68.;

fn count(n: usize) -> (String, f32) {
    (model::count(n), COUNT_WIDTH)
}

fn size(bytes: i64) -> (String, f32) {
    (model::bytes(bytes), SIZE_WIDTH)
}

fn value_cell(value: String) -> (String, f32) {
    (value, SIZE_WIDTH)
}

/// One clickable panel row: the label with an optional share bar under it,
/// then right-aligned figures. Same height, hover and «current» accent bar as
/// the Contents rows it replaces.
fn row(
    id: SharedString,
    label: RowLabel,
    share: Option<f32>,
    values: &[(String, f32)],
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    let p = brand::palette(cx);
    let hover = brand::reader_palette(cx).hover;
    let selector = id.clone();
    let tooltip = label.tooltip.clone();
    h_flex()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex_none()
        .h(px(OUTLINE_ROW_HEIGHT))
        .items_center()
        .gap_2()
        .pl(px(14.))
        .pr_3()
        .border_l_2()
        .border_color(gpui::transparent_black())
        .text_color(p.text)
        .when(active, |row| {
            row.border_color(p.accent).font_weight(FontWeight::MEDIUM)
        })
        .overflow_hidden()
        .cursor_pointer()
        .hover(move |row| row.bg(hover))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    div()
                        .min_w_0()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .when_some(label.tone, |text, tone| text.text_color(tone))
                        .child(label.text),
                )
                .when_some(share, |column, share| {
                    column.child(
                        div()
                            .w_full()
                            .h(px(3.))
                            .rounded(px(2.))
                            .bg(p.border_subtle)
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(share.clamp(0.01, 1.)))
                                    .rounded(px(2.))
                                    .bg(p.accent.opacity(if active { 0.9 } else { 0.55 })),
                            ),
                    )
                }),
        )
        .children(values.iter().map(|(value, width)| {
            div()
                .flex_none()
                .w(px(*width))
                .text_align(TextAlign::Right)
                .text_color(p.text_muted)
                .whitespace_nowrap()
                .child(value.clone())
        }))
        .when_some(tooltip, |row, tooltip| {
            row.tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn entry(url: &str, status: Option<i64>, time: Option<f64>, size: i64) -> Entry {
        Entry {
            method: "GET".into(),
            url: url.into(),
            status,
            mime: "application/javascript".into(),
            time,
            transferred: Some(size),
            ..Entry::default()
        }
    }

    #[test]
    fn tallies_types_statuses_hosts_and_the_top_requests() {
        let mut overview = Overview::default();
        let entries = [
            entry("https://a.test/app.js", Some(200), Some(30.), 100),
            entry("https://a.test/x.js?v=1", Some(404), Some(90.), 50),
            entry("https://b.test/", Some(0), None, 0),
            entry("https://a.test/y.js", Some(200), Some(90.), 400),
        ];
        for (index, entry) in entries.iter().enumerate() {
            overview.add(index, entry);
        }
        let before = overview.revision;
        overview.finish();
        assert_ne!(overview.revision, before, "a finish moves the revision");
        let script = Kind::ALL.iter().position(|k| *k == Kind::Script).unwrap();
        assert_eq!(
            overview.kinds[script],
            Tally {
                requests: 4,
                bytes: 550
            }
        );
        let slot = |class| StatusClass::ALL.iter().position(|c| *c == class).unwrap();
        assert_eq!(overview.statuses[slot(StatusClass::Success)], 2);
        assert_eq!(overview.statuses[slot(StatusClass::ClientError)], 1);
        assert_eq!(overview.statuses[slot(StatusClass::Failed)], 1);
        let hosts: Vec<_> = overview
            .top_hosts
            .iter()
            .map(|(h, t)| (h.as_ref(), t.requests))
            .collect();
        assert_eq!(hosts, [("a.test", 3), ("b.test", 1)]);
        assert_eq!(overview.more_hosts(), (0, 0));
        // Equal times keep file order; no time is not a time.
        assert_eq!(overview.slowest, [(1, 90.), (3, 90.), (0, 30.)]);
        assert_eq!(overview.largest, [(3, 400), (0, 100), (1, 50)]);
        assert_eq!(request_name(&entries[1]), "x.js");
        assert_eq!(request_name(&entries[2]), "b.test");
    }

    #[test]
    fn top_lists_and_hosts_stay_bounded() {
        let mut overview = Overview::default();
        for index in 0..(HOSTS_SHOWN + 20) {
            let url = format!("https://h{index}.test/r");
            let entry = entry(&url, Some(200), Some(index as f64 + 1.), index as i64 + 1);
            overview.add(index, &entry);
        }
        // One host with more requests leads.
        for index in 0..3 {
            overview.add(
                100 + index,
                &entry("https://h5.test/again", Some(200), None, 0),
            );
        }
        overview.finish();
        assert_eq!(overview.slowest.len(), TOP);
        assert_eq!(overview.largest.len(), TOP);
        assert_eq!(overview.slowest[0].0, HOSTS_SHOWN + 19);
        assert_eq!(overview.top_hosts.len(), HOSTS_SHOWN);
        assert_eq!(overview.top_hosts[0].0.as_ref(), "h5.test");
        assert_eq!(overview.top_hosts[0].1.requests, 4);
        let (hosts, requests) = overview.more_hosts();
        assert_eq!(hosts, 20);
        assert_eq!(requests, 20);
        overview.reset();
        assert!(overview.top_hosts.is_empty() && overview.slowest.is_empty());
    }

    #[test]
    fn hosts_beyond_the_kept_number_are_counted_together() {
        let mut overview = Overview::default();
        for index in 0..(HOSTS_KEPT + 3) {
            let url = format!("https://h{index}.test/");
            overview.add(index, &entry(&url, Some(200), None, 1));
        }
        overview.finish();
        assert_eq!(overview.hosts.len(), HOSTS_KEPT);
        let (hosts, requests) = overview.more_hosts();
        assert_eq!(hosts, HOSTS_KEPT - HOSTS_SHOWN + 1);
        assert_eq!(requests, HOSTS_KEPT + 3 - HOSTS_SHOWN);
    }
}
