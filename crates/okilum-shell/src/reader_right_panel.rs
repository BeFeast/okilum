//! Right-panel section labels and empty states (#646).
//!
//! Counts appear only when they are above zero. An empty section keeps its
//! header and shows one muted line, never «0 notes · 0 places».

use super::*;
#[path = "reader_file_outline.rs"]
pub(super) mod file_outline;

/// Attachment previews share the note outline's typography and independent
/// scroll areas, while unstructured files keep only their backlinks section.
pub(super) fn render_file(
    reader: &Reader,
    outline: Entity<file_outline::View>,
    cx: &mut Context<Reader>,
) -> AnyElement {
    file_outline::View::bind(&outline, reader, cx);
    let p = brand::palette(cx);
    let faint = brand::reader_palette(cx).text_faint;
    let section = |icon: &'static str, label: String| {
        h_flex()
            .flex_none()
            .gap_1p5()
            .px_4()
            .pt_3()
            .pb_1p5()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(faint)
            .child(Icon::default().path(icon).xsmall().text_color(faint))
            .child(label)
    };
    // Data and service files (code viewer formats, HAR, logs, plain text):
    // an empty «Linked from» only takes space (#2885), so it appears only
    // when a note actually links the file; Contents uses the room otherwise.
    // Notes and documents always keep it.
    let service = service_file(reader) && reader.pane.backlinks.is_empty();
    // A HAR file: its Overview takes the place of Contents, which has
    // nothing to list in either view (#2932; Source has no JSON Contents for
    // HAR, `contents_format`).
    let har = reader
        .pane
        .file_preview
        .as_ref()
        .and_then(|preview| preview.har.clone());
    let contents = har.is_none() && outline.read(cx).has_entries();
    let separator = || {
        div()
            .flex_none()
            .h(px(1.))
            .mx_4()
            .my_1()
            .bg(p.border_subtle)
    };
    v_flex()
        .w_full()
        .h_full()
        .flex_none()
        .text_size(px(brand::READER_CHROME_FONT_SIZE))
        .when_some(har.as_ref(), |panel, har| {
            let overview = reader_har::overview::render(har, cx);
            let empty = overview.is_none();
            panel
                .child(
                    section("icons/activity.svg", "Overview".into())
                        .debug_selector(|| "har-overview-heading".into()),
                )
                .children(overview)
                .when(empty, |panel| {
                    panel.child(
                        div()
                            .flex_none()
                            .px_4()
                            .py_1()
                            .text_color(p.text_muted)
                            .child(contents_empty(true)),
                    )
                })
        })
        .when(har.is_none() && (contents || service), |panel| {
            // Editing text that does not parse: the rows are of the last valid
            // text. The heading says so; nothing is inserted above the list.
            let stale = outline.read(cx).last_valid_tooltip();
            panel.child(
                section("icons/list.svg", "Contents".into())
                    .debug_selector(|| "file-contents-heading".into())
                    .when_some(stale, |heading, tooltip| {
                        heading.child(
                            h_flex()
                                .id("file-contents-last-valid")
                                .debug_selector(|| "file-contents-last-valid".into())
                                .ml_auto()
                                .gap_1()
                                .font_weight(FontWeight::NORMAL)
                                .text_color(p.warning)
                                .child(Icon::new(IconName::TriangleAlert).xsmall())
                                .child("Last valid text")
                                .tooltip(move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(tooltip.clone())
                                        .build(window, cx)
                                }),
                        )
                    }),
            )
        })
        .when(contents, |panel| panel.child(outline))
        .when(har.is_none() && service && !contents, |panel| {
            panel.child(
                div()
                    .flex_none()
                    .px_4()
                    .py_1()
                    .text_color(p.text_muted)
                    .child(contents_empty(true)),
            )
        })
        .when(!service, |panel| {
            panel
                .when(contents || har.is_some(), |panel| panel.child(separator()))
                .child(
                    section("icons/link.svg", reader.linked_from_title())
                        .debug_selector(|| "file-linked-from-heading".into()),
                )
                .child(reader.render_backlinks(cx))
        })
        .into_any_element()
}

/// Files the code viewer, the HAR viewer, the log viewer or the plain-text
/// viewer shows: data and service files.
pub(super) fn service_file(reader: &Reader) -> bool {
    reader.pane.file_preview.as_ref().is_some_and(|preview| {
        preview.code.is_some()
            || preview.har.is_some()
            || preview.log.is_some()
            || preview.text.is_some()
    })
}

