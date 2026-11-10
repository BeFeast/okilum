//! Save the original image bytes, never the clipboard's oriented PNG. The
//! destination belongs to this request; a stale completion cannot publish it.
use super::*;
use anyhow::{Context as _, Result};
use okilum_core::note_files::PreparedCopy;

#[cfg(test)]
thread_local! {
    static HOLD: std::cell::RefCell<Option<futures::channel::oneshot::Receiver<()>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Revision {
    len: u64,
    modified: Option<std::time::SystemTime>,
    created: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
impl Revision {
    fn of(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        }
    }
}
#[derive(Clone)]
enum Original {
    Local {
        path: PathBuf,
        revision: Revision,
        #[cfg(windows)]
        identity: okilum_core::windows_files::Identity,
    },
    Remote(String),
}
impl Original {
    /// Before showing the picker: no bytes are read and no URL is fetched.
    fn capture(target: &Target) -> Result<Self> {
        anyhow::ensure!(
            target.url.is_some() && !target.placeholder,
            "Image is unavailable"
        );
        if let Some(relative) = &target.relative {
            let path = reader_files::checked_path(&target.root, relative)?;
            let metadata = std::fs::metadata(&path)?;
            anyhow::ensure!(metadata.is_file(), "Image file not found");
            #[cfg(target_os = "macos")]
            {
                use std::os::macos::fs::MetadataExt;
                anyhow::ensure!(
                    metadata.st_flags() & 0x4000_0000 == 0,
                    "Download this image from iCloud first"
                );
            }
            anyhow::ensure!(
                metadata.len() <= note_paste::offer::MAX_IMAGE_BYTES as u64,
                "Image exceeds the 64 MB save limit"
            );
            Ok(Self::Local {
                #[cfg(windows)]
                identity: okilum_core::windows_files::checked_identity(&path)?,
                path,
                revision: Revision::of(&metadata),
            })
        } else {
            anyhow::ensure!(target.local.is_none(), "Image is outside this vault");
            let url = target.url.as_ref().unwrap();
            let parsed = url::Url::parse(url)?;
            anyhow::ensure!(
                matches!(parsed.scheme(), "http" | "https"),
                "Unsupported image address"
            );
            Ok(Self::Remote(url.clone()))
        }
    }
    fn validate(&self, target: &Target) -> Result<()> {
        match self {
            Self::Local { path, revision, .. } => {
                #[cfg(windows)]
                if let Self::Local { identity, .. } = self {
                    anyhow::ensure!(
                        okilum_core::windows_files::checked_identity(path)? == *identity,
                        "Image identity changed; reopen its menu"
                    );
                }
                let current = reader_files::checked_path(
                    &target.root,
                    target
                        .relative
                        .as_deref()
                        .context("Image reference changed")?,
                )?;
                anyhow::ensure!(
                    &current == path && Revision::of(&std::fs::metadata(path)?) == *revision,
                    "Image changed; reopen its menu"
                );
            }
            Self::Remote(url) => anyhow::ensure!(
                target.url.as_ref() == Some(url) && target.local.is_none(),
                "Image address changed"
            ),
        }
        Ok(())
    }
    fn bytes(&self, target: &Target) -> Result<Vec<u8>> {
        self.validate(target)?;
        match self {
            Self::Local { path, revision, .. } => {
                let mut file = std::fs::File::open(path)?;
                anyhow::ensure!(
                    Revision::of(&file.metadata()?) == *revision,
                    "Image changed while opening"
                );
                let mut bytes = Vec::new();
                (&mut file)
                    .take(note_paste::offer::MAX_IMAGE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                anyhow::ensure!(
                    bytes.len() <= note_paste::offer::MAX_IMAGE_BYTES,
                    "Image exceeds the 64 MB save limit"
                );
                anyhow::ensure!(
                    Revision::of(&file.metadata()?) == *revision,
                    "Image changed while reading"
                );
                self.validate(target)?;
                Ok(bytes)
            }
            // Only an explicit Save As with a chosen destination gets here.
            // Reuse Copy image's established HTTP limits/redirect/type policy;
            // retain response bytes rather than decode/re-encode them.
            Self::Remote(url) => {
                note_paste::download::fetch_remote_image(url).map_err(|e| anyhow::anyhow!("{e}"))
            }
        }
    }
    fn prepare(&self, target: &Target, path: &Path, drafts: &Path) -> Result<PreparedCopy> {
        // Existing source/destination names (including hard links) are rejected
        // by exclusive publication; there is no Replace path here.
        if let Self::Local { path: source, .. } = self {
            anyhow::ensure!(
                source != path && path.canonicalize().ok().as_ref() != Some(source),
                "Choose a new file, not the original image"
            );
        }
        let bytes = self.bytes(target)?;
        let copy = PreparedCopy::prepare(path, drafts, &bytes)?;
        self.validate(target)?;
        Ok(copy)
    }
}

impl Reader {
    pub(super) fn save_image_as(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if target.accepted_source.is_none() || !target.matches_accepted(self, cx) {
            return;
        }
        let Some(drafts) = self
            .session_directory
            .as_ref()
            .map(|state| state.join("editor-drafts"))
        else {
            reader_toast::error(
                "Cannot save an image without a recovery state directory",
                window,
                cx,
            );
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let work = target.clone();
            let original = cx
                .background_executor()
                .spawn(async move { Original::capture(&work) })
                .await;
            let original = match original {
                Ok(original) => original,
                Err(error) => {
                    let _ = this.update_in_own_window(cx, |_, window, cx| {
                        reader_toast::error(format!("Cannot save image: {error:#}"), window, cx)
                    });
                    return;
                }
            };
            let picker = this.update_in_own_window(cx, |reader, _, cx| {
                if !target.matches_accepted(reader, cx) {
                    return None;
                }
                let (directory, name) = match &original {
                    Original::Local { path, .. } => (
                        path.parent().unwrap_or(&target.root).to_path_buf(),
                        path.file_name().unwrap().to_string_lossy().into_owned(),
                    ),
                    Original::Remote(url) => (
                        target.root.clone(),
                        url::Url::parse(url)
                            .ok()
                            .and_then(|url| {
                                url.path_segments()
                                    .and_then(|mut parts| parts.next_back().map(str::to_owned))
                            })
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| "image".into()),
                    ),
                };
                Some(cx.prompt_for_new_path(&directory, Some(&name)))
            });
            let Ok(Some(picker)) = picker else {
                return;
            };
            let Ok(Ok(Some(path))) = picker.await else {
                return;
            };
            // Check after the picker, before any remote fetch or preparation.
            if !this
                .read_with(cx, |reader, cx| target.matches_accepted(reader, cx))
                .unwrap_or(false)
            {
                return;
            }
            let work = target.clone();
            let source = original.clone();
            let destination = path.clone();
            let staged = cx
                .background_executor()
                .spawn(async move { source.prepare(&work, &destination, &drafts) })
                .await;
            #[cfg(test)]
            if let Some(hold) = HOLD.with(|hold| hold.borrow_mut().take()) {
                let _ = hold.await;
            }
            let _ = this.update_in_own_window(cx, |reader, window, cx| {
                if !target.matches_accepted(reader, cx) {
                    return;
                }
                let result = staged.and_then(|copy| {
                    original.validate(&target)?;
                    // No async gap between the final accepted-source check and
                    // the atomic publish. Full byte IO/fsync ran in background.
                    copy.publish()
                });
                match result {
                    Ok(()) => reader_toast::transient(
                        format!("Image saved to {}", okilum_core::vault::display_path(&path)),
                        window,
                        cx,
                    ),
                    Err(error) => {
                        reader_toast::error(format!("Cannot save image: {error:#}"), window, cx)
                    }
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests;
