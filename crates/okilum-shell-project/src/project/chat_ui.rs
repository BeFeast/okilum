//! The Manager's primary surface is a conversation. Operational detail is opt-in.
use super::*;

impl ProjectView {
    pub(super) fn render_chrome(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = self
            .gateway
            .as_ref()
            .map(|gateway| gateway.status())
            .unwrap_or(GatewayStatus::NotConnected);
        let can_open = self.identity.as_ref().zip(self.chat.as_ref()).is_some();
        #[cfg(native_manager)]
        let can_open = can_open || (self.native.active() && self.chat.is_some());
        // #2748: identity, section navigation, status and actions share one
        // row. Candidates run from fullest to most compact: the status label
        // collapses to its dot, then the title goes, then Open in T3 moves
        // into the Manager menu, then the tabs shrink and finally become one
        // section menu button. The first candidate whose natural width fits
        // the panel is painted.
        let candidates = HEADER_CANDIDATES
            .iter()
            .map(|candidate| {
                (
                    self.header_navigation(candidate.title, candidate.tabs, cx),
                    self.header_actions(
                        &status,
                        candidate.status_label,
                        can_open && candidate.open_inline,
                        can_open,
                        cx,
                    ),
                )
            })
            .collect();
        fit_header(
            candidates,
            self.header_open_in_menu.clone(),
            brand::palette(cx).border_subtle,
        )
    }

    fn header_navigation(&self, title: bool, tabs: Tabs, cx: &mut Context<Self>) -> AnyElement {
        const SECTIONS: [(&str, &str, ProjectSection); 3] = [
            ("project-section-manager", "Chat", ProjectSection::Manager),
            ("project-section-rules", "Rules", ProjectSection::Rules),
            ("project-section-skills", "Skills", ProjectSection::Skills),
        ];
        let tabs = if tabs == Tabs::Picker {
            // The narrowest row names the current section and lists all
            // three in a menu: one button instead of three.
            use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
            let current = SECTIONS
                .iter()
                .find(|(_, _, section)| *section == self.section)
                .map_or("Chat", |(_, label, _)| *label);
            let owner = cx.entity().downgrade();
            let selected = self.section;
            Button::new("project-section-picker")
                .debug_selector(|| "project-section-picker".into())
                .ghost()
                .small()
                .label(current)
                .dropdown_caret(true)
                .tooltip("Section")
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |mut menu, _, _| {
                    for (_, label, section) in SECTIONS {
                        let owner = owner.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .checked(section == selected)
                                .on_click(move |_, _, cx| {
                                    let _ = owner
                                        .update(cx, |this, cx| this.select_section(section, cx));
                                }),
                        );
                    }
                    menu
                })
                .into_any_element()
        } else {
            let compact = tabs == Tabs::Compact;
            let mut row = h_flex()
                .id("project-sections")
                .flex_none()
                .items_center()
                .map(|row| if compact { row.gap_0p5() } else { row.gap_1() });
            for (id, label, section) in SECTIONS {
                let tab = Button::new(id)
                    .debug_selector(move || format!("project-section-label-{label}"))
                    .ghost()
                    .label(label)
                    .selected(self.section == section)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_section(section, cx)));
                row = row.child(if compact { tab.xsmall() } else { tab.small() });
            }
            row.into_any_element()
        };
        h_flex()
            .flex_none()
            .items_center()
            .gap_2()
            .when(title, |row| {
                row.child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(design::CHROME))
                        .line_height(px(design::LINE))
                        .debug_selector(|| "project-manager-title".into())
                        .child("Manager"),
                )
            })
            .child(tabs)
            .into_any_element()
    }

    fn header_actions(
        &self,
        status: &GatewayStatus,
        status_label: bool,
        open_inline: bool,
        can_open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut actions = h_flex()
            .flex_none()
            .items_center()
            .gap_1()
            .child(self.connection_line(status, status_label, can_open, cx));
        if open_inline {
            actions = actions.child(
                Button::new("project-open-t3")
                    .debug_selector(|| "project-open-t3".into())
                    .ghost()
                    .small()
                    .icon(IconName::ExternalLink)
                    .tooltip("Open in T3")
                    .disabled(self.opening_t3)
                    .on_click(cx.listener(|this, _, window, cx| this.open_in_t3(window, cx))),
            );
        }
        actions.into_any_element()
    }

    pub(super) fn render_model_controls(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let controls = h_flex()
            .max_w_full()
            .min_w_0()
            .items_center()
            .flex_wrap()
            .gap_2();
        if let Some(target) = self.manager_model_target() {
            controls
                .child(self.model_picker("project-manager-model", target, window, cx))
                .into_any_element()
        } else {
            controls.into_any_element()
        }
    }

    pub(super) fn render_chat_diagnostics(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = v_flex()
            .gap_2()
            .debug_selector(|| "project-diagnostics".into())
            .child(
                h_flex().child("Details").child(div().flex_1()).child(
                    Button::new("project-diagnostics-close")
                        .debug_selector(|| "project-diagnostics-close".into())
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .tooltip("Close details")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.connection_details = false;
                            cx.notify();
                        })),
                ),
            );
        #[cfg(native_manager)]
        if !self.native.title().is_empty() {
            panel = panel.child(format!("Title: {}", self.native.title()));
        }
        if let Some(target) = &self.model_target {
            panel = panel.child(format!("Thread: {}", target.thread_id));
        }
        if let Some(binding) = &self.binding {
            panel = panel.child(format!(
                "Bound thread: {}",
                binding.manager.thread_id.as_deref().unwrap_or("none")
            ));
        }
        if let Some(snapshot) = &self.models {
            panel = panel.child(format!(
                "Model: {} / {}; thread mode: {}; credential mode: {}",
                snapshot.configuration.model_selection.instance_id,
                snapshot.configuration.model_selection.model,
                snapshot.configuration.runtime_mode,
                snapshot.catalog.runtime_mode
            ));
        }
        if let Some(error) = &self.model_error {
            panel = panel.child(error.clone());
        }
        panel
            .child(
                div()
                    .debug_selector(|| "project-model-scheduler-limit".into())
                    .child(okilum_t3::models::SCHEDULED_TASK_LIMIT),
            )
            .into_any_element()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tabs {
    Regular,
    Compact,
    /// One button naming the current section; its menu lists all three.
    Picker,
}