/// Distinct linking notes and link places. `paths` arrive grouped by source
/// note, as `Vault::backlinks` returns them.
pub(super) fn link_counts<'a>(paths: impl IntoIterator<Item = &'a str>) -> (usize, usize) {
    let mut notes = 0;
    let mut places = 0;
    let mut previous: Option<&str> = None;
    for path in paths {
        places += 1;
        if previous != Some(path) {
            notes += 1;
            previous = Some(path);
        }
    }
    (notes, places)
}

/// «Linked from», with «N notes · M places» only when something links here.
/// `partial` (unreadable items) is only worth a word next to real counts; the
/// empty line already says the search covered readable items only.
pub(super) fn linked_from_title(
    scanned: bool,
    notes: usize,
    places: usize,
    partial: bool,
) -> String {
    if !scanned || places == 0 {
        return "Linked from".into();
    }
    let label = format!(
        "Linked from · {notes} {} · {places} {}",
        if notes == 1 { "note" } else { "notes" },
        if places == 1 { "place" } else { "places" }
    );
    if partial {
        format!("{label} · partial")
    } else {
        label
    }
}

/// The panel with no note open.
pub(super) const NO_NOTE: &str = "Open a note to see its headings and the notes that link to it.";

/// Empty sections remain quiet, including while inventory is pending.
pub(super) fn linked_from_empty(_scanned: bool, _complete: bool) -> &'static str {
    "—"
}

/// The same quiet empty state applies to notes and attachment previews.
pub(super) fn contents_empty(_file_preview: bool) -> &'static str {
    "—"
}

/// The one muted line an empty right-panel section shows under its header
/// (#646), aligned with the section label.
fn panel_empty_line(color: Hsla, text: &'static str) -> AnyElement {
    div()
        .px_4()
        .py_1()
        .text_sm()
        .text_color(color)
        .child(text)
        .into_any_element()
}

impl Reader {
    /// Replace the notes that link here. The groups (one card each) and the list that shows
    /// them are rebuilt together, so the list can never count cards that are not there (#1135).
    ///
    /// A background refresh that finds the same notes linking here (an unrelated file was edited,
    /// the inventory was reconciled) keeps the scroll position and only measures the cards
    /// again; opening another note (`new_note`) or a changed set starts from the top.
    pub(super) fn set_backlinks(&mut self, backlinks: Vec<Backlink>, new_note: bool) {
        let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
        for (ix, b) in backlinks.iter().enumerate() {
            match groups.last_mut() {
                Some(group) if backlinks[group.start].path == b.path => group.end = ix + 1,
                _ => groups.push(ix..ix + 1),
            }
        }
        let same_cards = !new_note
            && groups.len() == self.pane.backlink_groups.len()
            && groups
                .iter()
                .zip(&self.pane.backlink_groups)
                .all(|(new, old)| {
                    new.len() == old.len()
                        && backlinks[new.start].path == self.pane.backlinks[old.start].path
                });
        self.pane.backlinks = backlinks;
        if same_cards {
            self.backlinks_list.remeasure();
        } else {
            // About one source note with a place or two: a first estimate for the scroll extent;
            // the real height replaces it once a card has been in view.
            self.backlinks_list
                .reset_with_uniform_height(groups.len(), px(58.));
        }
        self.pane.backlink_groups = groups;
    }

    /// #381: the display title of a linking note (first H1, then frontmatter
    /// title, then file name) and, as secondary text, its folder path. Index
    /// notes (`@index`, `index`, `README`) take their folder's name.
    pub(super) fn backlink_title(&self, path: &str) -> (String, Option<String>) {
        let (folder, file) = path.rsplit_once('/').unwrap_or(("", path));
        let stem = file
            .rsplit_once('.')
            .filter(|(_, extension)| extension.eq_ignore_ascii_case("md"))
            .map_or(file, |(stem, _)| stem);
        let location = (!folder.is_empty()).then(|| folder.replace('/', " / "));
        let is_index = matches!(stem.to_lowercase().as_str(), "@index" | "index" | "readme");
        if is_index && !folder.is_empty() {
            let name = folder.rsplit('/').next().unwrap_or(folder).to_owned();
            return (name, location);
        }
        let title = self
            .backlink_titles
            .get(path)
            .cloned()
            .unwrap_or_else(|| stem.to_owned());
        (title, location)
    }

    /// Provisional labels use filenames. Ready supplies titles from sources
    /// already read by reconcile, never another round of cloud hydration.
    pub(super) fn sync_backlink_titles(&mut self) {
        let missing: Vec<String> = self
            .pane
            .backlinks
            .iter()
            .map(|b| b.path.clone())
            .filter(|p| !self.backlink_titles.contains_key(p))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if missing.is_empty() {
            return;
        }
        // Placeholders keep this from re-spawning while the read runs.
        let titles = Arc::make_mut(&mut self.backlink_titles);
        for path in &missing {
            let stem = path
                .rsplit('/')
                .next()
                .unwrap_or(path)
                .trim_end_matches(".md")
                .to_owned();
            titles.insert(path.clone(), stem);
        }
    }

