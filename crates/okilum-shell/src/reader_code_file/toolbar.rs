//! Shared slot descriptions and the single bottom line. Every element builder
//! is erased before composing the native view (Windows main-stack constraint).
use super::*;
use reader_viewer_chrome::{ViewerAction, ViewerChrome, ViewerView};
use std::rc::Rc;

impl CodePreview {
    pub(crate) fn compact_chrome(&self, entity: Entity<Self>, cx: &App) -> ViewerChrome {
        let mut chrome = self.chrome(entity, cx);
        chrome.compact_menu_id = Some("code-compact-more");
        let root = self.root.clone();
        let rel = self.rel.clone();
        chrome.open_actions.push(ViewerAction::new(
            "code-open-default",
            Icon::new(IconName::ExternalLink),
            "Open with default app",
            Rc::new(move |window, cx| {
                reader_files::run(reader_files::FileAction::Open, &root, &rel, window, cx)
            }),
        ));
        chrome
    }
    pub(crate) fn chrome(&self, entity: Entity<Self>, cx: &App) -> ViewerChrome {
        self.describe(entity, cx, true)
    }
    pub(crate) fn header_chrome(&self, entity: Entity<Self>, cx: &App) -> ViewerChrome {
        self.describe(entity, cx, false)
    }
    fn describe(&self, entity: Entity<Self>, cx: &App, live: bool) -> ViewerChrome {
        let mut chrome = ViewerChrome::default();
        if !matches!(self.state, State::Content(_))
            && !(reader_delimited::eligible(&self.rel) && self.table.is_some())
        {
            return chrome;
        }
        for view in View::ALL {
            let availability = self.availability(view);
            if availability == Availability::Absent {
                continue;
            }
            let target = entity.clone();
            let mut action = ViewerAction::new(
                view.id(),
                view.icon(),
                view.label(),
                Rc::new(move |window, cx| {
                    target.update(cx, |this, cx| this.set_view(view, window, cx))
                }),
            );
            action = match view {
                View::Table => action.with_action(ShowTable, cx),
                View::Tree => action.with_action(ShowTree, cx),
                View::Formatted => action.with_action(ShowFormatted, cx),
                View::Raw => action.with_action(ShowRaw, cx),
            };
            if matches!(view, View::Formatted | View::Raw) {
                action.tooltip = reader_shortcuts::hint(
                    if view == View::Formatted {
                        "Formatted: display only, the file is not changed"
                    } else {
                        "Raw: the text as it is in the file"
                    },
                    action.shortcut.as_ref().unwrap().as_ref(),
                    cx,
                );
            }
            if let Availability::Disabled(reason) = availability {
                action.enabled = false;
                action.disabled_reason = Some(reason.into());
            }
            chrome.views.push(ViewerView {
                active: self.view == view,
                action,
            });
        }
        if chrome.views.len() < 2 {
            chrome.views.clear();
        }
        if self.view != View::Table || self.table.is_some() {
            chrome.find = Some(if live {
                self.find_target(entity.clone(), cx)
            } else {
                self.find_descriptor(entity.clone(), cx)
            });
        }
        let folding = self.fold_availability();
        if folding != Availability::Absent {
            for folded in [true, false] {
                let target = entity.clone();
                let mut action = ViewerAction::new(
                    if folded {
                        "code-fold-all"
                    } else {
                        "code-unfold-all"
                    },
                    Icon::default().path(fold_icon(folded)),
                    if folded { "Fold all" } else { "Unfold all" },
                    Rc::new(move |_, cx| target.update(cx, |this, cx| this.fold_all(folded, cx))),
                );
                action = if folded {
                    action.with_action(FoldAll, cx)
                } else {
                    action.with_action(UnfoldAll, cx)
                };
                if let Availability::Disabled(reason) = &folding {
                    action.enabled = false;
                    action.disabled_reason = Some(reason.clone().into());
                }
                chrome.actions.push(action);
            }
        }
        let target = entity.clone();
        let mut selection = ViewerAction::new(
            "code-copy-selection",
            Icon::new(IconName::Copy),
            "Copy selection",
            Rc::new(move |_, cx| target.update(cx, |this, cx| this.copy_selection(cx))),
        )
        .with_action(CopySelection, cx);
        selection.enabled = self.view != View::Tree && self.view != View::Table;
        if !selection.enabled {
            selection.disabled_reason = Some("Select text in Formatted or Raw".into());
        }
        chrome.copy_actions.push(selection);
        let target = entity.clone();
        let mut node = ViewerAction::new(
            "code-copy-node",
            Icon::new(IconName::Copy),
            "Copy node",
            Rc::new(move |window, cx| target.update(cx, |this, cx| this.copy_node(window, cx))),
        )
        .with_action(CopyNode, cx);
        node.enabled = live && self.node_copy_range(cx).is_some();
        if !node.enabled {
            node.disabled_reason = Some("No parsed node at the caret".into());
        }
        chrome.copy_actions.push(node);
        if let Some(reason) = self.readonly_reason() {
            let mut lock = ViewerAction::new(
                "code-readonly",
                Icon::default().path("icons/lock.svg"),
                "Read-only",
                Rc::new(|_, _| {}),
            );
            lock.tooltip = format!("Read-only: {reason}").into();
            chrome.state = Some(lock);
        }
        if let Some(invalid) = self
            .raw_document
            .as_ref()
            .and_then(|doc| doc.invalid.as_ref())
        {
            let target = entity.clone();
            let mut warning = ViewerAction::new(
                "code-invalid",
                Icon::new(IconName::TriangleAlert).text_color(brand::palette(cx).warning),
                "Go to error",
                Rc::new(move |window, cx| {
                    target.update(cx, |this, cx| this.go_to_error(window, cx))
                }),
            )
            .with_action(GoToError, cx);
            warning.tooltip = reader_shortcuts::hint(
                &format!(
                    "Not valid JSON: line {}, column {}: {}. Go to error",
                    invalid.line, invalid.column, invalid.message
                ),
                &GoToError,
                cx,
            );
            if chrome.state.is_some() {
                chrome.badges.push(warning);
            } else {
                chrome.state = Some(warning);
            }
        }
        if self.view == View::Table {
            if let Some(table) = &self.table {
                let mut table_chrome = table.read(cx).chrome(table.clone(), live, cx);
                table_chrome.views = chrome.views;
                // Badges render beside the state slot; never drop the file's
                // own state (the read-only lock) when the table has no warning.
                if let Some(own) = chrome.state {
                    match table_chrome.state {
                        Some(_) => table_chrome.badges.push(own),
                        None => table_chrome.state = Some(own),
                    }
                }
                table_chrome.badges.extend(chrome.badges);
                return table_chrome;
            }
        }
        if let Some(reason) = &self.table_failure {
            let mut warning = ViewerAction::new(
                "table-unavailable",
                Icon::new(IconName::TriangleAlert).text_color(brand::palette(cx).warning),
                "Table unavailable",
                Rc::new(|_, _| {}),
            );
            warning.enabled = false;
            warning.disabled_reason = Some(reason.clone().into());
            warning.tooltip = reason.clone().into();
            match chrome.state {
                Some(_) => chrome.badges.push(warning),
                None => chrome.state = Some(warning),
            }
        }
        if reader_delimited::eligible(&self.rel) {
            chrome.copy_actions.retain(|a| a.id != "code-copy-node");
        }
        chrome
    }

