//! Source-bound link presentation. Jobs own immutable source/root snapshots;
//! changing the document or accepting any refresh invalidates the whole result.
use super::*;
use std::collections::BTreeMap;
use tessera_core::document_links::{
    self,
    prepared::{LinkPreparation, LinkState, TargetSnapshot},
};

pub(crate) type States = Arc<BTreeMap<String, LinkState>>;
pub(crate) type SnippetLinks = Vec<(std::ops::Range<usize>, String)>;

#[cfg(test)]
thread_local! {
    static PAINTED_LINKS: std::cell::RefCell<Option<Vec<(String, bool, bool)>>> = const { std::cell::RefCell::new(None) };
}

/// External protocol links have a visible destination, never an internal-note guess.
pub(crate) fn external_tooltip(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    match parsed.scheme() {
        "http" | "https" | "ftp" | "mailto" | "tel" => Some(match parsed.host_str() {
            Some(host) => format!("{host}\n{url}"),
            None => url.to_owned(),
        }),
        _ => None,
    }
}

/// Preserve authored-link identity while stripping snippet Markdown. Markers
/// are temporary and collision-free; they never enter the displayed string.
pub(crate) fn snippet_links(source: &str, plain: &str) -> SnippetLinks {
    let markers: Vec<char> = ('\u{e000}'..='\u{f8ff}')
        .filter(|c| !source.contains(*c))
        .take(2)
        .collect();
    if markers.len() != 2 {
        return Vec::new();
    }
    let links: Vec<_> = tessera_core::document_links::parse(source)
        .into_iter()
        .filter(|l| !l.wiki && external_tooltip(&l.target).is_some())
        .collect();
    let mut marked = source.to_owned();
    for link in links.iter().rev() {
        marked.insert(link.range.end, markers[1]);
        marked.insert(link.range.start, markers[0]);
    }
    let stripped = tessera_core::render::strip_inline_markdown(&marked);
    let mut unmarked = String::new();
    let mut ranges = Vec::new();
    let mut start = None;
    for ch in stripped.chars() {
        if ch == markers[0] {
            start = Some(unmarked.len());
        } else if ch == markers[1] {
            if let Some(start) = start.take() {
                ranges.push(start..unmarked.len());
            }
        } else {
            unmarked.push(ch);
        }
    }
    if unmarked != tessera_core::render::strip_inline_markdown(source)
        || ranges.len() != links.len()
    {
        return Vec::new();
    }
    ranges
        .into_iter()
        .zip(links)
        .filter_map(|(range, link)| {
            // Truncation keeps a prefix; do not turn a partial label/ellipsis into
            // a clickable destination or decorate a separately formatted property.
            (plain.get(range.clone()) == unmarked.get(range.clone()) && range.end <= plain.len())
                .then_some((range, link.target))
        })
        .collect()
}

pub(crate) fn decorate_snippet(
    mut text: String,
    mut highlight: Option<std::ops::Range<usize>>,
    links: SnippetLinks,
) -> (String, Option<std::ops::Range<usize>>, SnippetLinks) {
    let original_highlight = highlight.clone();
    let mut decorated = Vec::new();
    let mut added = 0;
    for (range, url) in links {
        // Destination marks retain their established muted appearance.
        if original_highlight
            .as_ref()
            .is_some_and(|h| h.start < range.end && range.start < h.end)
        {
            continue;
        }
        let end = range.end + added;
        text.insert_str(end, " ↗");
        if let Some(h) = &mut highlight {
            if h.start >= end {
                h.start += " ↗".len();
                h.end += " ↗".len();
            }
        }
        decorated.push((range.start + added..end + " ↗".len(), url));
        added += " ↗".len();
    }
    (text, highlight, decorated)
}

