//! App-wide document text zoom helpers and actions (non-Reader core).
//! The HUD overlay, window wheel/pinch handlers, and `Reader` glue stay in
//! `okilum-shell::reader_text_zoom` (#3235, epic #3241).
use crate::prelude::*;

gpui::actions!(note_text_zoom, [ZoomIn, ZoomOut, Reset]);

/// Themed code size at the default text size, scaled like body text
/// (GitHub #7: Settings text size used to leave Read code blocks unchanged).
pub fn code_block_font_size(cx: &App) -> Pixels {
    cx.theme().mono_font_size * (reader_ui_state::font_size(cx) / BODY_FONT_SIZE)
}

/// Keyboard, menu and overlay steps land on whole 10% values, so a zoom left
/// at 113% by a precision wheel goes to 120% / 110%, not 123% / 103%.
pub fn step(zoom: f32, direction: f32) -> f32 {
    ((zoom * 10.).round() + direction) / 10.
}

/// "150", "150%" or "150 %" → 1.5. Anything else is not a zoom.
pub fn parse_percent(text: &str) -> Option<f32> {
    let value = text
        .trim()
        .trim_end_matches('%')
        .trim()
        .parse::<f32>()
        .ok()?;
    (value.is_finite() && value > 0.).then_some(value / 100.)
}

pub fn modifier(modifiers: gpui::Modifiers, mac: bool) -> bool {
    !modifiers.alt
        && !modifiers.shift
        && if mac {
            modifiers.platform && !modifiers.control
        } else {
            modifiers.control && !modifiers.platform
        }
}

pub fn wheel_delta(delta: gpui::ScrollDelta) -> Option<f32> {
    let amount = match delta {
        // System line-count preferences must not multiply a physical wheel tick.
        gpui::ScrollDelta::Lines(lines) => {
            if lines.y.is_finite() && lines.y != 0. {
                lines.y.signum() * 0.1
            } else {
                return None;
            }
        }
        // Precision devices retain smooth deltas, bounded to 10% per event.
        gpui::ScrollDelta::Pixels(pixels) => (f32::from(pixels.y) / 400.).clamp(-0.1, 0.1),
    };
    (amount.is_finite() && amount != 0.).then_some(amount)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{ScrollDelta, TestAppContext};

    #[gpui::test]
    fn scale_persists_without_changing_base_font_or_other_presentation(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            reader_ui_state::install(temp.path(), cx);
            reader_ui_state::set_reading(20., 960., cx);
            reader_ui_state::set_text_zoom(1.4, cx);
            assert_eq!(reader_ui_state::font_size(cx), 28.);
            assert_eq!(reader_ui_state::base_font_size(cx), 20.);
            reader_ui_state::flush(cx);
            reader_ui_state::install(temp.path(), cx);
            assert_eq!(reader_ui_state::text_zoom(cx), 1.4);
            assert_eq!(reader_ui_state::base_font_size(cx), 20.);
            assert_eq!(reader_ui_state::reading_width(cx), 960.);
            reader_ui_state::set_text_zoom(f32::NAN, cx);
            assert_eq!(reader_ui_state::text_zoom(cx), 1.4);
            reader_ui_state::set_text_zoom(9., cx);
            assert_eq!(reader_ui_state::text_zoom(cx), 3.);
            reader_ui_state::set_text_zoom(1., cx);
            assert_eq!(reader_ui_state::font_size(cx), 20.);
        });
        assert_eq!(wheel_delta(ScrollDelta::Lines(point(0., 1.))), Some(0.1));
        assert_eq!(wheel_delta(ScrollDelta::Lines(point(0., 3.))), Some(0.1));
        assert_eq!(wheel_delta(ScrollDelta::Lines(point(0., 0.))), None);
        assert_eq!(
            wheel_delta(ScrollDelta::Pixels(point(px(0.), px(1000.)))),
            Some(0.1)
        );
        assert_eq!(
            wheel_delta(ScrollDelta::Pixels(point(px(0.), px(4.)))),
            Some(0.01)
        );
        assert!(modifier(
            gpui::Modifiers {
                platform: true,
                ..Default::default()
            },
            true
        ));
        assert!(!modifier(
            gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            true
        ));
        assert!(modifier(
            gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            false
        ));
        assert!(!modifier(
            gpui::Modifiers {
                platform: true,
                ..Default::default()
            },
            false
        ));
    }
}
