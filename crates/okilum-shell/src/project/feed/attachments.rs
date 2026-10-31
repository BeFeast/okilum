//! Structured history references survive text-only and attachment-only rows.
//! Transfers start only on explicit user actions, through the current native lease.
use super::*;
use okilum_t3::stream::{AttachmentKind, ChatAttachment, NativeClient};
use serde_json::Value;

#[derive(Default)]
pub(super) struct History {
    pub(super) rows: HashMap<u64, Vec<Value>>,
    downloads: HashMap<String, Download>,
}
enum Download {
    Loading(Arc<()>),
    Ready(Arc<tempfile::TempDir>, PathBuf, Option<Arc<gpui::Image>>),
    Error,
}
#[derive(Clone, Copy)]
enum Action {
    Preview,
    Open,
    Save,
}

fn filename(name: &str) -> String {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = leaf
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if clean.is_empty() || matches!(clean.as_str(), "." | "..") {
        "attachment".into()
    } else {
        clean
    }
}

impl History {
    pub(super) fn retain(&mut self, position: u64, item: &Value) -> bool {
        let rows = item["attachments"].as_array().cloned().unwrap_or_default();
        if self.rows.get(&position).is_some_and(|old| old == &rows) {
            return false;
        }
        if rows.is_empty() {
            return self.rows.remove(&position).is_some();
        }
        self.rows.insert(position, rows);
        true
    }
}

