//! The floating preview under an inline `$…$` formula while the caret is in
//! it (#1645). The source stays in the line; the card below it draws the
//! formula in two dimensions, or the reason it cannot, in the danger color.
//! It is an overlay: the text never moves to make room for it.
use super::*;
use crate::source_presentation::FormulaAt;
use okilum_core::tex::{self, Class, Node};

/// Space between the formula's line and the card.
const GAP: f32 = 4.;
/// Formula size relative to the note text.
const SCALE: f32 = 1.2;
/// Scripts and limits relative to their base.
const SCRIPT: f32 = 0.7;
const MIN_SIZE: f32 = 8.;

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

fn scaled(size: f32, by: f32) -> f32 {
    (size * by).max(MIN_SIZE)
}

/// A horizontal list centred on the math axis, with TeX's spacing around
/// operators and relations.
fn row(nodes: &[Node], size: f32, color: Hsla) -> AnyElement {
    let mut previous: Option<Class> = None;
    let mut children = Vec::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        let mut class = node.class();
        if class == Class::Bin
            && matches!(
                previous,
                None | Some(Class::Bin | Class::Rel | Class::Open | Class::Punct | Class::Large)
            )
        {
            class = Class::Ord;
        }
        let last = index + 1 == nodes.len();
        let (before, after) = match class {
            Class::Bin if !last => (4., 4.),
            Class::Rel if previous.is_some() && !last => (5., 5.),
            Class::Punct if !last => (0., 3.),
            Class::Function | Class::Large if !last => (0., 3.),
            _ => (0., 0.),
        };
        let mu = size / 18.;
        children.push(
            div()
                .ml(px(before * mu))
                .mr(px(after * mu))
                .child(element(node, size, color))
                .into_any_element(),
        );
        previous = Some(class);
    }
    h_flex()
        .items_center()
        .flex_none()
        .children(children)
        .into_any_element()
}

fn glyph(text: &str, size: f32, italic: bool, bold: bool) -> Div {
    div()
        .flex_none()
        .text_size(px(size))
        .line_height(px(size * 1.25))
        .whitespace_nowrap()
        .when(italic, |d| d.italic())
        .when(bold, |d| d.font_weight(FontWeight::BOLD))
        .child(SharedString::from(text.to_owned()))
}

/// True when `nodes` stand taller than a line, so delimiters grow with them.
fn tall(nodes: &[Node]) -> bool {
    nodes.iter().any(|node| match node {
        Node::Frac { .. } => true,
        Node::Scripts { base, sub, sup } => {
            (base.class() == Class::Large && limits(base)) || (sub.is_some() && sup.is_some())
        }
        Node::Group(inner) | Node::Fenced { body: inner, .. } | Node::Sqrt { body: inner, .. } => {
            tall(inner)
        }
        _ => false,
    })
}

/// Sums and products take their scripts above and below; integrals beside.
fn limits(base: &Node) -> bool {
    matches!(base, Node::Atom { text, class: Class::Large, .. } if !matches!(text.as_str(), "∫" | "∬" | "∭" | "∮"))
}

fn element(node: &Node, size: f32, color: Hsla) -> AnyElement {
    match node {
        Node::Atom { text, class, font } => {
            let italic = *class == Class::Var && !font.upright;
            let size = if *class == Class::Large {
                size * 1.3
            } else {
                size
            };
            glyph(text, size, italic, font.bold).into_any_element()
        }
        Node::Group(nodes) => row(nodes, size, color),
        Node::Scripts { base, sub, sup } => {
            let small = scaled(size, SCRIPT);
            if limits(base) {
                return v_flex()
                    .items_center()
                    .flex_none()
                    .children(sup.as_ref().map(|sup| row(sup, small, color)))
                    .child(element(base, size, color))
                    .children(sub.as_ref().map(|sub| row(sub, small, color)))
                    .into_any_element();
            }
            // A lone superscript rises, a lone subscript drops: the empty
            // half keeps the script off the axis.
            let spacer = || div().h(px(small * 0.6)).into_any_element();
            let scripts = v_flex()
                .flex_none()
                .items_start()
                .child(
                    sup.as_ref()
                        .map_or_else(spacer, |sup| row(sup, small, color)),
                )
                .child(
                    sub.as_ref()
                        .map_or_else(spacer, |sub| row(sub, small, color)),
                );
            h_flex()
                .items_center()
                .flex_none()
                .child(element(base, size, color))
                .child(scripts)
                .into_any_element()
        }
        Node::Frac { num, den, bar } => {
            let part = scaled(size, 0.9);
            v_flex()
                .items_center()
                .flex_none()
                .px(px(size * 0.1))
                .child(row(num, part, color))
                .child(
                    div()
                        .w_full()
                        .h(px(1.))
                        .my(px(size * 0.08))
                        .when(*bar, |d| d.bg(color)),
                )
                .child(row(den, part, color))
                .into_any_element()
        }
        Node::Sqrt { index, body } => h_flex()
            .items_end()
            .flex_none()
            .children(index.as_ref().map(|index| {
                div()
                    .mb(px(size * 0.5))
                    .child(row(index, scaled(size, 0.55), color))
            }))
            .child(glyph(
                "√",
                size * if tall(body) { 1.6 } else { 1.1 },
                false,
                false,
            ))
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(color)
                    .pl(px(size * 0.05))
                    .child(row(body, size, color)),
            )
            .into_any_element(),
        Node::Fenced { open, close, body } => {
            let delimiter = size * if tall(body) { 1.8 } else { 1. };
            h_flex()
                .items_center()
                .flex_none()
                .when(!open.is_empty(), |d| {
                    d.child(glyph(open, delimiter, false, false))
                })
                .child(row(body, size, color))
                .when(!close.is_empty(), |d| {
                    d.child(glyph(close, delimiter, false, false))
                })
                .into_any_element()
        }
        Node::Accent { mark, body } => {
            let inner = row(body, size, color);
            match *mark {
                '\u{305}' => div()
                    .flex_none()
                    .border_t_1()
                    .border_color(color)
                    .child(inner)
                    .into_any_element(),
                '\u{332}' => div()
                    .flex_none()
                    .border_b_1()
                    .border_color(color)
                    .child(inner)
                    .into_any_element(),
                mark => v_flex()
                    .items_center()
                    .flex_none()
                    .child(
                        glyph(spacing_accent(mark), scaled(size, 0.8), false, false)
                            .line_height(px(size * 0.45)),
                    )
                    .child(inner)
                    .into_any_element(),
            }
        }
        Node::Space(mu) => div()
            .flex_none()
            .w(px(size * f32::from(*mu) / 18.))
            .into_any_element(),
    }
}

/// The spacing form of a combining accent, drawn above its base.
fn spacing_accent(mark: char) -> &'static str {
    match mark {
        '\u{302}' => "ˆ",
        '\u{30C}' => "ˇ",
        '\u{303}' => "˜",
        '\u{301}' => "´",
        '\u{300}' => "`",
        '\u{307}' => "˙",
        '\u{308}' => "¨",
        '\u{306}' => "˘",
        '\u{20D7}' => "→",
        _ => "",
    }
}
