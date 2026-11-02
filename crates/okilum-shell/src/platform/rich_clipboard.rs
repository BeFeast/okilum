//! Standard native HTML + plain offers, outside GPUI core. The native owner is
//! held for the app lifetime (required by X11). Test writers never open a board.
use anyhow::{ensure, Result};
use gpui::{App, AppContext, ClipboardEntry, ClipboardItem, Global, Window};
use serde::{Deserialize, Serialize};

const MAX_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Payload {
    pub plain: String,
    pub html: String,
}
trait Sink {
    fn write(&mut self, payload: &Payload) -> Result<()>;
}
#[derive(Default)]
struct Native {
    board: Option<arboard::Clipboard>,
}
impl Sink for Native {
    fn write(&mut self, payload: &Payload) -> Result<()> {
        if self.board.is_none() {
            self.board = Some(arboard::Clipboard::new()?);
        }
        self.board
            .as_mut()
            .unwrap()
            .set_html(payload.html.clone(), Some(payload.plain.clone()))?;
        Ok(())
    }
}
struct Writer(Box<dyn Sink>);
impl Global for Writer {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Pending {
    kind: String,
    id: String,
}
fn begin(plain: &str, cx: &mut App) -> Result<Pending> {
    ensure!(
        plain.len() <= MAX_BYTES,
        "Selection exceeds the 8 MiB clipboard limit"
    );
    let pending = Pending {
        kind: "okilum-rich-copy-v1".into(),
        id: uuid::Uuid::new_v4().to_string(),
    };
    // Plain is available immediately. The unique offer marker also cancels a
    // delayed rich write when any later copy replaces this clipboard offer.
    cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(
        plain.into(),
        &pending,
    ));
    Ok(pending)
}
fn still_pending(pending: &Pending, plain: &str, cx: &App) -> bool {
    cx.read_from_clipboard().is_some_and(|item| item.into_entries().any(|entry| {
        matches!(entry, ClipboardEntry::String(text) if text.text == plain && text.metadata_json::<Pending>().as_ref() == Some(pending))
    }))
}
fn finish(pending: &Pending, plain: String, html: Result<String>, cx: &mut App) -> Result<()> {
    if !still_pending(pending, &plain, cx) {
        return Ok(());
    }
    // Consume the marker; native set_html follows this canonical plain write.
    cx.write_to_clipboard(ClipboardItem::new_string(plain.clone()));
    let html = html?;
    ensure!(
        plain.is_empty() || !html.trim().is_empty(),
        "Selection has no formatted content"
    );
    ensure!(
        plain.len().saturating_add(html.len()) <= MAX_BYTES,
        "Formatted clipboard exceeds 8 MiB"
    );
    if !cx.has_global::<Writer>() {
        cx.set_global(Writer(Box::new(Native::default())));
    }
    let result = cx.global_mut::<Writer>().0.write(&Payload {
        plain: plain.clone(),
        html,
    });
    if result.is_err() {
        // A failed native write may have cleared the platform clipboard.
        cx.write_to_clipboard(ClipboardItem::new_string(plain));
    }
    result
}

pub(crate) fn copy(plain: String, markdown: String, window: &mut Window, cx: &mut App) {
    let pending = match begin(&plain, cx) {
        Ok(pending) => pending,
        Err(error) => {
            crate::reader_toast::error(error.to_string(), window, cx);
            return;
        }
    };
    let work = cx
        .background_executor()
        .spawn(async move { okilum_core::render::clipboard::html(&markdown) });
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let html = work.await;
        let _ = cx.update_window(handle, |_, window, cx| {
            if let Err(error) = finish(&pending, plain, html, cx) {
                crate::reader_toast::error(
                    format!("Plain text copied; formatted copy failed: {error}"),
                    window,
                    cx,
                );
            }
        });
    })
    .detach();
}

#[cfg(test)]
pub(crate) fn test_sink(write: impl FnMut(&Payload) -> Result<()> + 'static, cx: &mut App) {
    struct TestSink<F>(F);
    impl<F: FnMut(&Payload) -> Result<()>> Sink for TestSink<F> {
        fn write(&mut self, payload: &Payload) -> Result<()> {
            (self.0)(payload)
        }
    }
    cx.set_global(Writer(Box::new(TestSink(write))));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    #[gpui::test]
    fn later_copy_cancels_rich_completion_even_when_plain_text_matches(
        cx: &mut gpui::TestAppContext,
    ) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            let capture = seen.clone();
            test_sink(
                move |payload| {
                    capture.borrow_mut().push(payload.clone());
                    Ok(())
                },
                cx,
            );
            let request = begin("same", cx).unwrap();
            cx.write_to_clipboard(ClipboardItem::new_string("same".into()));
            finish(&request, "same".into(), Ok("<b>same</b>".into()), cx).unwrap();
            assert!(seen.borrow().is_empty());
            let current = begin("snow 雪", cx).unwrap();
            finish(
                &current,
                "snow 雪".into(),
                Ok("<strong>snow 雪</strong>".into()),
                cx,
            )
            .unwrap();
            assert_eq!(seen.borrow().len(), 1);
            assert_eq!(seen.borrow()[0].plain, "snow 雪");
            assert_eq!(seen.borrow()[0].html, "<strong>snow 雪</strong>");
        });
    }
    #[gpui::test]
    fn native_or_conversion_failure_preserves_plain_without_claiming_rich_success(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            test_sink(|_| anyhow::bail!("mock native refusal"), cx);
            let request = begin("fallback", cx).unwrap();
            assert!(finish(
                &request,
                "fallback".into(),
                Ok("<b>fallback</b>".into()),
                cx
            )
            .is_err());
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("fallback")
            );
            let request = begin("plain", cx).unwrap();
            assert!(finish(
                &request,
                "plain".into(),
                Err(anyhow::anyhow!("conversion refused")),
                cx
            )
            .is_err());
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("plain")
            );
        });
    }
}