    /// «Linked from» with counts only when something links here (#646).
    fn linked_from_title(&self) -> String {
        // Files count their real backlinks too (a note can link or embed a
        // CSV, JSON, HAR or text file, #2885).
        let (notes, places) =
            reader_right_panel::link_counts(self.pane.backlinks.iter().map(|b| b.path.as_str()));
        reader_right_panel::linked_from_title(
            self.vault.inventory_scanned,
            notes,
            places,
            !self.vault.unreadable.is_empty() || !self.vault.cloud_placeholders.is_empty(),
        )
    }

    /// One card of the «Linked from» list: a source note and its places. Called only for the
    /// cards in view (#1135): a hub note can have thousands of them.
    fn render_backlink_group(&self, gx: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(range) = self.pane.backlink_groups.get(gx).cloned() else {
            return div().into_any_element();
        };
        let muted = cx.theme().muted_foreground;
        let current_bg = cx.theme().accent;
        let hover_bg = brand::reader_palette(cx).hover;
        let p = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let mark = p.accent.opacity(0.18);
        // One group per source note (#22): `Vault::backlinks` returns them grouped by path in
        // source-line order, so a run of equal paths is a group.
        // A range that no longer fits (the list one frame behind its groups) draws nothing
        // rather than taking the window down (#1313).
        let Some(links) = self
            .pane
            .backlinks
            .get(range)
            .filter(|links| !links.is_empty())
        else {
            return div().into_any_element();
        };
        let links: Vec<&Backlink> = links.iter().collect();
        let head = links[0];
        let (title, location) = self.backlink_title(&head.path);
        // Ambiguity is per link, but the flag is worth one word on the
        // card when any link in the group carries it.
        let any_ambiguous = links.iter().any(|b| b.ambiguous);
        let is_current = head.path == self.pane.current_rel;
        let via: Vec<String> = links
            .iter()
            .filter_map(|b| b.property.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let occurrences: Vec<_> = links
            .iter()
            .map(|b| {
                let (context, link, jump) = backlink_occurrence(b);
                let external = if b.property.is_none() {
                    prepared_links::snippet_links(&b.context, &context)
                } else {
                    Vec::new()
                };
                let (context, link, external) =
                    prepared_links::decorate_snippet(context, link, external);
                (context, link, jump, external)
            })
            .collect();
        let count = occurrences.len();
        let expanded = self.pane.backlinks_expanded.contains(&head.path);
        let shown = if expanded {
            count
        } else {
            count.min(BACKLINK_PLACES_SHOWN)
        };

        let card_rel = head.path.clone();
        let more_rel = head.path.clone();
        let group = SharedString::from(format!("bl-card-{gx}"));
        // #394 (variant A): the header is the source note — bold title
        // with its folder right beside it, place count, ↗ on hover —
        // and opens it. The rows below are places inside it: indented
        // under a rule, muted, this note's name bold on a light mark,
        // never link-blue. Each place opens the source there.
        let hover_rel = head.path.clone();
        let header = h_flex()
            .id(SharedString::from(format!("bl-source-{gx}")))
            .group(group.clone())
            .gap_1p5()
            .px_2()
            .py_1p5()
            .rounded(px(8.))
            .cursor_pointer()
            .when(is_current, |s| s.bg(current_bg))
            .hover(move |s| s.bg(hover_bg))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_panel_note(reader_layout::Panel::Backlinks, &card_rel, None, window, cx)
            }))
            // Only the icon and title preview the note (#1101): the
            // rest of the card and its places scroll and read freely.
            .child(
                h_flex()
                    .id(SharedString::from(format!("bl-title-{gx}")))
                    .debug_selector(move || format!("backlink-title-{gx}"))
                    .flex_none()
                    .max_w(relative(0.6))
                    .min_w_0()
                    .gap_1p5()
                    .on_hover(cx.listener(move |this, active, window, cx| {
                        let key = format!("backlink:{hover_rel}");
                        if *active {
                            let body = this.body_bounds;
                            let widths = this
                                .workspace_panels()
                                .widths(&this.panel_widths, f32::from(body.size.width));
                            this.hover_note_beside(
                                key,
                                reader_hover::Target {
                                    path: hover_rel.clone(),
                                    heading: None,
                                },
                                window.mouse_position(),
                                body.right() - px(widths.backlinks),
                                window,
                                cx,
                            );
                        } else {
                            this.leave_hover(&key, cx);
                        }
                    }))
                    .child(
                        Icon::new(if any_ambiguous {
                            IconName::TriangleAlert
                        } else {
                            IconName::FileText
                        })
                        .small()
                        .text_color(if any_ambiguous {
                            cx.theme().warning
                        } else {
                            p.text_muted
                        }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(title),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_xs()
                    .text_color(faint)
                    .children(location),
            )
            .children(via.into_iter().map(|key| {
                div()
                    .flex_none()
                    .px_1()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(p.border_subtle)
                    .text_size(px(10.5))
                    .text_color(faint)
                    .child(key)
            }))
            .when(any_ambiguous, |row| {
                // Ambiguity is surfaced, never resolved silently.
                row.child(
                    div()
                        .flex_none()
                        .px_1p5()
                        .rounded(px(4.))
                        .text_size(px(10.5))
                        .bg(cx.theme().warning.opacity(0.15))
                        .text_color(cx.theme().warning)
                        .child("ambiguous"),
                )
            })
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(faint)
                    .child(count.to_string()),
            )
            .child(
                div()
                    .flex_none()
                    .opacity(0.)
                    .group_hover(group.clone(), |s| s.opacity(1.))
                    .child(
                        Icon::default()
                            .path(brand::READER_OPEN_ICON)
                            .small()
                            .text_color(p.text_muted),
                    ),
            );
        let rows = occurrences
            .into_iter()
            .take(shown)
            .enumerate()
            .map(|(lx, (context, link, jump, external))| {
                let rel = head.path.clone();
                let highlight = link
                    .and_then(|r| text_ranges::safe_highlight(&context, r))
                    .map(|r| {
                        (
                            r,
                            HighlightStyle {
                                background_color: Some(mark),
                                color: Some(p.text),
                                font_weight: Some(FontWeight::SEMIBOLD),
                                ..Default::default()
                            },
                        )
                    });
                let mut highlights: Vec<_> = highlight.into_iter().collect();
                highlights.extend(external.iter().map(|(range, _)| {
                    (
                        range.clone(),
                        HighlightStyle {
                            color: Some(p.link),
                            ..Default::default()
                        },
                    )
                }));
                highlights.sort_by_key(|(range, _)| range.start);
                let ranges = external.iter().map(|(range, _)| range.clone()).collect();
                let tooltip_links = external.clone();
                let text = InteractiveText::new(
                    ("bl-context", lx),
                    StyledText::new(context).with_highlights(highlights),
                )
                .on_click(ranges, move |ix, _, cx| {
                    cx.stop_propagation();
                    cx.open_url(&external[ix].1);
                })
                .tooltip(move |ix, window, cx| {
                    let (_, url) = tooltip_links.iter().find(|(r, _)| r.contains(&ix))?;
                    Some(
                        gpui_component::tooltip::Tooltip::new(prepared_links::external_tooltip(
                            url,
                        )?)
                        .build(window, cx),
                    )
                });
                div()
                    .id(SharedString::from(format!("bl-{gx}-{lx}")))
                    .debug_selector(move || format!("backlink-place-{gx}-{lx}"))
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .text_size(px(12.5))
                    .line_height(px(18.))
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_panel_note(
                            reader_layout::Panel::Backlinks,
                            &rel,
                            jump.as_deref(),
                            window,
                            cx,
                        )
                    }))
                    .child(text)
            })
            .collect::<Vec<_>>();
        let card = v_flex().child(header).child(
            v_flex()
                .ml(px(15.))
                .pl_2()
                .border_l_2()
                .border_color(p.border_subtle)
                .children(rows)
                .when(shown < count, |places| {
                    places.child(
                        div()
                            .id(SharedString::from(format!("bl-more-{gx}")))
                            .debug_selector(move || format!("bl-more-{gx}"))
                            .px_2()
                            .py_0p5()
                            .text_xs()
                            .text_color(faint)
                            .cursor_pointer()
                            .hover(move |s| s.text_color(muted))
                            .child(format!("Show {} more", count - shown))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pane.backlinks_expanded.insert(more_rel.clone());
                                this.backlinks_list.remeasure_items(gx..gx + 1);
                                cx.notify();
                            })),
                    )
                }),
        );
        // A list measures the element, not its margins: the spacing is the item's padding.
        div()
            .debug_selector(move || format!("bl-card-{gx}"))
            .px_1()
            .pb_1p5()
            .child(card)
            .into_any_element()
    }

    /// «Linked from»: the cards in view only. The list state counts `backlink_groups`; see
    /// `set_backlinks`, the one place that changes both.
    fn render_backlinks(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        if self.pane.file_preview.is_some() && self.pane.backlinks.is_empty() {
            return panel_empty_line(muted, "—");
        }
        let empty =
            (self.pane.backlink_groups.is_empty() || !self.vault.inventory_scanned).then(|| {
                reader_right_panel::linked_from_empty(
                    self.vault.inventory_scanned,
                    self.vault.inventory_complete,
                )
            });
        v_flex()
            .id("backlinks")
            .debug_selector(|| "backlinks-list".into())
            .flex_1()
            .min_h(px(96.))
            .overflow_hidden()
            // Scrolling wins over previews (#1101); the list scrolls itself.
            .on_scroll_wheel(
                cx.listener(|this, _: &ScrollWheelEvent, _, cx| this.panel_scrolled(cx)),
            )
            .py_1()
            .when_some(empty, |panel, line| {
                panel.child(panel_empty_line(muted, line))
            })
            .when(empty.is_none(), |panel| {
                panel.px_1().child(
                    list(
                        self.backlinks_list.clone(),
                        cx.processor(|this, gx, _window, cx| this.render_backlink_group(gx, cx)),
                    )
                    .size_full(),
                )
            })
            .into_any_element()
    }

    /// docs/design/reader.md §Right panel: Contents above «Linked from», each
    /// scrolling on its own so neither can hide the other.
    pub(super) fn render_right_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.active_timeline().is_some() {
            return self.render_timeline(cx);
        }
        let p = brand::palette(cx);
        // No note is open (a new or empty vault): one line about what the
        // panel shows, not two sections of dashes (#1349).
        if self.selected_file().is_empty() && self.active_timeline().is_none() {
            return div()
                .w_full()
                .h_full()
                .px_4()
                .pt_3()
                .text_size(px(brand::READER_CHROME_FONT_SIZE))
                .text_color(p.text_muted)
                .debug_selector(|| "right-panel-no-note".into())
                .child(reader_right_panel::NO_NOTE)
                .into_any_element();
        }
        let faint = brand::reader_palette(cx).text_faint;
        let section = |icon: &'static str, label: String| {
            h_flex()
                .flex_none()
                .gap_1p5()
                .px_4()
                .pt_3()
                .pb_1p5()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(faint)
                .child(Icon::default().path(icon).xsmall().text_color(faint))
                .child(label)
        };
        let linked = self.linked_from_title();
        v_flex()
            .w_full()
            .h_full()
            .flex_none()
            .text_size(px(brand::READER_CHROME_FONT_SIZE))
            .when_some(self.render_properties_section(cx), |panel, props| {
                panel.child(props).child(
                    div()
                        .flex_none()
                        .h(px(1.))
                        .mx_4()
                        .my_1()
                        .bg(p.border_subtle),
                )
            })
            .child(
                section("icons/list.svg", "Contents".into())
                    .debug_selector(|| "right-contents-heading".into()),
            )
            .child(self.render_outline(cx))
            .child(
                div()
                    .flex_none()
                    .h(px(1.))
                    .mx_4()
                    .my_1()
                    .bg(p.border_subtle),
            )
            .child(
                section("icons/link.svg", linked)
                    .debug_selector(|| "right-linked-from-heading".into()),
            )
            .child(self.render_backlinks(cx))
            .into_any_element()
    }

    pub(super) fn has_properties(&self) -> bool {
        // A previewed file has no frontmatter; `properties` still holds the last note's.
        if self.pane.file_preview.is_some() {
            return false;
        }
        match &self.pane.properties {
            Ok(props) => !props.is_empty(),
            Err(_) => true,
        }
    }

    /// Properties in the right panel (#386): above Contents, collapsible.
    fn render_properties_section(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use reader_sidebar::Section;
        if !self.has_properties() {
            return None;
        }
        let faint = brand::reader_palette(cx).text_faint;
        let collapsed = self.sidebar.is_collapsed(Section::Properties);
        let summary = match &self.pane.properties {
            Ok(props) => reader_properties::summary(props),
            Err(_) => "unreadable".into(),
        };
        Some(
            v_flex()
                .flex_none()
                .max_h(relative(0.45))
                .child(
                    h_flex()
                        .id("properties-section")
                        .debug_selector(|| "properties-section".into())
                        .gap_1p5()
                        .px_4()
                        .pt_3()
                        .pb_1p5()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(faint)
                        .cursor_pointer()
                        .child(
                            Icon::new(if collapsed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .xsmall(),
                        )
                        .child(Icon::default().path(READER_SLIDERS_ICON).xsmall())
                        .child("Properties")
                        .when(collapsed, |row| {
                            row.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .font_weight(FontWeight::NORMAL)
                                    .child(summary),
                            )
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_section(Section::Properties, cx)
                        })),
                )
                .when(!collapsed, |section| {
                    section.child(
                        div()
                            .id("properties-body")
                            .overflow_y_scroll()
                            .px_4()
                            .pb_2()
                            .child(self.render_properties_grid(cx)),
                    )
                })
                .into_any_element(),
        )
    }

    /// Right panel closed (or compact): one summary line above the document
    /// that expands in place (#386).
    pub(super) fn render_properties_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let available = f32::from(self.body_bounds.size.width);
        if !self.has_properties()
            || self
                .panels
                .visible(reader_layout::Panel::Backlinks, available)
        {
            return None;
        }
        let p = brand::palette(cx);
        let tokens = brand::reader_palette(cx);
        let summary = match &self.pane.properties {
            Ok(props) => reader_properties::summary(props),
            Err(_) => "Properties could not be read".into(),
        };
        let open = self.pane.properties_open;
        let header = h_flex()
            .id("properties-strip")
            .gap_2()
            .px_2p5()
            .py_1p5()
            .rounded(px(8.))
            .text_size(px(12.5))
            .text_color(p.text_muted)
            .cursor_pointer()
            .hover(move |d| d.bg(tokens.hover))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(tokens.text_faint),
            )
            .child(Icon::default().path(READER_SLIDERS_ICON).small())
            .child(if open {
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.text)
                    .child("Properties")
            } else {
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(summary)
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.pane.properties_open = !this.pane.properties_open;
                cx.notify();
            }));
        Some(
            v_flex()
                .id("properties-inline")
                .flex_none()
                .mx(px(READER_SIDE_PADDING - 10.))
                .mt(px(16.))
                .max_h(relative(0.45))
                .when(open, |d| d.rounded(px(10.)).p_1())
                .child(header)
                .when(open, |d| {
                    d.child(
                        div()
                            .id("properties-inline-body")
                            .overflow_y_scroll()
                            .px_2p5()
                            .pb_2()
                            .text_size(px(brand::READER_CHROME_FONT_SIZE))
                            .child(self.render_properties_grid(cx)),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_properties_grid(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = brand::reader_palette(cx);
        let props = match &self.pane.properties {
            Ok(props) => props,
            Err(error) => {
                let detail = format!("Frontmatter is not valid YAML: {error}");
                return div()
                    .id("properties-read-error")
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child("Properties could not be read")
                    .tooltip(move |window, cx| {
                        let detail = detail.clone();
                        let width =
                            (f32::from(window.viewport_size().width) - 64.).clamp(160., 360.);
                        gpui_component::tooltip::Tooltip::element(move |_, _| {
                            div().w(px(width)).whitespace_normal().child(detail.clone())
                        })
                        .build(window, cx)
                    })
                    .into_any_element();
            }
        };
        let hidden = props.iter().filter(|p| p.hidden()).count();
        let today = reader_properties::today();
        let rows = props
            .iter()
            .filter(|p| self.pane.show_hidden_properties || !p.hidden())
            .enumerate()
            .map(|(ix, property)| {
                h_flex()
                    .items_start()
                    .gap_2p5()
                    .py(px(3.))
                    .child(
                        div()
                            .w(px(92.))
                            .flex_none()
                            .pt(px(1.))
                            .text_color(tokens.text_faint)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(property.key.clone()),
                    )
                    .child(div().flex_1().min_w_0().child(self.render_property_value(
                        &property.key,
                        &property.value,
                        (ix, 0),
                        today,
                        cx,
                    )))
            })
            .collect::<Vec<_>>();
        v_flex()
            .children(rows)
            .when(hidden > 0, |grid| {
                grid.child(
                    div()
                        .id("properties-hidden-toggle")
                        .pt_1p5()
                        .text_xs()
                        .text_color(tokens.text_faint)
                        .cursor_pointer()
                        .child(if self.pane.show_hidden_properties {
                            format!("Hide system properties ({hidden})")
                        } else {
                            format!("Show system properties ({hidden})")
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pane.show_hidden_properties = !this.pane.show_hidden_properties;
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    fn render_property_value(
        &self,
        key: &str,
        value: &okilum_core::properties::PropertyValue,
        id: (usize, usize),
        today: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use okilum_core::properties::PropertyValue as V;
        let p = brand::palette(cx);
        let tokens = brand::reader_palette(cx);
        let chip = |text: String, status: bool| {
            h_flex()
                .child(
                    div()
                        .px_2()
                        .rounded_full()
                        .text_xs()
                        .when(status, |d| {
                            d.bg(p.accent.opacity(0.15))
                                .text_color(p.link)
                                .font_weight(FontWeight::MEDIUM)
                        })
                        .when(!status, |d| d.bg(p.surface_raised).text_color(p.text))
                        .child(text),
                )
                .into_any_element()
        };
        match value {
            V::Text(text) | V::Number(text) if reader_properties::chip_key(key) => {
                let text = if key.eq_ignore_ascii_case("tags") || key.eq_ignore_ascii_case("tag") {
                    format!("#{}", text.trim_start_matches('#'))
                } else {
                    text.clone()
                };
                chip(text, reader_properties::status_key(key))
            }
            V::Text(text) | V::Number(text) => div().child(text.clone()).into_any_element(),
            V::Bool(b) => div()
                .child(if *b { "yes" } else { "no" })
                .into_any_element(),
            V::Empty => div()
                .text_color(tokens.text_faint)
                .child("—")
                .into_any_element(),
            V::Date {
                year,
                month,
                day,
                time,
            } => div()
                .child(reader_properties::date_label(
                    *year, *month, *day, *time, today,
                ))
                .into_any_element(),
            V::Url(url) => {
                let shown = url
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .trim_end_matches('/')
                    .to_owned();
                let open = url.clone();
                let tooltip = prepared_links::external_tooltip(url).unwrap_or_else(|| url.clone());
                div()
                    .id(SharedString::from(format!("prop-url-{}-{}", id.0, id.1)))
                    .text_color(p.link)
                    .cursor_pointer()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(format!("{shown} ↗"))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                    })
                    .on_click(move |_, _, cx| cx.open_url(&open))
                    .into_any_element()
            }
            V::Link { target, label } => {
                let resolution = self.vault.resolve_from(target, &self.pane.current_rel);
                let missing = matches!(resolution, okilum_core::vault::Resolution::Unresolved);
                let target = target.clone();
                div()
                    .id(SharedString::from(format!("prop-link-{}-{}", id.0, id.1)))
                    .text_color(if missing { tokens.missing_link } else { p.link })
                    .cursor_pointer()
                    .child(label.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_property_link(&target, window, cx)
                    }))
                    .into_any_element()
            }
            V::List(items) => h_flex()
                .flex_wrap()
                .gap_1()
                .children(items.iter().enumerate().map(|(ix, item)| {
                    let shown = match item {
                        V::Text(text) | V::Number(text) => chip(
                            if key.eq_ignore_ascii_case("tags") || key.eq_ignore_ascii_case("tag") {
                                format!("#{}", text.trim_start_matches('#'))
                            } else {
                                text.clone()
                            },
                            false,
                        ),
                        other => self.render_property_value(key, other, (id.0, ix + 1), today, cx),
                    };
                    shown
                }))
                .into_any_element(),
        }
    }

    /// A property wikilink resolves like a body link from the open note;
    /// ambiguity is offered as choices, never picked silently.
    pub(super) fn open_property_link(
        &mut self,
        target: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use okilum_core::vault::Resolution;
        let (path, heading) = match target.split_once('#') {
            Some((path, heading)) => (path, Some(heading.to_owned())),
            None => (target, None),
        };
        match self.vault.resolve_from(path, &self.pane.current_rel) {
            Resolution::Resolved { path } => {
                self.open_note_at(&path, None, heading.as_deref(), window, cx)
            }
            Resolution::Ambiguous { candidates } => {
                self.link_notice = Some(format!("«{path}» names several notes:").into());
                self.pane.link_choices = candidates
                    .into_iter()
                    .map(|c| (c, heading.clone()))
                    .collect();
                cx.notify();
            }
            Resolution::Unresolved => {
                self.link_notice = Some(format!("No note named «{path}».").into());
                cx.notify();
            }
        }
    }

    /// Index into `outline` of the section containing the top visible block.
    pub(super) fn current_section(&self, cx: &App) -> Option<usize> {
        if let Some(editing) = &self.pane.editing {
            return editing
                .input()
                .read(cx)
                .top_visible_line()
                .and_then(|line| self.draft_outline.current(line));
        }
        let top = self
            .pane
            .content
            .read(cx)
            .list_state()
            .logical_scroll_top()
            .item_ix;
        let after = self
            .pane
            .outline
            .partition_point(|heading| heading.target.block <= top);
        after.checked_sub(1)
    }

    fn render_outline(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(outline) = reader_typed_view::outline(self, cx) {
            return outline;
        }
        let p = brand::palette(cx);
        if self.pane.file_preview.is_some() || self.visible_outline().is_empty() {
            return div()
                .flex_none()
                .py_1()
                .child(panel_empty_line(
                    p.text_muted,
                    reader_right_panel::contents_empty(self.pane.file_preview.is_some()),
                ))
                .into_any_element();
        }
        let hover = brand::reader_palette(cx).hover;
        let current = self.current_section(cx);
        let entity = cx.entity().downgrade();
        let base = self
            .visible_outline()
            .iter()
            .map(|h| h.level)
            .min()
            .unwrap_or(1);
        // Only the rows in view are built, so the panel costs the same for 32 and 436
        // headings (#1004). The list needs a height of its own: every row's, and
        // it may use what «Linked from» does not need (#2814), not only 60 %.
        let rows_height = self.visible_outline().len() as f32 * OUTLINE_ROW_HEIGHT;
        let full = rows_height + 2. * OUTLINE_PADDING_Y;
        v_flex()
            .id("reader-outline")
            .debug_selector(|| "reader-outline-list".into())
            .flex_shrink(1.)
            .h(px(full))
            .min_h(px(full.min(6. * OUTLINE_ROW_HEIGHT)))
            .py(px(OUTLINE_PADDING_Y))
            .child(
                list(
                    self.outline_list.clone(),
                    cx.processor(move |this, ix: usize, _window, cx| {
                        let Some(heading) = this.visible_outline().get(ix) else {
                            return div().into_any_element();
                        };
                        let p = brand::palette(cx);
                        let target = heading.clone();
                        let label = this
                            .pane
                            .query_navigation
                            .active
                            .borrow()
                            .as_ref()
                            .map_or(heading.text.as_str(), |query| {
                                query.heading_label(&heading.text)
                            })
                            .to_owned();
                        let is_current = current == Some(ix);
                        let indent = f32::from(heading.level.saturating_sub(base)) * 12.;
                        div()
                            .id(("reader-outline-item", ix))
                            .debug_selector(move || format!("reader-outline-row-{ix}"))
                            // Never shrink a row below its text; the list scrolls.
                            .flex_none()
                            .h(px(OUTLINE_ROW_HEIGHT))
                            .pl(px(14. + indent))
                            .pr_3()
                            .py(px(3.))
                            .border_l_2()
                            .border_color(if is_current {
                                p.accent
                            } else {
                                gpui::transparent_black()
                            })
                            .text_color(if is_current { p.text } else { p.text_muted })
                            .when(is_current, |d| d.font_weight(FontWeight::MEDIUM))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .cursor_pointer()
                            .hover(move |d| d.bg(hover))
                            .child(
                                div()
                                    .debug_selector(move || format!("reader-outline-text-{ix}"))
                                    .text_ellipsis()
                                    .child(label),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_outline_heading(target.clone(), window, cx);
                            }))
                            .context_menu({
                                let entity = entity.clone();
                                let text = heading.text.clone();
                                move |menu, _, _| {
                                    reader_deep_link::heading_link_item(menu, &entity, text.clone())
                                }
                            })
                            .into_any_element()
                    }),
                )
                .size_full(),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn counts_group_runs_of_one_source() {
        assert_eq!(link_counts([]), (0, 0));
        assert_eq!(link_counts(["a.md", "a.md", "b.md"]), (2, 3));
    }

    #[test]
    fn empty_linked_from_has_no_zero_counts() {
        for partial in [false, true] {
            let title = linked_from_title(true, 0, 0, partial);
            assert_eq!(title, "Linked from");
            assert!(!title.contains('0'));
        }
        assert_eq!(linked_from_title(false, 0, 0, false), "Linked from");
    }

    #[test]
    fn linked_from_counts_when_present() {
        assert_eq!(
            linked_from_title(true, 1, 1, false),
            "Linked from · 1 note · 1 place"
        );
        assert_eq!(
            linked_from_title(true, 2, 5, true),
            "Linked from · 2 notes · 5 places · partial"
        );
        // Counts left over from a scan that has not finished are not shown.
        assert_eq!(linked_from_title(false, 2, 5, false), "Linked from");
    }

    #[test]
    fn empty_lines_are_single_and_count_free() {
        for line in [
            linked_from_empty(false, false),
            linked_from_empty(true, true),
            linked_from_empty(true, false),
            contents_empty(false),
            contents_empty(true),
        ] {
            assert!(!line.is_empty());
            assert!(!line.chars().any(|c| c.is_ascii_digit()));
            assert!(!line.contains('\n'));
        }
        assert_eq!(linked_from_empty(true, true), "—");
    }
}
