//! On-demand browsing in the existing remote window, without a local vault.
use super::{view::Panel, *};
use crate::*;
use okilum_core::remote_markdown::folder::{self, Folder, Kind, Listing};

pub(super) type Lister =
    Arc<dyn Fn(&Folder, &Arc<AtomicBool>) -> io::Result<Listing> + Send + Sync>;
pub(super) struct Browser {
    pub(super) folder: Folder,
    pub(super) result: Option<io::Result<Listing>>,
}
impl Panel {
    pub(super) fn revoke_folder(&mut self) {
        self.folder_serial = self
            .folder_serial
            .checked_add(1)
            .expect("remote folder generation exhausted");
        if let Some(cancel) = self.folder_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.folder_task = None;
    }
    pub(super) fn browse(&mut self, folder: Folder, cx: &mut Context<Self>) {
        if !self.stop(cx) {
            return;
        }
        // stop protects any local editor and revokes in-flight read/save/list
        // publication before the new folder gains UI authority.
        self.edit = None;
        self.content = None;
        self.prepared = None;
        self.requested_source = None;
        let serial = self.folder_serial;
        self.browser = Some(Browser {
            folder: folder.clone(),
            result: None,
        });
        let cancel = Arc::new(AtomicBool::new(false));
        self.folder_cancel = Some(cancel.clone());
        let loader = self.lister.clone();
        let target = folder.clone();
        let (send, receive) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = loader(&target, &cancel);
            let _ = send.send_blocking(result);
        });
        self.folder_task = Some(cx.spawn(async move |this, cx| {
            if let Ok(result) = receive.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if this.folder_serial != serial
                        || !this.browser.as_ref().is_some_and(|b| b.folder == folder)
                    {
                        return;
                    }
                    this.browser.as_mut().unwrap().result = Some(result);
                    this.folder_cancel = None;
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }
    fn activate_entry(
        &mut self,
        folder: &Folder,
        serial: u64,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if self.folder_serial != serial {
            return;
        }
        let Some(browser) = &self.browser else {
            return;
        };
        if &browser.folder != folder {
            return;
        }
        let Some(Ok(listing)) = &browser.result else {
            return;
        };
        let Some(entry) = listing.entries.get(index) else {
            return;
        };
        if entry.kind == Kind::Directory {
            match folder.child_folder(entry) {
                Ok(next) => self.browse(next, cx),
                Err(error) => {
                    self.notice = Some(error.to_string());
                    cx.notify();
                }
            }
        } else {
            match folder.markdown(entry) {
                Ok(source) => self.open(source, None, cx),
                Err(error) => {
                    self.notice = Some(error.to_string());
                    cx.notify();
                }
            }
        }
    }
    pub(super) fn folder_controls(&self, cx: &Context<Self>) -> AnyElement {
        let folder = self.browser.as_ref().map(|b| b.folder.clone()).or_else(|| {
            self.requested_source
                .as_ref()
                .and_then(|s| Folder::containing(s).ok())
        });
        let mut controls = h_flex().gap_2();
        if let Some(folder) = folder {
            let up = if self.browser.is_some() {
                folder.parent()
            } else {
                Some(folder.clone())
            };
            controls = controls.child(
                Button::new("remote-folder-up")
                    .debug_selector(|| "remote-folder-up".into())
                    .small()
                    .label(if self.browser.is_some() {
                        "Up"
                    } else {
                        "Browse folder"
                    })
                    .disabled(up.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(folder) = &up {
                            this.browse(folder.clone(), cx);
                        }
                    })),
            );
            if self.browser.is_some() {
                controls = controls.child(
                    Button::new("remote-folder-retry")
                        .debug_selector(|| "remote-folder-retry".into())
                        .small()
                        .label("Refresh / Retry")
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.browse(folder.clone(), cx)),
                        ),
                );
            }
        }
        controls.into_any_element()
    }
    pub(super) fn folder_view(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let browser = self.browser.as_ref()?;
        let mut body = v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(div().px_3().child(browser.folder.url().to_string()));
        match &browser.result {
            None => body = body.child(div().px_3().child("Listing remote folder…")),
            Some(Err(error)) => {
                body = body.child(
                    div()
                        .debug_selector(|| "remote-folder-error".into())
                        .px_3()
                        .text_color(brand::palette(cx).danger)
                        .child(format!("{}: {error}", browser.folder.url())),
                )
            }
            Some(Ok(listing)) => {
                body = body.child(div().px_3().text_sm().child(format!(
                    "{} entries · Read-only listing · Markdown files open in this window",
                    listing.entries.len()
                )));
                let mut rows = v_flex()
                    .id("remote-folder-entries")
                    .debug_selector(|| "remote-folder-entries".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll();
                if listing.entries.is_empty() {
                    rows = rows.child(div().px_3().child("This folder is empty"));
                }
                for (index, entry) in listing.entries.iter().enumerate() {
                    let origin = browser.folder.clone();
                    let serial = self.folder_serial;
                    let markdown = entry.is_markdown();
                    let directory = entry.kind == Kind::Directory;
                    let title = format!(
                        "{}{}",
                        if directory {
                            "Folder · "
                        } else if markdown {
                            "Markdown · "
                        } else {
                            ""
                        },
                        entry.label()
                    );
                    let row = div()
                        .id(("remote-folder-entry", index))
                        .debug_selector(move || format!("remote-folder-entry-{index}"))
                        .px_3()
                        .py_2()
                        .when(markdown, |row| row.text_color(cx.theme().primary))
                        .when(directory, |row| row.font_weight(FontWeight::SEMIBOLD))
                        .child(title)
                        .when(directory || markdown, |row| {
                            row.cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.activate_entry(&origin, serial, index, cx)
                                }))
                        });
                    rows = rows.child(row);
                }
                body = body.child(rows);
            }
        }
        Some(body.into_any_element())
    }
}
pub(super) fn default_lister() -> Lister {
    Arc::new(|folder, cancel| {
        // The same flag aborts a connection waiting in an askpass dialog.
        folder::list(folder, &mut OpenSsh::cancellable(cancel.clone()), || {
            cancel.load(Ordering::Acquire)
        })
    })
}