impl Feed {
    pub(super) fn render_attachments(&self, position: u64, _cx: &mut App) -> AnyElement {
        let mut tray = v_flex().gap_2().w_full();
        for (index, raw) in self
            .attachments
            .rows
            .get(&position)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let attachment = serde_json::from_value::<ChatAttachment>(raw.clone())
                .ok()
                .filter(|a| a.validate().is_ok());
            let Some(attachment) = attachment else {
                tray = tray.child(div().child(format!(
                    "{} · unsupported attachment",
                    raw["name"].as_str().unwrap_or("File")
                )));
                continue;
            };
            let state = self.attachments.downloads.get(&attachment.id);
            let mut row = v_flex().gap_1().child(format!(
                "{} · {} bytes",
                attachment.metadata.name, attachment.metadata.size_bytes
            ));
            if let Some(Download::Ready(_, _, Some(image))) = state {
                row = row.child(
                    gpui::img(image.clone())
                        .max_w_full()
                        .w(px(320.))
                        .h(px(240.))
                        .object_fit(ObjectFit::Contain),
                );
            }
            let mut controls = h_flex().gap_2();
            let image = attachment.metadata.kind == AttachmentKind::Image;
            for (label, action) in if image {
                vec![
                    ("Preview", Action::Preview),
                    ("Open", Action::Open),
                    ("Save…", Action::Save),
                ]
            } else {
                vec![("Open", Action::Open), ("Save…", Action::Save)]
            } {
                let parent = self.parent.clone();
                let attachment = attachment.clone();
                controls = controls.child(
                    Button::new(SharedString::from(format!(
                        "history-attachment-{position}-{index}-{label}"
                    )))
                    .debug_selector(move || {
                        format!("history-attachment-{position}-{index}-{label}")
                    })
                    .small()
                    .ghost()
                    .label(label)
                    .disabled(matches!(state, Some(Download::Loading(_))))
                    .on_click(move |_, window, cx| {
                        let _ = parent.update(cx, |view, cx| {
                            let Some(client) =
                                view.native.model_client().filter(|c| c.access_allowed())
                            else {
                                return;
                            };
                            view.feed.update(cx, |feed, cx| {
                                feed.download_attachment(
                                    attachment.clone(),
                                    action,
                                    client,
                                    window,
                                    cx,
                                )
                            });
                        });
                    }),
                );
            }
            row = row.child(controls);
            if matches!(state, Some(Download::Loading(_))) {
                row = row.child("Downloading…");
            }
            if matches!(state, Some(Download::Error)) {
                row = row.child("Could not download this file. Try again after reconnecting.");
            }
            tray = tray.child(row);
        }
        tray.into_any_element()
    }

    fn download_attachment(
        &mut self,
        attachment: ChatAttachment,
        action: Action,
        client: Arc<NativeClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Download::Ready(directory, path, _)) =
            self.attachments.downloads.get(&attachment.id)
        {
            self.attachment_action(
                directory.clone(),
                path.clone(),
                filename(&attachment.metadata.name),
                action,
                window,
                cx,
            );
            return;
        }
        if matches!(
            self.attachments.downloads.get(&attachment.id),
            Some(Download::Loading(_))
        ) {
            return;
        }
        let lease = Arc::new(());
        self.attachments
            .downloads
            .insert(attachment.id.clone(), Download::Loading(lease.clone()));
        self.attachments_changed(cx);
        cx.spawn_in(window, async move |this, cx| {
            let worker = client.clone();
            let asset = attachment.clone();
            let result = cx.background_executor().spawn(async move {
                let bytes = worker.download_attachment(&asset)?;
                let preview = if asset.metadata.kind == AttachmentKind::Image { super::super::attachments::thumbnail(&bytes) } else { None };
                let directory = Arc::new(tempfile::tempdir()?);
                let path = directory.path().join(filename(&asset.metadata.name));
                std::fs::write(&path, bytes)?;
                Ok::<_, anyhow::Error>((directory, path, preview))
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !matches!(this.attachments.downloads.get(&attachment.id), Some(Download::Loading(current)) if Arc::ptr_eq(current, &lease)) { return; }
                match result {
                    Ok((directory, path, preview)) if client.access_allowed() => {
                        this.attachments.downloads.insert(attachment.id.clone(), Download::Ready(directory.clone(), path.clone(), preview));
                        this.attachment_action(directory, path, filename(&attachment.metadata.name), action, window, cx);
                    }
                    _ => { this.attachments.downloads.insert(attachment.id, Download::Error); }
                }
                this.attachments_changed(cx);
            });
        }).detach();
    }

    fn attachment_action(
        &mut self,
        directory: Arc<tempfile::TempDir>,
        path: PathBuf,
        name: String,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            Action::Preview => {}
            Action::Open => {
                if let Ok(url) = url::Url::from_file_path(&path) {
                    cx.open_url(url.as_str());
                }
            }
            Action::Save => {
                let picker = cx.prompt_for_new_path(&std::env::temp_dir(), Some(&name));
                cx.spawn_in(window, async move |this, cx| {
                    let Ok(Ok(Some(destination))) = picker.await else { return; };
                    let result = cx.background_executor().spawn(async move {
                        let _keep = directory;
                        let parent = destination.parent().context("Missing download directory")?;
                        let mut file = tempfile::NamedTempFile::new_in(parent)?;
                        std::io::copy(&mut std::fs::File::open(path)?, &mut file)?;
                        file.as_file().sync_all()?;
                        file.persist(destination)?;
                        Ok::<_, anyhow::Error>(())
                    }).await;
                    let _ = this.update(cx, |this, cx| {
                        if result.is_err() {
                            // Keep the confirmed download available for another Save.
                            this.parent.update(cx, |view, cx| { view.error = Some("Could not save the attachment. Choose another destination.".into()); cx.notify(); }).ok();
                        }
                    });
                }).detach();
            }
        }
    }

    fn attachments_changed(&mut self, cx: &mut Context<Self>) {
        let indices: Vec<_> = self
            .attachments
            .rows
            .keys()
            .filter_map(|p| self.indices.get(p).copied())
            .collect();
        self.scroller.update(cx, |state, cx| {
            for index in indices {
                state.remeasure_items(index..index + 1, cx);
            }
        });
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn attachment_only_history_and_unknown_types_remain_visible_without_path_escape() {
        let mut history = History::default();
        let item = serde_json::json!({"type":"user_message","text":"","attachments":[{"id":"asset-1","type":"file","name":"../report.txt","mimeType":"text/plain","sizeBytes":4},{"type":"future","name":"future.bin"}]});
        assert!(history.retain(1, &item));
        assert!(!history.retain(1, &item));
        assert_eq!(history.rows[&1].len(), 2);
        let asset: ChatAttachment = serde_json::from_value(history.rows[&1][0].clone()).unwrap();
        assert!(asset.validate().is_ok());
        assert_eq!(filename(&asset.metadata.name), "report.txt");
        assert_eq!(filename("C:\\private\\report.txt"), "report.txt");
        assert_eq!(filename(".."), "attachment");
        assert!(history.retain(1, &serde_json::json!({"attachments":[]})));
        assert!(!history.rows.contains_key(&1));
    }

    #[gpui::test]
    fn save_download_uses_exact_bytes_and_cancel_keeps_existing_files(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let downloaded = Arc::new(tempfile::tempdir().unwrap());
        let source = downloaded.path().join("report.bin");
        let bytes = b"Downloaded binary\0\xff\n";
        std::fs::write(&source, bytes).unwrap();
        let destination = temp.path().join("chosen.bin");
        std::fs::write(&destination, b"Original bytes").unwrap();
        let (root, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = super::super::super::fixture::view(
                    temp.path().to_owned(),
                    Arc::new(super::super::super::fixture::Fake::default()),
                    window,
                    cx,
                );
                view.active = false;
                view
            });
            Root::new(view, window, cx)
        });
        let view = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<ProjectView>().unwrap()
        });
        for save in [false, true] {
            view.update_in(visual, |view, window, cx| {
                view.feed.update(cx, |feed, cx| {
                    feed.attachment_action(
                        downloaded.clone(),
                        source.clone(),
                        "report.bin".into(),
                        Action::Save,
                        window,
                        cx,
                    )
                });
            });
            assert!(visual.did_prompt_for_new_path());
            let chosen = destination.clone();
            visual.simulate_new_path_selection(move |_| save.then_some(chosen));
            visual.run_until_parked();
            assert_eq!(
                std::fs::read(&destination).unwrap().as_slice(),
                if save {
                    bytes.as_slice()
                } else {
                    b"Original bytes".as_slice()
                }
            );
            assert_eq!(
                std::fs::read(&source).unwrap(),
                bytes,
                "the retained download is reusable after Save"
            );
        }
    }
}
