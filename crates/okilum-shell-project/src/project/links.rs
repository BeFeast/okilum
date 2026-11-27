//! Chat links use their confirmed project root, independently of the note that
//! happens to be displayed in the Reader. File validation stays off the UI.
use super::feed::content::{LinkHandler, LinkTarget};
use super::*;

#[derive(Clone)]
pub struct OpenFile {
    pub area: PathBuf,
    pub path: PathBuf,
    pub fragment: Option<String>,
}
impl EventEmitter<OpenFile> for ProjectView {}

pub fn local_file(root: &Path, path: &Path) -> anyhow::Result<PathBuf> {
    let root = root.canonicalize()?;
    let path = path.canonicalize()?;
    anyhow::ensure!(
        path.starts_with(&root) && path.is_file(),
        "The file is not available inside this project"
    );
    Ok(path)
}

impl ProjectView {
    pub(super) fn chat_link_handler(parent: WeakEntity<Self>) -> LinkHandler {
        Arc::new(move |target, window, cx| {
            let _ = parent.update(cx, |view, cx| view.open_chat_link(target, window, cx));
        })
    }

    pub fn open_chat_link(
        &mut self,
        target: LinkTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            LinkTarget::External(url) => cx.open_url(url.as_str()),
            LinkTarget::Unavailable { reason, .. } => {
                self.threads.link_notice = Some(reason.to_owned());
                cx.notify();
            }
            LinkTarget::File { path, fragment } => {
                let Some(area) = self.area.clone() else {
                    self.threads.link_notice =
                        Some("A local project root is not confirmed for this conversation.".into());
                    cx.notify();
                    return;
                };
                let root = area.clone();
                let generation = self.generation;
                cx.spawn_in(window, async move |this, cx| {
                    let result = cx.background_executor().spawn(async move { local_file(&root, &path) }).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.generation != generation || this.area.as_ref() != Some(&area) || !this.active {
                            return;
                        }
                        match result {
                            Ok(path) => {
                                this.threads.link_notice = None;
                                cx.emit(OpenFile { area, path, fragment });
                            }
                            Err(_) => this.threads.link_notice = Some("This file is missing, unavailable, or outside the linked local project.".into()),
                        }
                        cx.notify();
                    });
                }).detach();
            }
        }
    }
}
