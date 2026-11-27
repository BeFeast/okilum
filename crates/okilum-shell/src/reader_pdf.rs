//! Reader glue for inline PDF viewer (#477, #3238).
use super::*;
pub(crate) use okilum_shell_viewers::reader_pdf::*;

impl Reader {
    pub(crate) fn pdf_viewer(&self) -> Option<&Entity<PdfViewer>> {
        self.pane.file_preview.as_ref()?.pdf.as_ref()
    }

    pub(crate) fn pdf_zoom(&mut self, zoom: Zoom, cx: &mut Context<Self>) {
        if let Some(viewer) = self.pdf_viewer().cloned() {
            viewer.update(cx, |viewer, cx| viewer.zoom(zoom, cx));
        }
    }
}
