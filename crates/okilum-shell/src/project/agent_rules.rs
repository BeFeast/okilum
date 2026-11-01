//! Read-only Rules surface (#1590). The owner discovers off the UI thread and
//! installs the resulting snapshot. This view never contacts an executor.
use super::*;
use okilum_core::agent_rules::{Provenance, RuleStack};

pub(crate) struct RulesView {
    stack: RuleStack,
    opened: Option<usize>,
}

impl RulesView {
    pub(crate) fn new(stack: RuleStack) -> Self {
        Self {
            stack,
            opened: None,
        }
    }

    /// The parent owns project identity/generation checks before replacement.
    pub(crate) fn replace(&mut self, stack: RuleStack, cx: &mut Context<Self>) {
        self.stack = stack;
        self.opened = None;
        cx.notify();
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
        if self.stack.layers.is_empty() {
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
                row = row.child(
                    div().child(format!(
                        "Repeated text across layers at lines {}",
                        layer
                            .duplicate_lines
                            .iter()
                            .map(usize::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                );
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