pub(crate) fn presentation(
    url: &str,
    states: &BTreeMap<String, LinkState>,
) -> gpui_component::text::LinkPresentation {
    use gpui_component::text::LinkPresentation;
    if let Some(tooltip) = external_tooltip(url) {
        return LinkPresentation {
            external: true,
            tooltip: Some(tooltip.into()),
            ..Default::default()
        };
    }
    let fallback = LinkState::unknown();
    let state = states.get(url).unwrap_or(&fallback);
    let mut style = gpui::HighlightStyle::default();
    if state.status.is_missing() {
        style.color = Some(crate::brand::reader_palette_current().missing_link);
        style.underline = Some(gpui::UnderlineStyle {
            thickness: px(1.),
            wavy: true,
            ..Default::default()
        });
    }
    let presentation = LinkPresentation {
        style,
        tooltip: Some(state.reason.clone().into()),
        inert: state.status.is_missing(),
        ..Default::default()
    };
    #[cfg(test)]
    PAINTED_LINKS.with(|painted| {
        if let Some(painted) = &mut *painted.borrow_mut() {
            painted.push((
                url.into(),
                presentation.inert,
                presentation
                    .style
                    .underline
                    .is_some_and(|underline| underline.wavy),
            ));
        }
    });
    presentation
}

pub(crate) struct PreparedDocument {
    pub source: String,
    pub original: Option<String>,
    pub identities: Vec<document_links::prepared::LinkIdentity>,
    /// Leading frontmatter YAML for the Properties view (#386).
    pub frontmatter: Option<String>,
}

pub(crate) struct DocumentRequest {
    pub rel: String,
    pub jump: Option<String>,
    pub heading: Option<String>,
    pub history_index: Option<usize>,
    pub restore_position: Option<ListOffset>,
}

