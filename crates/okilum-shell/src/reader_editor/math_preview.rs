//! The floating preview under an inline `$…$` formula while the caret is in
//! it (#1645). The source stays in the line; the card below it draws the
//! formula in two dimensions, or the reason it cannot, in the danger color.
//! It is an overlay: the text never moves to make room for it.
use super::*;
use crate::source_presentation::FormulaAt;
use okilum_core::tex::{self, Node};

pub(crate) use okilum_shell_text::math_layout::row;

/// Space between the formula's line and the card.
const GAP: f32 = 4.;
/// Formula size relative to the note text.
const SCALE: f32 = 1.2;

pub(super) struct MathPreview {
    reader: WeakEntity<Reader>,
    input: Entity<EditorState>,
    /// The last formula parsed, so moving the caret inside it parses nothing.
    parsed: Option<(String, Result<Vec<Node>, tex::Error>)>,
    /// Where the card was drawn, to redraw it once if the editor's layout
    /// moved the formula in the same frame (reveal, wrap).
    shown_at: Option<Point<Pixels>>,
    _subscriptions: [Subscription; 2],
}

impl MathPreview {
    pub(super) fn new(
        reader: Entity<Reader>,
        input: Entity<EditorState>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            _subscriptions: [
                cx.observe(&input, |_, _, cx| cx.notify()),
                cx.observe(&reader, |_, _, cx| cx.notify()),
            ],
            reader: reader.downgrade(),
            input,
            parsed: None,
            shown_at: None,
        }
    }

    /// The formula under the caret and where its card goes, in window
    /// coordinates: below the formula's last row, at its first glyph.
    fn target(&self, window: &Window, cx: &App) -> Option<(FormulaAt, Point<Pixels>)> {
        let reader = self.reader.upgrade()?;
        let input = self.input.read(cx);
        if !input.focus_handle(cx).is_focused(window) {
            return None;
        }
        let formula = reader
            .read(cx)
            .pane
            .editing
            .as_ref()
            .filter(|editing| editing.input == self.input)?
            .live_preview
            .formula_at(input)?;
        let start = input.range_to_bounds(&(formula.range.start..formula.range.start))?;
        let end = input.range_to_bounds(&(formula.range.end..formula.range.end))?;
        let position = point(start.left(), start.bottom().max(end.bottom()) + px(GAP));
        Some((formula, position))
    }
}

impl Render for MathPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().absolute().size_0();
        let Some((formula, position)) = self.target(window, cx) else {
            self.shown_at = None;
            return root;
        };
        if self
            .parsed
            .as_ref()
            .is_none_or(|(tex, _)| *tex != formula.tex)
        {
            self.parsed = Some((formula.tex.clone(), tex::parse(&formula.tex)));
        }
        self.shown_at = Some(position);
        // The editor lays out after this render; follow it if it moved.
        cx.on_next_frame(window, |this, window, cx| {
            let now = this.target(window, cx).map(|(_, at)| at);
            if now.is_some() && now != this.shown_at {
                cx.notify();
            }
        });
        let palette = brand::palette(cx);
        let size = reader_ui_state::font_size(cx) * SCALE;
        let body = match self.parsed.as_ref().map(|(_, parsed)| parsed) {
            Some(Ok(nodes)) => div()
                .debug_selector(|| "math-preview-formula".into())
                .text_color(palette.text)
                .child(row(nodes, size, palette.text))
                .into_any_element(),
            Some(Err(error)) => div()
                .debug_selector(|| "math-preview-error".into())
                .text_sm()
                .text_color(palette.danger)
                .child(SharedString::from(error.to_string()))
                .into_any_element(),
            None => return root,
        };
        root.child(
            deferred(
                anchored()
                    .position(position)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("math-preview-card")
                            .debug_selector(|| "math-preview-card".into())
                            .max_w(px(640.))
                            .overflow_hidden()
                            .px_3()
                            .py_2()
                            .rounded_lg()
                            .bg(palette.surface)
                            .border_1()
                            .border_color(palette.border)
                            .shadow_md()
                            .font_family(reader_fonts::note(cx))
                            .child(body),
                    ),
            )
            .with_priority(1),
        )
    }
}
