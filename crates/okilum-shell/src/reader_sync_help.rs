//! Contextual offline Sync Help. No controller, runtime or service dependency.
use crate::*;
use gpui_component::{text::TextView, WindowExt};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const GUIDE: &str = include_str!("../../../docs/help/sync/setup.md");
const FILES: &[(&str, &[u8])] = &[
    (
        "index.html",
        include_bytes!("../../../docs/help/sync/index.html"),
    ),
    (
        "captions.vtt",
        include_bytes!("../../../docs/help/sync/captions.vtt"),
    ),
    (
        "assets/sync-v6.mp4",
        include_bytes!("../../../docs/help/sync/assets/sync-v6.mp4"),
    ),
    (
        "assets/poster-v6.png",
        include_bytes!("../../../docs/help/sync/assets/poster-v6.png"),
    ),
    (
        "assets/sync-settings.png",
        include_bytes!("../../../docs/help/sync/assets/sync-settings.png"),
    ),
    (
        "assets/befeast.svg",
        include_bytes!("../../../docs/help/sync/assets/befeast.svg"),
    ),
];

/// The Settings owner mounts this next to the Sync introduction. Help remains
/// usable offline and does not require enabling the Sync NetworkAccess scope.
pub(crate) fn entry() -> Button {
    Button::new("sync-setup-help")
        .label("Help: connect a computer")
        .ghost()
        .on_click(|_, window, cx| show(window, cx))
}

pub(crate) fn show(window: &mut Window, cx: &mut App) {
    let view = cx.new(|_| SyncHelp {
        busy: false,
        failed: false,
    });
    // Reuse the existing What's new sheet route; no browser or filesystem is
    // needed to read the setup steps. It belongs to the invoking Settings window.
    window.open_sheet(cx, move |sheet, _, _| {
        sheet
            .title("Sync setup help")
            .size(px(560.))
            .resizable(false)
            .child(view.clone())
    });
}

struct SyncHelp {
    busy: bool,
    failed: bool,
}
impl SyncHelp {
    fn open_video(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.failed = false;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let state = reader_history::state_directory()?;
                    let file = publish_local(&state)?;
                    let mut url = url::Url::from_file_path(file)
                        .map_err(|_| anyhow::anyhow!("Local Help path cannot be opened"))?;
                    url.set_fragment(Some("watch"));
                    Ok::<_, anyhow::Error>(url.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(url) => cx.open_url(&url),
                    Err(_) => this.failed = true,
                }
                cx.notify();
            });
        })
        .detach();
    }
}
impl Render for SyncHelp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = brand::palette(cx);
        let mut content = v_flex().id("sync-help-content").size_full().gap_4()
            .overflow_y_scroll().text_color(p.text)
            .child(TextView::markdown("sync-help-written-guide", GUIDE).selectable(true).w_full())
            .child(Button::new("sync-help-copy-guide").label("Copy written guide").ghost()
                .on_click(|_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(GUIDE))))
            .child(Button::new("sync-help-local-video").label(if self.busy { "Preparing local guide…" } else { "Open local video guide" })
                .disabled(self.busy).on_click(cx.listener(|this, _, _, cx| this.open_video(cx))))
            .child(div().text_sm().text_color(p.text_muted)
                .child("Optional: opens a bundled local page in your browser. Existing pair only; no first connection recording. No hosted media is loaded."));
        if self.failed {
            content = content.child(div().id("sync-help-local-error").text_sm()
                .child("Could not open the local video guide. The written steps above remain available; you can copy them."));
        }
        content
    }
}

fn fingerprint() -> String {
    let mut digest = Sha256::new();
    for (name, bytes) in FILES {
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}
fn real_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Help directory replaced"
    );
    Ok(())
}
fn verify_local(root: &Path) -> anyhow::Result<PathBuf> {
    real_directory(root)?;
    real_directory(&root.join("assets"))?;
    for (name, bytes) in FILES {
        let path = root.join(name);
        let metadata = fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() == bytes.len() as u64,
            "Help asset replaced"
        );
        anyhow::ensure!(fs::read(path)? == *bytes, "Help asset changed");
    }
    Ok(root.join("index.html"))
}
/// Publish only immutable bundled assets outside vaults/indexes. Stage a complete
/// generation before making it visible; never overwrite a changed existing copy.
fn publish_local(state: &Path) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(state)?;
    real_directory(state)?;
    let help = state.join("help");
    match fs::create_dir(&help) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    real_directory(&help)?;
    let destination = help.join(format!("sync-{}", fingerprint()));
    match fs::symlink_metadata(&destination) {
        Ok(_) => return verify_local(&destination),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let temporary = tempfile::Builder::new()
        .prefix(".sync-help-")
        .tempdir_in(&help)?;
    fs::create_dir(temporary.path().join("assets"))?;
    for (name, bytes) in FILES {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temporary.path().join(name))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        fs::File::open(temporary.path().join("assets"))?.sync_all()?;
        fs::File::open(temporary.path())?.sync_all()?;
    }
    if let Err(error) = fs::rename(temporary.path(), &destination) {
        // Another Help window may have published this exact generation first.
        // Only a byte-verified copy can win that race.
        if verify_local(&destination).is_err() {
            return Err(error.into());
        }
    }
    #[cfg(unix)]
    fs::File::open(help)?.sync_all()?;
    verify_local(&destination)
}
