//! Capture interaction state; the Reader owns the composer and navigation hooks.
use anyhow::{bail, Result};
use okilum_core::capture::{Capture, Receipt};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(crate) struct CaptureComposer {
    pub text: String,
    pub folder: Option<PathBuf>,
    pub visible: bool,
    pub error: Option<String>,
    pub saved: Option<Receipt>,
    pending: Option<Capture>,
}

impl CaptureComposer {
    pub fn show(&mut self) {
        self.visible = true;
    }

    /// Closing the composer never discards authored text.
    pub fn cancel(&mut self) {
        self.visible = false;
    }

    /// Explicit Save only. Run filesystem IO on the Reader's background executor.
    /// On success, render `saved.path` as an Open action (also when navigation fails).
    /// On error, keep the composer open and show `error` alongside the exact draft.
    pub fn save(&mut self, root: &Path, drafts: &Path, records: &Path) -> Result<Receipt> {
        let result = self.publish(root, drafts, records);
        match &result {
            Ok(receipt) => {
                self.saved = Some(receipt.clone());
                self.pending = None;
                self.text.clear();
                self.error = None;
            }
            Err(error) => {
                self.visible = true;
                self.error = Some(format!("{error:#}"));
            }
        }
        result
    }

    fn publish(&mut self, root: &Path, drafts: &Path, records: &Path) -> Result<Receipt> {
        if self.pending.is_some() {
            bail!("An earlier capture needs recovery before another save; your text is retained");
        }
        let folder = self
            .folder
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Choose a capture folder"))?;
        let capture = Capture::new(folder, self.text.clone())?;
        self.pending = Some(capture);
        self.pending
            .as_ref()
            .unwrap()
            .publish(root, drafts, records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_and_validation_error_keep_authored_text() {
        let mut composer = CaptureComposer {
            text: "Unfinished thought\r\n".into(),
            ..Default::default()
        };
        composer.show();
        composer.cancel();
        assert_eq!(composer.text, "Unfinished thought\r\n");
        assert!(!composer.visible);
        // Missing folder fails before any filesystem operation.
        assert!(composer
            .save(
                Path::new("unused"),
                Path::new("unused"),
                Path::new("unused")
            )
            .is_err());
        assert!(composer.visible);
        assert!(composer
            .error
            .as_deref()
            .unwrap()
            .contains("Choose a capture folder"));
        assert_eq!(composer.text, "Unfinished thought\r\n");
    }
}
