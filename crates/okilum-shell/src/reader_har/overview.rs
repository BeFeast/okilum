//! The right panel's Overview of a HAR capture (#2932): what the capture
//! holds at a glance, next to the list. The request list and its details
//! already show one request; the panel shows the whole capture: totals, the
//! split by type, status and host, and the slowest and largest requests.
//!
//! Type and status rows toggle the list's header filters, a host row puts
//! the host into Find, a request row selects that request; from Source each
//! of them goes back to the request list. Everything is
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
        let mut hosts: Vec<(&str, Tally)> = self
            .hosts
            .iter()
            .map(|(host, tally)| (host.as_ref(), *tally))
            .collect();
        let order = |a: &(&str, Tally), b: &(&str, Tally)| {
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
            .map(|(host, tally)| (Box::from(host), tally))
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

/// A panel row acts on the request list: from Source, it comes back first.
fn act(
    viewer: &Entity<HarView>,
    window: &mut Window,
    cx: &mut App,
    action: impl FnOnce(&mut HarView, &mut Window, &mut Context<HarView>),
) {
    viewer.update(cx, |this, cx| action(this, window, cx));
    if let Some(reader) = viewer.read(cx).reader.upgrade() {
        reader.update(cx, |reader, cx| reader.show_har_source(false, window, cx));
    }
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

/// One line of the Overview, as drawn and as copied: the panel, its copy
/// menu and «Copy overview» all read the same lines.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Line {
    Fact {
        id: &'static str,
        label: &'static str,
        value: String,
        /// Said under the value, e.g. how many sizes are unknown.
        note: Option<String>,
        danger: bool,
    },
    Heading(&'static str),
    Type(Kind, Tally),
    Status(StatusClass, usize),
    Host(usize, Box<str>, Tally),
    MoreHosts {
        hosts: usize,
        requests: usize,
    },
    /// A request under Slowest or Largest: its list, request and figure.
    Request(&'static str, usize, String),
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", model::count(n), if n == 1 { one } else { many })
}

/// «N of M unknown» for a total some requests have no size for.
fn unknown_note(unknown: usize, requests: usize) -> Option<String> {
    (unknown > 0 && unknown < requests).then(|| {
        format!(
            "{} of {} unknown",
            model::count(unknown),
            model::count(requests)
        )
    })
}

/// The Overview's lines, in panel order.
pub(crate) fn lines(view: &HarView) -> Vec<Line> {
    let creator = view.outcome().and_then(|o| o.creator.clone());
    lines_of(&view.stats, &view.overview, creator)
}

pub(crate) fn lines_of(stats: &Stats, overview: &Overview, creator: Option<String>) -> Vec<Line> {
    let fact = |id, label, value: String, note, danger| Line::Fact {
        id,
        label,
        value,
        note,
        danger,
    };
    let mut lines = vec![fact(
        "requests",
        "Requests",
        model::count(stats.requests),
        None,
        false,
    )];
    // Wire bytes and decoded bytes are different numbers; neither stands in
    // for the other, and a partial total says it is one (#2932).
    lines.push(fact(
        "transferred",
        "Transferred",
        stats
            .transferred_label()
            .unwrap_or_else(|| "Unknown".into()),
        unknown_note(stats.wire_unknown, stats.requests),
        false,
    ));
    lines.push(fact(
        "content",
        "Content size",
        stats.content_label().unwrap_or_else(|| "Unknown".into()),
        unknown_note(stats.content_unknown, stats.requests),
        false,
    ));
    if let Some(span) = stats.span_ms() {
        lines.push(fact(
            "duration",
            "Duration",
            model::millis(span),
            None,
            false,
        ));
    }
    lines.push(fact(
        "errors",
        "Errors",
        model::count(stats.errors),
        None,
        stats.errors > 0,
    ));
    if let Some(creator) = creator {
        lines.push(fact("creator", "Created by", creator, None, false));
    }
    let kinds: Vec<_> = Kind::ALL
        .into_iter()
        .zip(overview.kinds)
        .filter(|(_, tally)| tally.requests > 0)
        .map(|(kind, tally)| Line::Type(kind, tally))
        .collect();
    if !kinds.is_empty() {
        lines.push(Line::Heading("Types"));
        lines.extend(kinds);
    }
    let statuses: Vec<_> = StatusClass::ALL
        .into_iter()
        .zip(overview.statuses)
        .filter(|(_, n)| *n > 0)
        .map(|(class, n)| Line::Status(class, n))
        .collect();
    if !statuses.is_empty() {
        lines.push(Line::Heading("Status"));
        lines.extend(statuses);
    }
    if !overview.top_hosts.is_empty() {
        lines.push(Line::Heading("Domains"));
        for (position, (host, tally)) in overview.top_hosts.iter().enumerate() {
            lines.push(Line::Host(position, host.clone(), *tally));
        }
        let (hosts, requests) = overview.more_hosts();
        if hosts > 0 {
            lines.push(Line::MoreHosts { hosts, requests });
        }
    }
    if !overview.slowest.is_empty() {
        lines.push(Line::Heading("Slowest"));
        for &(index, ms) in &overview.slowest {
            lines.push(Line::Request("slowest", index, model::millis(ms)));
        }
    }
    if !overview.largest.is_empty() {
        lines.push(Line::Heading("Largest"));
        for &(index, bytes) in &overview.largest {
            lines.push(Line::Request("largest", index, model::bytes(bytes)));
        }
    }
    lines
}

/// A line's name and value as copied (Copy value, Copy name, Copy name:
/// value); `None` for headings.
pub(crate) fn pair(entries: &Entries, line: &Line) -> Option<(String, String)> {
    let figures = |tally: &Tally| {
        format!(
            "{} · {}",
            plural(tally.requests, "request", "requests"),
            model::bytes(tally.bytes)
        )
    };
    Some(match line {
        Line::Fact {
            label, value, note, ..
        } => (
            (*label).to_owned(),
            match note {
                Some(note) => format!("{value} ({note})"),
                None => value.clone(),
            },
        ),
        Line::Heading(_) => return None,
        Line::Type(kind, tally) => (kind.label().to_owned(), figures(tally)),
        Line::Status(class, n) => (class.label().to_owned(), plural(*n, "request", "requests")),
        Line::Host(_, host, tally) => (host.to_string(), figures(tally)),
        Line::MoreHosts { hosts, requests } => (
            format!("{} more", plural(*hosts, "host", "hosts")),
            plural(*requests, "request", "requests"),
        ),
        Line::Request(_, index, value) => {
            let entry = entries.get(*index)?;
            (format!("{} {}", entry.method, entry.url), value.clone())
        }
    })
}

/// The whole Overview as plain text (Copy overview).
pub(crate) fn text(lines: &[Line], entries: &Entries) -> String {
    let mut out = String::new();
    for line in lines {
        match line {
            Line::Heading(title) => {
                out.push('\n');
                out.push_str(title);
                out.push('\n');
            }
            Line::Fact { .. } => {
                if let Some((name, value)) = pair(entries, line) {
                    out.push_str(&format!("{name}: {value}\n"));
                }
            }
            _ => {
                if let Some((name, value)) = pair(entries, line) {
                    out.push_str(&format!("  {name}: {value}\n"));
                }
            }
        }
    }
    out
}

/// Puts `text` on the clipboard, as the details pane's copies do.
fn copy_text(text: String, window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    reader_toast::transient("Copied", window, cx);
}

/// What a row's context menu offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyWhat {
    Value,
    Name,
    Pair,
    Overview,
}

/// Copies line `line` (by its position in [`lines`]) or the whole Overview.
pub(crate) fn copy(
    viewer: &Entity<HarView>,
    line: usize,
    what: CopyWhat,
    window: &mut Window,
    cx: &mut App,
) {
    let view = viewer.read(cx);
    let text = match what {
        CopyWhat::Overview => Some(text(&lines(view), &view.entries)),
        _ => lines(view)
            .get(line)
            .and_then(|line| pair(&view.entries, line))
            .map(|(name, value)| match what {
                CopyWhat::Value => value,
                CopyWhat::Name => name,
                _ => format!("{name}: {value}"),
            }),
    };
    if let Some(text) = text {
        copy_text(text, window, cx);
    }
}

/// The copy menu of line `line`: its value, name and pair, or for a request
/// the list row's copies (URL, cURL, response body), then the whole Overview.
fn copy_menu(
    viewer: &Entity<HarView>,
    line: usize,
    request: Option<usize>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    let viewer = viewer.downgrade();
    move |mut menu: PopupMenu, _, _| {
        let item = |menu: PopupMenu, label: &'static str, what: CopyWhat| {
            let viewer = viewer.clone();
            menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                if let Some(viewer) = viewer.upgrade() {
                    copy(&viewer, line, what, window, cx);
                }
            }))
        };
        if let Some(index) = request {
            for what in RequestCopy::all() {
                let viewer = viewer.clone();
                menu = menu.item(PopupMenuItem::new(what.label()).on_click(
                    move |_, window, cx| {
                        let _ = viewer
                            .update(cx, |this, cx| this.copy_request(index, what, window, cx));
                    },
                ));
            }
        } else {
            menu = item(menu, "Copy value", CopyWhat::Value);
            menu = item(menu, "Copy name", CopyWhat::Name);
            menu = item(menu, "Copy name: value", CopyWhat::Pair);
        }
        item(menu.separator(), "Copy overview", CopyWhat::Overview)
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
    let total = view.stats.requests.max(1) as f32;
    let active_host = active_host(view, cx);
    let hover = brand::reader_palette(cx).hover;
    let mut body = v_flex()
        .id("har-overview")
        .debug_selector(|| "har-overview".into())
        .w_full()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .pt_1()
        .pb_3();
    for (position, line) in lines(view).into_iter().enumerate() {
        let target = viewer.clone();
        let element: AnyElement = match line {
            Line::Heading(title) => div()
                .flex_none()
                .px_4()
                .pt_3()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(faint)
                .child(title)
                .into_any_element(),
            Line::Fact {
                id,
                label,
                value,
                note,
                danger,
            } => v_flex()
                .id(SharedString::from(format!("har-overview-{id}")))
                .debug_selector(move || format!("har-overview-{id}"))
                .flex_none()
                .py(px(3.))
                .pl(px(16.))
                .pr_3()
                .hover(move |row| row.bg(hover))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
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
                                .text_color(if danger { p.danger } else { p.text })
                                .child(value),
                        ),
                )
                .when_some(note, |row, note| {
                    row.child(
                        div()
                            .debug_selector(move || format!("har-overview-{id}-note"))
                            .w_full()
                            .text_xs()
                            .text_align(TextAlign::Right)
                            .text_color(p.text_muted)
                            .child(note),
                    )
                })
                .context_menu(copy_menu(viewer, position, None))
                .into_any_element(),
            // Types: toggles the Type filter.
            Line::Type(kind, tally) => row(
                SharedString::from(format!("har-overview-type-{}", kind.label())),
                RowLabel::plain(kind.label().to_owned()),
                Some(tally.requests as f32 / total),
                &[count(tally.requests), size(tally.bytes)],
                view.filters.kind == Some(kind),
                cx,
            )
            .on_click(move |_, window, cx| {
                act(&target, window, cx, |this, _, cx| {
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
            })
            .context_menu(copy_menu(viewer, position, None))
            .into_any_element(),
            // Status: toggles the Status filter.
            Line::Status(class, n) => {
                let mut label = RowLabel::plain(class.label().to_owned());
                if matches!(
                    class,
                    StatusClass::ClientError | StatusClass::ServerError | StatusClass::Failed
                ) {
                    label.tone = Some(p.danger);
                }
                row(
                    SharedString::from(format!("har-overview-status-{}", class.label())),
                    label,
                    Some(n as f32 / total),
                    &[count(n)],
                    view.filters.status == Some(class),
                    cx,
                )
                .on_click(move |_, window, cx| {
                    act(&target, window, cx, |this, _, cx| {
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
                })
                .context_menu(copy_menu(viewer, position, None))
                .into_any_element()
            }
            // Domains: a host goes into Find (and comes out again).
            Line::Host(index, host, tally) => {
                let active = active_host == Some(index);
                let text: SharedString = host.to_string().into();
                let mut label = RowLabel::plain(text.to_string());
                label.tooltip = Some(text.clone());
                row(
                    SharedString::from(format!("har-overview-host-{index}")),
                    label,
                    Some(tally.requests as f32 / total),
                    &[count(tally.requests), size(tally.bytes)],
                    active,
                    cx,
                )
                .on_click(move |_, window, cx| {
                    let text = text.clone();
                    act(&target, window, cx, |this, window, cx| {
                        let value = if active { "".into() } else { text };
                        this.filter
                            .update(cx, |input, cx| input.set_value(value, window, cx));
                        this.refresh(cx);
                        cx.notify();
                    })
                })
                .context_menu(copy_menu(viewer, position, None))
                .into_any_element()
            }
            Line::MoreHosts { hosts, requests } => div()
                .id("har-overview-more-hosts")
                .debug_selector(|| "har-overview-more-hosts".into())
                .flex_none()
                .pl(px(16.))
                .pr_3()
                .py_1()
                .text_color(p.text_muted)
                .child(format!(
                    "{} more · {}",
                    plural(hosts, "host", "hosts"),
                    plural(requests, "request", "requests")
                ))
                .context_menu(copy_menu(viewer, position, None))
                .into_any_element(),
            // Slowest and Largest: a click selects the request.
            Line::Request(list, index, value) => {
                let Some(entry) = view.entries.get(index) else {
                    continue;
                };
                let mut label = RowLabel::plain(request_name(entry));
                label.tooltip = Some(format!("{} {}", entry.method, entry.url).into());
                if entry.failed() {
                    label.tone = Some(p.danger);
                }
                row(
                    SharedString::from(format!("har-overview-{list}-{index}")),
                    label,
                    None,
                    &[value_cell(value)],
                    view.selected == Some(index),
                    cx,
                )
                .on_click(move |_, window, cx| {
                    act(&target, window, cx, |this, _, cx| this.reveal(index, cx))
                })
                .context_menu(copy_menu(viewer, position, Some(index)))
                .into_any_element()
            }
        };
        body = body.child(element);
    }
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

    /// Indexes `har` with the real HAR reader and tallies it as the view does.
    fn tally(har: &str) -> (Stats, Overview, Entries, Option<String>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.har");
        std::fs::write(&path, har).unwrap();
        let mut list = Vec::new();
        let outcome = okilum_core::har::index_file(
            &path,
            &std::sync::atomic::AtomicBool::new(false),
            &mut |batch| list.extend(batch.entries),
        )
        .unwrap();
        assert_eq!(outcome.problem, None);
        let (mut stats, mut overview, mut entries) =
            (Stats::default(), Overview::default(), Entries::default());
        for (index, entry) in list.iter().enumerate() {
            stats.add(entry);
            overview.add(index, entry);
        }
        overview.finish();
        entries.push(list);
        (stats, overview, entries, outcome.creator)
    }

    fn fact(lines: &[Line], wanted: &str) -> (String, Option<String>) {
        lines
            .iter()
            .find_map(|line| match line {
                Line::Fact {
                    id, value, note, ..
                } if *id == wanted => Some((value.clone(), note.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{wanted} line"))
    }

    /// Sol review of #2932: «Transferred» counts wire bytes only. A request
    /// whose wire size is unknown is never counted with its decoded size;
    /// the total says it is partial and by how many requests.
    #[test]
    fn review2932_wire_and_content_sizes_stay_apart() {
        let response = |sizes: &str, content: i64| {
            format!(
                r#"{{"startedDateTime":"2026-11-14T10:00:00.000Z","time":10,"request":{{"method":"GET","url":"https://a.test/r","headers":[]}},"response":{{"status":200,"headers":[],{sizes},"content":{{"size":{content},"mimeType":"text/plain"}}}}}}"#
            )
        };
        let entries = [
            // Compressed: 300 bytes on the wire for 1000 decoded.
            response(
                r#""headersSize":-1,"bodySize":-1,"_transferSize":300"#,
                1000,
            ),
            // Unknown: no wire size at all; 1 MB decoded must not count.
            response(r#""headersSize":-1,"bodySize":-1"#, 1_000_000),
            // From the cache: zero bytes on the wire is a known size.
            response(r#""headersSize":-1,"bodySize":-1,"_transferSize":0"#, 5000),
            // Headers without a body size: still unknown, not 200 bytes.
            response(r#""headersSize":200,"bodySize":-1"#, 50),
            // Header and body sizes, no `_transferSize`.
            response(r#""headersSize":100,"bodySize":400"#, 900),
        ];
        let har = format!(
            r#"{{"log":{{"version":"1.2","creator":{{"name":"t","version":"1"}},"entries":[{}]}}}}"#,
            entries.join(",")
        );
        let (stats, overview, list, creator) = tally(&har);
        assert_eq!(stats.requests, 5);
        assert_eq!((stats.transferred, stats.wire_unknown), (800, 2));
        assert_eq!((stats.content, stats.content_unknown), (1_006_950, 0));
        assert_eq!(stats.transferred_label().as_deref(), Some("≥ 800 B"));
        assert_eq!(stats.content_label().as_deref(), Some("1.0 MB"));
        let lines = lines_of(&stats, &overview, creator);
        assert_eq!(
            fact(&lines, "transferred"),
            ("≥ 800 B".into(), Some("2 of 5 unknown".into()))
        );
        assert_eq!(fact(&lines, "content"), ("1.0 MB".into(), None));
        let copied = text(&lines, &list);
        assert!(
            copied.contains("Transferred: ≥ 800 B (2 of 5 unknown)\n"),
            "{copied}"
        );
        assert!(copied.contains("Content size: 1.0 MB\n"), "{copied}");

        // No wire size anywhere: unknown, never zero or the decoded size.
        let har = format!(
            r#"{{"log":{{"entries":[{}]}}}}"#,
            response(r#""headersSize":-1,"bodySize":-1"#, 4096)
        );
        let (stats, overview, _, creator) = tally(&har);
        assert_eq!(stats.transferred_label(), None);
        let lines = lines_of(&stats, &overview, creator);
        assert_eq!(fact(&lines, "transferred"), ("Unknown".into(), None));
        assert_eq!(fact(&lines, "content"), ("4.1 KB".into(), None));
    }

    /// Every drawn line has a copyable name and value, and Copy overview
    /// holds them all, sections included.
    #[test]
    fn review2932_overview_lines_copy_as_text() {
        let har = r#"{"log":{"creator":{"name":"WebInspector","version":"537.36"},"entries":[
            {"startedDateTime":"2026-11-14T10:00:00.000Z","time":40,"request":{"method":"GET","url":"https://a.test/app.js","headers":[]},"response":{"status":200,"headers":[],"_transferSize":300,"content":{"size":1000,"mimeType":"application/javascript"}}},
            {"startedDateTime":"2026-11-14T10:00:00.100Z","time":90,"request":{"method":"POST","url":"https://b.test/api?x=1","headers":[]},"response":{"status":404,"headers":[],"_transferSize":120,"content":{"size":20,"mimeType":"application/json"}},"_resourceType":"fetch"}
        ]}}"#;
        let (stats, overview, list, creator) = tally(har);
        let lines = lines_of(&stats, &overview, creator);
        for line in &lines {
            assert_eq!(
                pair(&list, line).is_some(),
                !matches!(line, Line::Heading(_)),
                "{line:?}"
            );
        }
        let script = lines
            .iter()
            .find(|l| matches!(l, Line::Type(Kind::Script, _)))
            .unwrap();
        assert_eq!(
            pair(&list, script),
            Some(("Script".into(), "1 request · 300 B".into()))
        );
        let slowest = lines
            .iter()
            .find(|l| matches!(l, Line::Request("slowest", 1, _)))
            .unwrap();
        assert_eq!(
            pair(&list, slowest),
            Some(("POST https://b.test/api?x=1".into(), "90 ms".into()))
        );
        let copied = text(&lines, &list);
        for wanted in [
            "Requests: 2\n",
            "Transferred: 420 B\n",
            "Errors: 1\n",
            "Created by: WebInspector 537.36\n",
            "\nTypes\n  Fetch/XHR: 1 request · 120 B\n  Script: 1 request · 300 B\n",
            "\nStatus\n  2xx: 1 request\n  4xx: 1 request\n",
            "\nDomains\n",
            "  a.test: 1 request · 300 B\n",
            "\nSlowest\n  POST https://b.test/api?x=1: 90 ms\n",
        ] {
            assert!(copied.contains(wanted), "{wanted:?} in {copied}");
        }
    }

    /// The tally costs a few array updates per request plus one host sort per
    /// applied batch set: a 200 000-request capture over 10 000 hosts, sorted
    /// every 1 000 requests, stays far below a frame budget per batch.
    #[test]
    fn review2932_tallying_a_large_capture_is_cheap() {
        let entries: Vec<Entry> = (0..okilum_core::har::MAX_ENTRIES)
            .map(|i| {
                entry(
                    &format!("https://h{}.test/r/{i}", i % HOSTS_KEPT),
                    Some(200),
                    Some((i % 997) as f64),
                    (i % 4093) as i64,
                )
            })
            .collect();
        let mut overview = Overview::default();
        let started = std::time::Instant::now();
        let mut slowest_batch = std::time::Duration::ZERO;
        for (batch, chunk) in entries.chunks(1000).enumerate() {
            let at = std::time::Instant::now();
            for (offset, entry) in chunk.iter().enumerate() {
                overview.add(batch * 1000 + offset, entry);
            }
            overview.finish();
            slowest_batch = slowest_batch.max(at.elapsed());
        }
        let total = started.elapsed();
        println!("review2932 tally: {total:?} total, slowest batch {slowest_batch:?}");
        assert_eq!(overview.top_hosts.len(), HOSTS_SHOWN);
        assert_eq!(overview.slowest.len(), TOP);
        // Generous for unoptimised test builds on shared runners.
        assert!(
            slowest_batch < std::time::Duration::from_millis(250),
            "{slowest_batch:?}"
        );
    }
}