impl Reader {
    pub(crate) fn prepare_document(
        &mut self,
        rel: &str,
        jump: Option<&str>,
        heading: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.leave_source(cx) {
            return;
        }
        self.cancel_pending_landing();
        self.document_preparation_generation = self.document_preparation_generation.wrapping_add(1);
        let generation = self.document_preparation_generation;
        let request = DocumentRequest {
            rel: rel.to_owned(),
            jump: jump.map(str::to_owned),
            heading: heading.map(str::to_owned),
            history_index: self.history_nav,
            restore_position: self.history_nav.map(|index| self.history_positions[index]),
        };
        let vault = self.vault.clone();
        let rel = rel.to_owned();
        let html = self.use_html;
        cx.spawn_in(window, async move |this, cx| {
            let document = cx
                .background_executor()
                .spawn(async move {
                    if html && !Path::new(&rel).is_absolute() {
                        tessera_core::render_html(&vault, &rel, "InspiredGitHub").map(|html| {
                            PreparedDocument {
                                source: html,
                                original: None,
                                identities: Vec::new(),
                                frontmatter: None,
                            }
                        })
                    } else {
                        tessera_core::render::reader_document(&vault, &rel).map(|document| {
                            PreparedDocument {
                                source: document.rendered,
                                original: Some(document.original_body),
                                identities: document.links,
                                frontmatter: document.frontmatter,
                            }
                        })
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.document_preparation_generation != generation {
                    return;
                }
                this.accept_prepared_document(request, document, window, cx);
            });
        })
        .detach();
    }

    pub(crate) fn invalidate_links(&mut self) {
        self.link_preparation_generation = self.link_preparation_generation.wrapping_add(1);
        // Action evidence is pending, but the displayed source has not changed.
        // Keep its verified appearance until a newer result replaces it.
        self.prepared_links = Arc::default();
    }

    fn prepare_links(&mut self, original: String, cx: &mut Context<Self>) {
        self.invalidate_links();
        let generation = self.link_preparation_generation;
        let navigation = self.navigation_generation;
        let from = self.current_rel.clone();
        let vault = self.vault.clone();
        let rendered = self.note_source.clone();
        let identities = self.link_identities.clone();
        cx.spawn(async move |this, cx| {
            let states = cx
                .background_executor()
                .spawn(async move {
                    let mut preparation = LinkPreparation::new(&vault, &from, |target| {
                        let (raw, heading_source) = if target == from {
                            (original.clone(), rendered.clone())
                        } else if !vault.inventory_complete {
                            return Err(
                                "Link destination is pending background inventory verification."
                                    .into(),
                            );
                        } else {
                            let raw = std::fs::read_to_string(vault.root.join(target))
                                .map_err(|_| "Document source is unavailable.".to_owned())?;
                            let heading_source =
                                tessera_core::render::reader_heading_source(&vault, target, &raw);
                            (raw, heading_source)
                        };
                        use sha2::{Digest, Sha256};
                        Ok(TargetSnapshot {
                            revision: format!("sha256:{:x}", Sha256::digest(raw.as_bytes())),
                            headings: document_links::HeadingInventory::new(&heading_source),
                            supports_setext: true,
                            managed: None,
                        })
                    });
                    Arc::new(preparation.identities(&identities))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.link_preparation_generation != generation
                    || this.navigation_generation != navigation
                {
                    return;
                }
                this.link_presentations = states.clone();
                this.prepared_links = states;
                // TextView caches its entity render independently of Reader.
                // Refresh paint without replacing the parse, scroll or selection.
                this.content.update(cx, |_, cx| cx.notify());
                if let Some(overlay) = &this.table_overlay {
                    overlay.update(cx, |_, cx| cx.notify());
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn refresh_link_preparation(&mut self, cx: &mut Context<Self>) {
        if let Some(original) = self.link_original_source.clone() {
            self.prepare_links(original, cx);
        } else {
            self.invalidate_links();
        }
    }
}

pub(crate) fn managed_states(preview: &serde_json::Value, current: bool) -> States {
    let mut states = BTreeMap::new();
    if !current || preview["prepared_links_version"] != 1 {
        return Arc::new(states);
    }
    if let Some(links) = preview["links"].as_array() {
        for link in links {
            if let (Some(url), Ok(state)) = (
                link["url"].as_str(),
                serde_json::from_value::<LinkState>(link["prepared"].clone()),
            ) {
                states
                    .entry(url.to_owned())
                    .and_modify(|old| {
                        if old != &state {
                            *old = LinkState::unknown();
                        }
                    })
                    .or_insert(state);
            }
        }
    }
    Arc::new(states)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::sync::Mutex;
    use tessera_core::document_links::prepared::LinkStatus;

    #[cfg(unix)]
    #[gpui::test]
    fn unsaved_editor_exit_keeps_missing_paint_through_overlapping_save_watcher_refresh(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n\n[[target|Control]]").unwrap();
        std::fs::write(root.join("target.md"), "# Target").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(temp.path().join("cache")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        PAINTED_LINKS.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
        let (first_send, first_hold) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            v.incremental_hold = Some(first_hold);
            v.toggle_source(window, cx);
            v.editing.as_ref().unwrap().set_value(
                "# Start\n\n[[target|Control]]\n\n[[missing|Missing]]",
                window,
                cx,
            );
            // No explicit save. Cmd+E's lifecycle save races the watcher.
            v.toggle_source(window, cx);
        });
        visual.run_until_parked();
        let url = reader.read_with(visual, |v, _| {
            assert!(v.editing.is_none() && v.incremental_active);
            v.link_identities
                .iter()
                .find(|link| link.target == "missing")
                .unwrap()
                .url
                .clone()
        });
        let drawn = || {
            PAINTED_LINKS.with(|trace| {
                trace
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .iter()
                    .filter(|(u, _, _)| u == &url)
                    .map(|(_, inert, wavy)| (*inert, *wavy))
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            drawn().last(),
            Some(&(true, true)),
            "positive control: actual renderer sees the initial missing style"
        );
        PAINTED_LINKS.with(|trace| trace.borrow_mut().as_mut().unwrap().clear());
        let (second_send, second_hold) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            v.incremental_hold = Some(second_hold);
            v.apply_vault_changes(
                tessera_core::Changes {
                    changed: std::collections::BTreeSet::from(["start.md".into()]),
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        first_send.try_send(()).unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(v.incremental_active);
            assert!(
                v.prepared_links.is_empty(),
                "action evidence remains pending"
            );
            assert_eq!(
                v.link_presentations[&url].status,
                LinkStatus::MissingDocument
            );
        });
        visual.update(|window, cx| handle_link(&reader.downgrade(), &url, window, cx));
        reader.read_with(visual, |v, _| {
            assert_eq!(v.current_rel, "start.md");
            assert_eq!(
                v.link_notice.as_deref(),
                Some(LinkState::unknown().reason.as_str())
            );
        });
        assert!(!drawn().is_empty(), "watcher refresh must paint a frame");
        assert!(
            drawn().iter().all(|state| *state == (true, true)),
            "missing link must not become ordinary while its same-source refresh is pending: {:?}",
            drawn()
        );
        second_send.try_send(()).unwrap();
        visual.run_until_parked();
        assert_eq!(drawn().last(), Some(&(true, true)));
        reader.update_in(visual, |v, window, cx| {
            // A previous-source worker must never refill a newly accepted note.
            v.refresh_link_preparation(cx);
            v.open_note("target.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert_eq!(v.current_rel, "target.md");
            assert!(v.prepared_links.is_empty() && v.link_presentations.is_empty());
        });
        PAINTED_LINKS.with(|trace| *trace.borrow_mut() = None);
    }

    #[test]
    fn external_links_and_snippet_offsets_are_source_bound() {
        for url in [
            "https://example.com/path",
            "HTTP://example.com",
            "mailto:hello@example.com",
            "tel:+123",
        ] {
            assert!(presentation(url, &BTreeMap::new()).external);
            assert!(matches!(
                document_links::destination(url, false),
                document_links::Destination::External(_)
            ));
        }
        for url in [
            "tessera://note/a",
            "file:///tmp/a",
            "a.md",
            "#Heading",
            "javascript:alert(1)",
        ] {
            assert!(!presentation(url, &BTreeMap::new()).external);
        }
        let source =
            "[[Target]] and [**Сайт**](https://example.com) then [Сайт](https://other.example)";
        let (plain, target) =
            tessera_core::render::strip_inline_markdown_tracking(source, Some("Target"));
        let ranges = snippet_links(source, &plain);
        assert_eq!(ranges.len(), 2);
        assert_eq!(&plain[ranges[0].0.clone()], "Сайт");
        let (shown, target, ranges) = decorate_snippet(plain, target, ranges);
        assert_eq!(&shown[target.unwrap()], "Target");
        assert_eq!(&shown[ranges[0].0.clone()], "Сайт ↗");
        assert_eq!(ranges[1].1, "https://other.example");
        let source = "[A](https://a.example) [[Target]] [B](https://b.example)";
        let (plain, target) =
            tessera_core::render::strip_inline_markdown_tracking(source, Some("Target"));
        let links = snippet_links(source, &plain);
        let (shown, target, links) = decorate_snippet(plain, target, links);
        assert_eq!(&shown[target.unwrap()], "Target");
        assert_eq!(links.len(), 2);
        assert_eq!(&shown[links[1].0.clone()], "B ↗");
        let cut = "Target and С…";
        assert!(snippet_links(source, cut).is_empty());
        assert!(snippet_links(
            "`[code](https://example.com)`",
            "`[code](https://example.com)`"
        )
        .is_empty());
    }

    #[gpui::test]
    fn external_marker_preserves_copy_and_reference_link_action(cx: &mut TestAppContext) {
        struct Fixture {
            text: Entity<TextViewState>,
            calls: Arc<Mutex<Vec<String>>>,
        }
        impl Render for Fixture {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let calls = self.calls.clone();
                div().w(px(450.)).child(
                    TextView::new(&self.text)
                        .selectable(true)
                        .selection_format(SelectionFormat::Plain)
                        .link_presentation(|url| presentation(url, &BTreeMap::new()))
                        .on_link_click(move |url, _, _, _| {
                            calls.lock().unwrap().push(url.to_string())
                        }),
                )
            }
        }
        cx.update(gpui_component::init);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut text = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let state = cx.new(|cx| {
                TextViewState::markdown(
                    "Before [**Site** label][web] after.\n\n[web]: https://example.com/path",
                    cx,
                )
                .selectable(true)
                .retain_selection_on_layout(true)
            });
            text = Some(state.clone());
            let view = cx.new(|_| Fixture {
                text: state,
                calls: calls.clone(),
            });
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let marker = visual
            .debug_bounds("external-link-marker")
            .expect("reference link has a marker");
        visual.simulate_click(marker.center(), Modifiers::default());
        assert_eq!(*calls.lock().unwrap(), ["https://example.com/path"]);
        let text = text.unwrap();
        // Drag through the split inline fragments, not only Select All.
        let y = marker.center().y;
        visual.simulate_mouse_down(point(px(1.), y), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(point(px(440.), y), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(point(px(440.), y), MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            text.read_with(visual, |v, _| v.selected_text()).trim(),
            "Before Site label after."
        );
        text.update(visual, |v, cx| v.set_search_query("after", cx));
        visual.run_until_parked();
        assert_eq!(text.read_with(visual, |v, _| v.search_status().1), 1);
        assert_eq!(
            text.read_with(visual, |v, _| v.selected_text()).trim(),
            "Before Site label after."
        );
        text.update(visual, |v, cx| v.set_search_query("↗", cx));
        visual.run_until_parked();
        assert_eq!(text.read_with(visual, |v, _| v.search_status().1), 0);
        text.update(visual, |v, cx| v.select_all(cx));
        visual.run_until_parked();
        assert_eq!(
            text.read_with(visual, |v, _| v.selected_text()),
            "Before Site label after.\n"
        );
        text.update(visual, |v, cx| {
            v.set_text("[Internal](tessera://note/a)", cx)
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("external-link-marker").is_none(),
            "internal link has no external cue"
        );
    }

    struct LinkFixture {
        calls: Arc<Mutex<Vec<String>>>,
        presentations: Arc<Mutex<Vec<(String, bool)>>>,
    }
    impl Render for LinkFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let calls = self.calls.clone();
            let seen = self.presentations.clone();
            let state = LinkState {
                status: LinkStatus::MissingHeading,
                reason: "Heading not found: Landing".into(),
                target_revision: Some("exact".into()),
                action_url: None,
            };
            let states = BTreeMap::from([("tessera://missing".into(), state)]);
            div().size_full().child(markdown_plugins(
                TextView::markdown("prepared-fixture", "[Resolved control](tessera://control)\n\n> [!note] Nested\n> [Missing heading](tessera://missing)\n\n[Missing heading](tessera://missing)").selectable(true),
                Arc::new(move |url, _, _, _| calls.lock().unwrap().push(url.into())),
                Arc::new(|_| None), SelectionFormat::Plain,
            ).link_presentation(move |url| {
                let result = presentation(url, &states);
                seen.lock().unwrap().push((url.into(), result.inert));
                result
            }))
        }
    }

    #[gpui::test]
    fn actual_shared_plugin_keeps_nested_missing_links_inert_with_positive_control(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let presentations = Arc::new(Mutex::new(Vec::new()));
        let (_, cx) = cx.add_window_view(|_, _| LinkFixture {
            calls: calls.clone(),
            presentations: presentations.clone(),
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_click(point(px(30.), px(10.)), Modifiers::default());
        cx.run_until_parked();
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|u| u == "tessera://control"),
            "actual callback/hit-test positive control"
        );
        // Cover the same nested area as the existing recursive plugin regression.
        for y in (44..200).step_by(4) {
            cx.simulate_mouse_move(point(px(40.), px(y as f32)), None, Modifiers::default());
            cx.simulate_click(point(px(40.), px(y as f32)), Modifiers::default());
        }
        cx.run_until_parked();
        let seen = presentations.lock().unwrap();
        assert!(
            seen.iter()
                .filter(|(url, inert)| url == "tessera://missing" && *inert)
                .count()
                >= 2,
            "both top-level and nested link runs received presentation"
        );
        assert!(!calls
            .lock()
            .unwrap()
            .iter()
            .any(|u| u == "tessera://missing"));
        let missing = LinkState {
            status: LinkStatus::MissingDocument,
            reason: "Document not found: absent.md".into(),
            target_revision: None,
            action_url: None,
        };
        let p = presentation("u", &BTreeMap::from([("u".into(), missing)]));
        assert!(p.inert && p.style.color.is_some() && p.style.underline.is_some());
        assert_eq!(p.tooltip.as_deref(), Some("Document not found: absent.md"));
    }

    #[gpui::test]
    fn root_missing_pointer_preserves_preexisting_selection_and_reader_state(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root =
            std::env::temp_dir().join(format!("tessera-inert-selection-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = "[Target control](target.md)\n\nUnique selectable paragraph for the pointer regression.\n";
        std::fs::write(root.join("start.md"), source).unwrap();
        std::fs::write(root.join("target.md"), "# Target").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let bounds = reader.read_with(visual, |v, cx| v.content.read(cx).bounds());
        let link = bounds.origin + point(px(25.), px(10.));
        visual.simulate_click(link, Modifiers::default());
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert_eq!(
                v.current_rel, "target.md",
                "same-position pointer navigation positive control"
            );
            v.history_move(-1, window, cx);
        });
        visual.run_until_parked();
        std::fs::remove_file(root.join("target.md")).unwrap();
        reader.update_in(visual, |v, window, cx| {
            let mut changes = tessera_core::Changes::default();
            changes.changed.insert("target.md".into());
            v.apply_vault_changes(changes, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, _, _| {
            assert!(v
                .prepared_links
                .values()
                .any(|state| state.status == LinkStatus::MissingDocument));
            v.link_notice = Some("Retained notice".into());
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
        let bounds = reader.read_with(visual, |v, cx| v.content.read(cx).bounds());
        let link = bounds.origin + point(px(25.), px(10.));
        let start = bounds.origin + point(px(5.), px(50.));
        let end = start + point(px(90.), px(0.));
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        let before = reader.read_with(visual, |v, cx| {
            (
                v.content.read(cx).selected_text(),
                v.content.entity_id(),
                v.current_rel.clone(),
                v.history.clone(),
                v.history_ix,
                v.link_notice.clone(),
                v.content.read(cx).list_state().logical_scroll_top(),
                v.note_source.clone(),
            )
        });
        assert!(
            !before.0.is_empty() && before.0.len() < 55,
            "real partial-selection positive control: {:?}",
            before.0
        );
        assert_eq!(
            source.matches(before.0.trim()).count(),
            1,
            "selected text identifies a unique source range"
        );
        for down in [true, false] {
            if down {
                visual.simulate_mouse_down(link, MouseButton::Left, Modifiers::default());
            } else {
                visual.simulate_mouse_up(link, MouseButton::Left, Modifiers::default());
            }
            visual.run_until_parked();
            reader.read_with(visual, |v, cx| {
                assert_eq!(
                    v.content.read(cx).selected_text(),
                    before.0,
                    "down={down}, bounds={bounds:?}, link={link:?}"
                );
                assert_eq!(v.content.entity_id(), before.1);
                assert_eq!(v.current_rel, before.2);
                assert_eq!(v.history, before.3);
                assert_eq!(v.history_ix, before.4);
                assert_eq!(v.link_notice, before.5);
                let scroll = v.content.read(cx).list_state().logical_scroll_top();
                assert_eq!(scroll.item_ix, before.6.item_ix);
                assert_eq!(scroll.offset_in_item, before.6.offset_in_item);
                assert_eq!(v.note_source, before.7);
            });
        }
        // Multi-click on the same inert link still performs ordinary word selection.
        visual.simulate_event(MouseDownEvent {
            position: link,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
            click_count: 2,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            position: link,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
            click_count: 2,
        });
        visual.run_until_parked();
        reader.read_with(visual, |v, cx| {
            assert_eq!(v.content.read(cx).selected_text().trim(), "Target");
            assert_eq!(v.current_rel, "start.md");
            assert_eq!(v.link_notice, before.5);
        });
        // A lost release on an exempt link cannot exempt the next ordinary press.
        visual.simulate_mouse_down(link, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.simulate_click(start, Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |v, cx| {
            assert!(
                v.content.read(cx).selected_text().is_empty(),
                "ordinary text click still clears selection"
            )
        });
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            source
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn reader_refresh_updates_status_without_replacing_document_or_history(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root =
            std::env::temp_dir().join(format!("tessera-prepared-reader-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        PAINTED_LINKS.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
        let source =
            "# Start\n\n[Absent](target.md#Landing)\n\n> [!note]\n> [[target#Landing|Nested]]\n";
        std::fs::write(root.as_path().join("start.md"), source).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.as_path().into()),
                        note: Some("start.md".into()),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let url = "tessera://unresolved/target.md%23Landing";
        let urls = reader.read_with(visual, |v, _| {
            v.link_identities
                .iter()
                .map(|link| link.url.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            urls.len(),
            2,
            "positive control: document and nested callout links"
        );
        let assert_paint = |missing: bool| {
            PAINTED_LINKS.with(|trace| {
                let trace = trace.borrow();
                let trace = trace.as_ref().unwrap();
                for url in &urls {
                    assert_eq!(
                        trace
                            .iter()
                            .rfind(|(u, _, _)| u == url)
                            .map(|(_, inert, wavy)| (*inert, *wavy)),
                        Some((missing, missing)),
                        "actual paint for {url}"
                    );
                }
            });
        };
        assert_paint(true);
        let content = reader.update_in(visual, |v, _, cx| {
            assert_eq!(v.prepared_links[url].status, LinkStatus::MissingDocument);
            v.content.update(cx, |s, cx| s.select_all(cx));
            v.link_notice = Some("Retain notice".into());
            v.content.entity_id()
        });
        let selection = reader.read_with(visual, |v, cx| v.content.read(cx).selected_text());
        assert!(
            !selection.is_empty(),
            "positive control: selected document text"
        );
        visual.update(|window, cx| handle_link(&reader.downgrade(), url, window, cx));
        reader.update_in(visual, |v, _, _| {
            assert_eq!(v.current_rel, "start.md");
            assert_eq!(v.history, ["start.md"]);
            assert_eq!(v.history_ix, 0);
            assert_eq!(v.link_notice.as_deref(), Some("Retain notice"));
        });
        for (text, expected) in [
            (Some("# Other"), LinkStatus::MissingHeading),
            (Some("# Landing"), LinkStatus::Resolved),
            (None, LinkStatus::MissingDocument),
        ] {
            if let Some(text) = text {
                std::fs::write(root.as_path().join("target.md"), text).unwrap();
            } else {
                std::fs::rename(
                    root.as_path().join("target.md"),
                    root.as_path().join("renamed.md"),
                )
                .unwrap();
            }
            PAINTED_LINKS.with(|trace| trace.borrow_mut().as_mut().unwrap().clear());
            reader.update_in(visual, |v, window, cx| {
                let mut changes = tessera_core::Changes::default();
                changes.changed.insert("target.md".into());
                v.apply_vault_changes(changes, window, cx);
                assert!(
                    v.prepared_links.is_empty(),
                    "refresh immediately exposes pending evidence"
                );
            });
            visual.run_until_parked();
            reader.update_in(visual, |v, _, cx| {
                assert_eq!(v.prepared_links[url].status, expected);
                assert_eq!(v.content.read(cx).selected_text(), selection);
                assert_eq!(v.content.entity_id(), content);
                assert_eq!(v.history, ["start.md"]);
            });
            assert_paint(expected.is_missing());
        }
        // Force a late previous-source completion through the actual async seam.
        reader.update_in(visual, |v, _, cx| {
            v.refresh_link_preparation(cx);
            v.invalidate_links();
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, _, _| assert!(v.prepared_links.is_empty()));
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            source
        );
        PAINTED_LINKS.with(|trace| *trace.borrow_mut() = None);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn highlighted_heading_preparation_agrees_with_reader_navigation_and_back(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!(
            "tessera-highlight-heading-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n\n[Go](target.md#Landing)").unwrap();
        std::fs::write(root.join("target.md"), "# ==Landing==\n\n[Self](#Landing)").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let url = reader.update_in(visual, |v, _, _| {
            let url = v.link_identities[0].url.clone();
            assert_eq!(v.prepared_links[&url].status, LinkStatus::Resolved);
            url
        });
        visual.update(|window, cx| handle_link(&reader.downgrade(), &url, window, cx));
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert_eq!(v.current_rel, "target.md");
            assert!(v.link_notice.is_none());
            assert_eq!(
                v.prepared_links[&v.link_identities[0].url].status,
                LinkStatus::Resolved,
                "self and cross-note heading states agree"
            );
            assert!(tessera_core::document_links::heading(&v.note_source, "Landing").is_ok());
            v.history_move(-1, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, _, _| assert_eq!(v.current_rel, "start.md"));
        // Read failure remains unknown, never a fabricated missing heading.
        std::fs::write(root.join("target.md"), [0xff, 0xfe]).unwrap();
        reader.update_in(visual, |v, _, cx| v.refresh_link_preparation(cx));
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            // Progressive first-source URLs may be unresolved until inventory;
            // Back prepares a new emitted identity against the full inventory.
            assert_eq!(
                v.prepared_links[&v.link_identities[0].url].status,
                LinkStatus::Unknown
            );
        });
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn adjacent_embed_setext_preparation_agrees_with_navigation_and_self(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!(
            "tessera-highlight-heading-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n\n[Go](target.md#Landing)").unwrap();
        std::fs::write(
            root.join("target.md"),
            "![[child]]\nLanding\n=======\n\n[Self](#Landing)",
        )
        .unwrap();
        std::fs::write(root.join("child.md"), "Child body").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let url = reader.update_in(visual, |v, _, _| {
            let url = v.link_identities[0].url.clone();
            assert_eq!(v.prepared_links[&url].status, LinkStatus::Resolved);
            url
        });
        visual.update(|window, cx| handle_link(&reader.downgrade(), &url, window, cx));
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert_eq!(v.current_rel, "target.md");
            assert!(v.link_notice.is_none());
            assert_eq!(
                v.prepared_links[&v.link_identities[0].url].status,
                LinkStatus::Resolved,
                "self and cross-note heading states agree"
            );
            assert!(tessera_core::document_links::heading(&v.note_source, "Landing").is_ok());
            v.history_move(-1, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, _, _| assert_eq!(v.current_rel, "start.md"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn unresolved_embed_setext_preparation_agrees_with_navigation_and_self(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!(
            "tessera-highlight-heading-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n\n[Go](target.md#Landing)").unwrap();
        std::fs::write(
            root.join("target.md"),
            "![[child]]\nLanding\n=======\n\n[Self](#Landing)",
        )
        .unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let url = reader.update_in(visual, |v, _, _| {
            let url = v.link_identities[0].url.clone();
            assert_eq!(v.prepared_links[&url].status, LinkStatus::Resolved);
            url
        });
        visual.update(|window, cx| handle_link(&reader.downgrade(), &url, window, cx));
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert_eq!(v.current_rel, "target.md");
            assert!(v.link_notice.is_none());
            assert_eq!(
                v.prepared_links[&v.link_identities[0].url].status,
                LinkStatus::Resolved,
                "self and cross-note heading states agree"
            );
            assert!(tessera_core::document_links::heading(&v.note_source, "Landing").is_ok());
            v.history_move(-1, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, _, _| assert_eq!(v.current_rel, "start.md"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
