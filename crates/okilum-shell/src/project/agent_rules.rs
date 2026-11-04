//! Read-only Rules surface (#1590). Discovery runs on the background executor;
//! the owner forwards existing vault write batches. No timers/provider calls.
use super::*;
use okilum_core::agent_rules::{discover, Provenance, Refresh, RuleStack};

pub(crate) struct RulesView {
    stack: RuleStack,
    opened: Option<usize>,
    refresh: Refresh,
}

impl RulesView {
    pub(crate) fn new(stack: RuleStack) -> Self {
        Self {
            stack,
            opened: None,
            refresh: Refresh::default(),
        }
    }

    /// Create only when Rules is enabled/active. No filesystem access on the UI.
    pub(crate) fn for_project(vault: PathBuf, project: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new(RuleStack::default());
        view.bind(vault, project, cx);
        view
    }

    /// Explicit owner scope change; immediately hide the previous project's rules.
    pub(crate) fn bind(&mut self, vault: PathBuf, project: PathBuf, cx: &mut Context<Self>) {
        self.stack = RuleStack::default();
        self.opened = None;
        self.refresh.bind(vault, project);
        self.start_discovery(cx);
        cx.notify();
    }

    /// Forward the existing watcher's coalesced write/remove/directory/rescan
    /// batches, not raw notify read/access events. Arbitrary @import names and
    /// symlink targets make filename-only filtering unsafe here.
    pub(crate) fn external_changed(
        &mut self,
        changes: &okilum_core::watch::Changes,
        cx: &mut Context<Self>,
    ) {
        if !changes.is_empty() {
            self.reload(cx);
        }
    }

    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.refresh.request();
        self.start_discovery(cx);
        cx.notify();
    }

    /// Call when Rules/Agents is disabled or its owner leaves this view. A running
    /// read may finish, but cannot install data or schedule another read.
    pub(crate) fn deactivate(&mut self, cx: &mut Context<Self>) {
        self.refresh.deactivate();
        self.stack = RuleStack::default();
        self.opened = None;
        cx.notify();
    }

    /// Compatibility hook for an owner-provided snapshot; supersedes worker reads.
    #[allow(dead_code, reason = "owner snapshot hook, not wired yet")]
    pub(crate) fn replace(&mut self, stack: RuleStack, cx: &mut Context<Self>) {
        self.refresh.supersede();
        self.install(stack);
        cx.notify();
    }

    fn install(&mut self, stack: RuleStack) {
        // Track the source identity rather than an index: creating/deleting a
        // preceding layer must never redirect an already-open rule to another file.
        self.opened = self.opened.and_then(|index| {
            let old = self.stack.layers.get(index)?;
            stack.layers.iter().position(|layer| {
                layer.path == old.path
                    && layer.provenance == old.provenance
                    && layer.source.is_some()
            })
        });
        self.stack = stack;
    }

    fn start_discovery(&mut self, cx: &mut Context<Self>) {
        let Some(ticket) = self.refresh.begin() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let job = ticket.clone();
            let stack = cx
                .background_executor()
                .spawn(async move { discover(&job.vault, &job.project) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.refresh.finish(&ticket) {
                    this.install(stack);
                }
                // At most one coalesced follow-up; no periodic reads or polling.
                this.start_discovery(cx);
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for RulesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = v_flex()
            .id("project-rules")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_3()
            .p_3()
            .child(div().child("Rules"))
            .child(div().text_color(brand::palette(cx).text_muted).child(
                "Local instruction files. Remote agent loading is unknown. Tokens are approximate.",
            ));
        panel = panel.child(
            Button::new("rules-refresh")
                .ghost()
                .label("Refresh")
                .disabled(!self.refresh.is_bound())
                .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
        );
        if self.refresh.is_pending() {
            panel = panel.child(
                "Refreshing local rules; showing the previous snapshot until discovery finishes.",
            );
        }
        if let Some(index) = self.opened {
            if let Some(layer) = self.stack.layers.get(index) {
                panel = panel
                    .child(
                        Button::new("rules-back")
                            .ghost()
                            .label("Back to layers")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.opened = None;
                                cx.notify();
                            })),
                    )
                    .child(div().child(format!(
                        "{} · Read only · Discovery snapshot",
                        layer.path.display()
                    )))
                    .child(div().child(layer.source.clone().unwrap_or_default()));
                return panel.into_any_element();
            }
        }
        panel = panel.child(div().child(format!(
            "{} bytes · ≈ {} tokens · {} entries (including provider candidates and repeated references)",
            self.stack.total_bytes(), self.stack.approximate_tokens(), self.stack.layers.len()
        )));
        if self.stack.layers.is_empty() && !self.refresh.is_pending() {
            panel = panel.child("No ancestor instruction files found for this project.");
        }
        for diagnostic in &self.stack.diagnostics {
            panel = panel.child(
                div()
                    .text_color(cx.theme().danger)
                    .child(diagnostic.clone()),
            );
        }
        for (index, layer) in self.stack.layers.iter().enumerate() {
            let provenance = match &layer.provenance {
                Provenance::Ancestor => "Ancestor / project instruction".to_owned(),
                Provenance::Import { from, line } => {
                    format!("Imported from {}:{line}", from.display())
                }
                Provenance::Alternative => "Provider candidate · order unknown".to_owned(),
            };
            let mut row = v_flex()
                .gap_1()
                .id(("rule-layer", index))
                .child(div().child(format!("{} · {}", index + 1, layer.path.display())))
                .child(
                    div()
                        .text_color(brand::palette(cx).text_muted)
                        .child(provenance),
                )
                .child(div().child(format!(
                    "{} bytes · ≈ {} tokens",
                    layer.bytes(),
                    layer.approximate_tokens()
                )));
            if !layer.duplicate_lines.is_empty() {
                row = row.child(div().child(format!(
                        "Repeated text across layers at lines {}",
                        layer
                            .duplicate_lines
                            .iter()
                            .map(usize::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
            }
            if let Some(error) = &layer.error {
                row = row.child(div().text_color(cx.theme().danger).child(error.clone()));
            }
            row = row.child(
                Button::new(("rule-open", index))
                    .ghost()
                    .label("Open read only")
                    .disabled(layer.source.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.opened = Some(index);
                        cx.notify();
                    })),
            );
            panel = panel.child(row);
        }
        panel.into_any_element()
    }
}