struct HeaderCandidate {
    title: bool,
    tabs: Tabs,
    status_label: bool,
    open_inline: bool,
}

const fn candidate(
    title: bool,
    tabs: Tabs,
    status_label: bool,
    open_inline: bool,
) -> HeaderCandidate {
    HeaderCandidate {
        title,
        tabs,
        status_label,
        open_inline,
    }
}

const HEADER_CANDIDATES: [HeaderCandidate; 6] = [
    candidate(true, Tabs::Regular, true, true),
    candidate(true, Tabs::Regular, false, true),
    candidate(false, Tabs::Regular, false, true),
    candidate(false, Tabs::Regular, false, false),
    candidate(false, Tabs::Compact, false, false),
    candidate(false, Tabs::Picker, false, false),
];

/// Lays out each candidate's navigation and actions at their natural widths
/// and paints the first pair that fits side by side; the last always paints.
/// Only the painted pair registers hitboxes, tab stops and debug bounds;
/// `open_in_menu` records whether that pair moved Open in T3 into the menu.
fn fit_header(
    candidates: Vec<(AnyElement, AnyElement)>,
    open_in_menu: Rc<std::cell::Cell<bool>>,
    border: Hsla,
) -> AnyElement {
    div()
        .relative()
        .w_full()
        .h(px(40.))
        .flex_none()
        .border_b_1()
        .border_color(border)
        .debug_selector(|| "project-chat-header".into())
        .child(
            canvas(
                move |bounds, window, cx| {
                    let natural = size(AvailableSpace::MaxContent, AvailableSpace::MaxContent);
                    let inset = px(design::INSET);
                    let room = bounds.size.width - inset * 2. - px(design::GAP);
                    let last = candidates.len().saturating_sub(1);
                    for (index, (mut navigation, mut actions)) in candidates.into_iter().enumerate()
                    {
                        let left = navigation.layout_as_root(natural, window, cx);
                        let right = actions.layout_as_root(natural, window, cx);
                        if index < last && left.width + right.width > room {
                            continue;
                        }
                        open_in_menu.set(!HEADER_CANDIDATES[index].open_inline);
                        let top = |height: Pixels| {
                            bounds.top() + ((bounds.size.height - height) / 2.).max(px(0.))
                        };
                        navigation.prepaint_at(
                            point(bounds.left() + inset, top(left.height)),
                            window,
                            cx,
                        );
                        actions.prepaint_at(
                            point(bounds.right() - inset - right.width, top(right.height)),
                            window,
                            cx,
                        );
                        return Some((navigation, actions));
                    }
                    None
                },
                |_, painted, window, cx| {
                    if let Some((mut navigation, mut actions)) = painted {
                        navigation.paint(window, cx);
                        actions.paint(window, cx);
                    }
                },
            )
            .absolute()
            .size_full(),
        )
        .into_any_element()
}
