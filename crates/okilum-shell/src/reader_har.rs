//! Reader glue and Reader-hosted tests for HAR viewer (#2836, #3238).
use super::*;
pub(crate) use okilum_shell_viewers::reader_har::bind_keys;
pub(crate) use okilum_shell_viewers::reader_har::*;

#[cfg(test)]
mod tests;

impl Reader {
    /// Switches the open HAR file between its request list and the JSON
    /// viewer. The JSON viewer is the ordinary code preview, so Find, Copy,
    /// folding and its own views work as for any JSON file.
    pub(crate) fn show_har_source(
        &mut self,
        source: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self.pane.file_preview.as_mut() else {
            return;
        };
        let Some(har) = preview.har.clone() else {
            return;
        };
        if source == preview.code.is_some() {
            return;
        }
        preview.code = if source {
            Some(har.update(cx, |har, cx| har.source_view(window, cx)))
        } else {
            None
        };
        self.focus_handle.focus(window, cx);
        cx.notify();
    }
}