    pub(crate) fn record_edit_failure(&mut self, error: &anyhow::Error, cx: &mut Context<Self>) {
        if error.chain().any(|cause| cause.is::<EditLocationError>()) {
            self.storage_reason = Some("this folder cannot be written".into());
            cx.notify();
        }
    }
    fn readonly_reason(&self) -> Option<String> {
        if let Some(reason) = &self.storage_reason {
            return Some(reason.clone());
        }
        if language(&self.rel) == Some("xml") {
            return Some("XML files are view-only".into());
        }
        self.source_bytes
            .filter(|bytes| *bytes > EDIT_BYTES)
            .map(|bytes| format!("{} is over the 4 MB editing limit", view::size(bytes)))
    }
    pub(super) fn status(&self, entity: Entity<Self>, cx: &App) -> reader_status_line::StatusLine {
        use reader_status_line::{StatusLine, Tone};
        if self.view == View::Raw {
            if let Some(reason) = &self.table_failure {
                return StatusLine {
                    text: reason.clone().into(),
                    tone: Tone::Warning,
                    ..Default::default()
                };
            }
        }
        let Some(raw) = &self.raw_document else {
            return StatusLine::default();
        };
        let mut status = StatusLine::default();
        if let Some(invalid) = &raw.invalid {
            status.icon = Some(Icon::new(IconName::TriangleAlert));
            status.tone = Tone::Warning;
            status.label = "Not valid JSON.".into();
            status.text = format!(
                "Line {}, column {}: {}",
                invalid.line, invalid.column, invalid.message
            )
            .into();
            let target = entity.clone();
            let mut action = ViewerAction::new(
                "code-go-to-error",
                Icon::new(IconName::ArrowRight),
                "Go to error",
                Rc::new(move |window, cx| {
                    target.update(cx, |this, cx| this.go_to_error(window, cx))
                }),
            )
            .with_action(GoToError, cx);
            action.labelled = true;
            status.actions.push(action);
            let mut repair = ViewerAction::new(
                "code-repair",
                Icon::default().path("icons/wrench.svg"),
                "Repair…",
                Rc::new(|window, cx| {
                    window.dispatch_action(Box::new(reader_editor::RepairCode), cx)
                }),
            )
            .with_action(reader_editor::RepairCode, cx);
            repair.labelled = true;
            repair.enabled = self.editable();
            if !repair.enabled {
                repair.disabled_reason = self.readonly_reason().map(Into::into);
            }
            status.actions.push(repair);
        } else if self.json.as_ref().is_some_and(|json| json.formatting) {
            status.text = format!("Formatting {}…", view::size(raw.text.len())).into();
        } else if self.json.is_some() && raw.text.len() > PRETTY_BYTES && self.view == View::Raw {
            status.label = "Large file.".into();
            status.text = format!(
                "{}, opened as raw text to stay fast.",
                view::size(raw.text.len())
            )
            .into();
            if self.availability(View::Formatted).available() {
                let target = entity;
                let mut action = ViewerAction::new(
                    "code-show-formatted",
                    View::Formatted.icon(),
                    "Show formatted",
                    Rc::new(move |window, cx| {
                        target.update(cx, |this, cx| this.set_view(View::Formatted, window, cx))
                    }),
                )
                .with_action(ShowFormatted, cx);
                action.labelled = true;
                status.actions.push(action);
            }
        } else if raw.pages.len() > 1 && (self.view == View::Tree || self.paged) {
            status.label = "Large file.".into();
            status.text = if self.view == View::Tree {
                format!(
                    "{} on a single line. Tree shows all of it.",
                    view::size(raw.text.len())
                )
            } else {
                format!(
                    "{} on a single line, so {} shows it in parts. {} cover the whole file.",
                    view::size(raw.text.len()),
                    self.view.label(),
                    if self.availability(View::Tree).available() {
                        "Tree, Find and Copy"
                    } else {
                        "Find and Copy"
                    }
                )
            }
            .into();
        } else if let Some(reason) = self.readonly_reason().filter(|_| {
            self.storage_reason.is_some()
                || self.source_bytes.is_some_and(|bytes| bytes > EDIT_BYTES)
        }) {
            status.icon = Some(Icon::default().path("icons/lock.svg"));
            status.label = "Read-only.".into();
            status.text = reason.into();
            let root = self.root.clone();
            let rel = self.rel.clone();
            let mut open = ViewerAction::new(
                "code-status-open",
                Icon::new(IconName::ExternalLink),
                "Open with default app",
                Rc::new(move |window, cx| {
                    reader_files::run(reader_files::FileAction::Open, &root, &rel, window, cx)
                }),
            );
            open.labelled = true;
            status.actions.push(open);
        }
        status
    }

