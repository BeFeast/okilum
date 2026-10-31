//! Chat links use their confirmed project root, independently of the note that
//! happens to be displayed in the Reader. File validation stays off the UI.
use super::feed::content::{LinkHandler, LinkTarget};
use super::*;

#[derive(Clone)]
struct OpenFile {
    area: PathBuf,
    path: PathBuf,
    fragment: Option<String>,
}
impl EventEmitter<OpenFile> for ProjectView {}

fn local_file(root: &Path, path: &Path) -> anyhow::Result<PathBuf> {
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

    fn open_chat_link(&mut self, target: LinkTarget, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn forward_chat_links(child: &Entity<Self>, cx: &mut Context<Self>) {
        cx.subscribe(child, |_, _, request: &OpenFile, cx| {
            cx.emit(request.clone())
        })
        .detach();
    }
}

impl Reader {
    pub(super) fn connect_manager_links(
        &mut self,
        manager: &Entity<ProjectView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(
            manager,
            window,
            |this, manager, request: &OpenFile, window, cx| {
                if this.project.as_ref() != Some(&manager)
                    || this.project_navigation.active.as_ref().map(|p| &p.folder)
                        != Some(&request.area)
                {
                    return;
                }
                let target = this.vault_root.canonicalize().ok().and_then(|vault| {
                    request
                        .path
                        .strip_prefix(vault)
                        .ok()
                        .map(okilum_core::vault::note_path)
                });
                let Some(relative) = target else {
                    this.link_notice = Some("This chat file is outside the current vault.".into());
                    cx.notify();
                    return;
                };
                let line = request
                    .fragment
                    .as_deref()
                    .and_then(|s| s.strip_prefix('L'))
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|n| *n > 0);
                this.open_note_landing(
                    &relative,
                    None,
                    if line.is_some() {
                        None
                    } else {
                        request.fragment.as_deref()
                    },
                    line.map(prepared_links::Landing::Line),
                    window,
                    cx,
                );
            },
        )
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn file_validation_requires_a_real_file_under_the_confirmed_project() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Note.md"), "# Note").unwrap();
        std::fs::write(other.path().join("Other.md"), "# Other").unwrap();
        assert_eq!(
            local_file(root.path(), &root.path().join("Note.md")).unwrap(),
            root.path().join("Note.md").canonicalize().unwrap()
        );
        assert!(local_file(root.path(), root.path()).is_err());
        assert!(local_file(root.path(), &root.path().join("missing.md")).is_err());
        assert!(local_file(root.path(), &other.path().join("Other.md")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(other.path().join("Other.md"), root.path().join("Alias.md"))
                .unwrap();
            assert!(local_file(root.path(), &root.path().join("Alias.md")).is_err());
        }
    }
}
