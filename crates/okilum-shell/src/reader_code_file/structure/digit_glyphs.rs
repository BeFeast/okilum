//! Prepare bounded printable ASCII phases in every colour used by Tree labels.
use super::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, PartialEq)]
struct Key {
    window: WindowId,
    font: Font,
    size: Pixels,
    scale: f32,
    colors: Vec<Hsla>,
}

#[derive(Default)]
pub(super) struct DigitGlyphs(Rc<RefCell<Option<Key>>>);

#[cfg(test)]
pub(in super::super) type GlyphRequest = (char, u8, u8, Pixels, Hsla);

#[cfg(test)]
thread_local! {
    pub(in super::super) static REQUESTS: RefCell<Vec<GlyphRequest>> = const { RefCell::new(Vec::new()) };
}

pub(in super::super) const LABEL_GLYPHS: &str = "!\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";

impl DigitGlyphs {
    #[inline(never)]
    pub(super) fn element(&self, cx: &App) -> AnyElement {
        let prepared = self.0.clone();
        let mut colors = vec![brand::reader_palette(cx).text_faint];
        for name in ["string", "number", "boolean", "constant"] {
            if let Some(color) = cx
                .theme()
                .highlight_theme
                .style
                .syntax
                .style(name)
                .and_then(|style| style.color)
            {
                if !colors.contains(&color) {
                    colors.push(color);
                }
            }
        }
        div()
            .absolute()
            .font_family(reader_fonts::code(cx))
            .font_weight(FontWeight::NORMAL)
            .text_size(text_size(cx))
            .child(canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    let style = window.text_style();
                    let mut colors = colors.clone();
                    if !colors.contains(&style.color) {
                        colors.push(style.color);
                    }
                    let key = Key {
                        window: window.window_handle().window_id(),
                        font: style.font(),
                        size: style.font_size.to_pixels(window.rem_size()),
                        scale: window.scale_factor(),
                        colors,
                    };
                    if prepared.borrow().as_ref() == Some(&key) {
                        return;
                    }
                    // No document walk: bounded ASCII at GPUI's finite subpixel
                    // phases in actual foreground/count/syntax colours. macOS
                    // includes colour-dependent dilation in its atlas key. Row text
                    // keeps its exact size, shaping and positions. An empty
                    // content mask prevents these cache fills from painting.
                    let line = window.text_system().shape_line(
                        LABEL_GLYPHS.into(),
                        key.size,
                        &[style.to_run(LABEL_GLYPHS.len())],
                        None,
                    );
                    let mask = ContentMask {
                        bounds: Bounds::new(Point::default(), Size::default()),
                    };
                    let result: anyhow::Result<()> =
                        window.with_content_mask(Some(mask), |window| {
                            for color in &key.colors {
                                for run in &line.runs {
                                    for glyph in &run.glyphs {
                                        for y in 0..SUBPIXEL_VARIANTS_Y {
                                            for x in 0..SUBPIXEL_VARIANTS_X {
                                                let origin = point(
                                                    px(100.
                                                        + x as f32 / SUBPIXEL_VARIANTS_X as f32)
                                                        / key.scale,
                                                    px(100.
                                                        + y as f32 / SUBPIXEL_VARIANTS_Y as f32)
                                                        / key.scale,
                                                );
                                                #[cfg(test)]
                                                REQUESTS.with(|requests| {
                                                    requests.borrow_mut().push((
                                                        line.text.as_bytes()[glyph.index] as char,
                                                        x,
                                                        y,
                                                        key.size,
                                                        *color,
                                                    ))
                                                });
                                                window.paint_glyph(
                                                    origin,
                                                    run.font_id,
                                                    glyph.id,
                                                    key.size,
                                                    *color,
                                                )?;
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(())
                        });
                    match result {
                        Ok(()) => *prepared.borrow_mut() = Some(key),
                        Err(error) => log::warn!("Tree label glyph preparation failed: {error:#}"),
                    }
                },
            ))
            .into_any_element()
    }
}