    #[inline(never)]
    pub(super) fn bottom_line(&self, cx: &mut Context<Self>) -> AnyElement {
        // The hosted table supplies the one shared bottom status line.
        if self.view == View::Table {
            return div().into_any_element();
        }
        let mut status = self.status(cx.entity(), cx);
        let trailing = (self.view != View::Tree
            && self.view != View::Table
            && self
                .document
                .as_ref()
                .is_some_and(|doc| doc.pages.len() > 1))
        .then(|| self.position(cx));
        if trailing.is_some() && status.label.is_empty() && status.text.is_empty() {
            status.label = "Position".into();
        }
        reader_status_line::render(&status, "code-status", trailing, cx)
    }
    #[inline(never)]
    fn position(&self, cx: &mut Context<Self>) -> AnyElement {
        let doc = self.document.as_ref().unwrap();
        let percentage =
            (doc.pages[self.page].start as f64 / doc.text.len().max(1) as f64 * 100.).round();
        let buttons = [false, true].map(|next| {
            reader_icon_button(
                if next {
                    "code-next-part"
                } else {
                    "code-previous-part"
                },
                if next {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronLeft
                },
                if next {
                    reader_shortcuts::hint("Next part", &NextPart, cx)
                } else {
                    reader_shortcuts::hint("Previous part", &PreviousPart, cx)
                },
                cx,
            )
            .debug_selector(move || {
                if next {
                    "code-next-part".into()
                } else {
                    "code-previous-part".into()
                }
            })
            .disabled(if next {
                self.page + 1 == doc.pages.len()
            } else {
                self.page == 0
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.show_page(
                    if next {
                        this.page + 1
                    } else {
                        this.page.saturating_sub(1)
                    },
                    window,
                    cx,
                )
            }))
        });
        h_flex()
            .flex_none()
            .gap_1()
            .child(div().text_xs().child(format!(
                "{percentage:.0}% of {}",
                view::size(doc.text.len())
            )))
            .children(buttons)
            .child(
                div()
                    .w(px(84.))
                    .debug_selector(|| "code-part-scrubber".into())
                    .child(gpui_component::slider::Slider::new(&self.scrubber).horizontal()),
            )
            .into_any_element()
    }
    #[inline(never)]
    pub(super) fn message(&self, message: &str, cx: &App) -> AnyElement {
        let root = self.root.clone();
        let rel = self.rel.clone();
        v_flex()
            .gap_2()
            .py_4()
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(message.to_owned()),
            )
            .child(
                Button::new("code-message-open")
                    .debug_selector(|| "code-message-open".into())
                    .ghost()
                    .label("Open with default app")
                    .icon(IconName::ExternalLink)
                    .on_click(move |_, window, cx| {
                        reader_files::run(reader_files::FileAction::Open, &root, &rel, window, cx)
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
std::thread_local! {
    static BUILT_FOLD_ICONS: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
}
#[cfg(test)]
pub(super) fn take_built_fold_icons() -> Vec<&'static str> {
    BUILT_FOLD_ICONS.with(|icons| std::mem::take(&mut *icons.borrow_mut()))
}

/// Fold glyphs must remain distinct from the adjacent note-width control.
pub(super) fn fold_icon(folded: bool) -> &'static str {
    let path = if folded {
        brand::READER_COLLAPSE_ICON
    } else {
        brand::READER_EXPAND_ICON
    };
    #[cfg(test)]
    BUILT_FOLD_ICONS.with(|icons| icons.borrow_mut().push(path));
    path
}
