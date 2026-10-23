//! Smart paste in the note editor (#1611): the paste commands, a drop of
//! files onto the note, and the work a paste leaves for the background
//! (copying files, a large image, a page title, large rich text, the remote
//! images of rich text).
//!
//! Background results go to the paste position, found again after edits
//! ([`Anchor`]); when it cannot be found they go to the caret, with a toast.
//! A file's link is inserted only after the file is complete on disk.
use super::*;
use crate::note_paste::anchor::{Anchor, Region};
use crate::note_paste::download::{self, DownloadError};
use crate::note_paste::files::{self, FileAction, FilePlan};
use crate::note_paste::router::{self, FollowUp, RemoteImage, RichJob, RouteContext, Routed};
use crate::note_paste::{attachments, fetch, title, DroppedFiles};
use gpui_component::input::clipboard::PasteMode;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// A background paste job shows its progress toast after this long.
const PROGRESS_AFTER: Duration = Duration::from_millis(300);

impl Reader {
    /// Paste as Plain Text and Paste as Titled Link, from keys and menus.
    pub(super) fn paste_with_mode(
        &mut self,
        mode: PasteMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.source_input() else {
            cx.propagate();
            return;
        };
        if !input.focus_handle(cx).is_focused(window) {
            cx.propagate();
            return;
        }
        input.update(cx, |state, cx| state.paste_with_mode(mode, window, cx));
    }

    /// Files dropped onto the note: the same router as a paste of their
    /// URLs, at the drop point (the caret when the point has no offset).
    pub(super) fn drop_on_editor(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.source_input() else {
            return;
        };
        let paths = paths.0.to_vec();
        if paths.is_empty() {
            return;
        }
        let plain_file = self
            .file_preview
            .as_ref()
            .is_some_and(|p| reader_delimited::editable(&p.rel));
        let note_rel = (!plain_file).then(|| self.selected_file().to_owned());
        let position = window.mouse_position();
        let (source, offset) = {
            let state = input.read(cx);
            let offset = state
                .source_offset_for_point(position)
                .unwrap_or_else(|| state.cursor());
            (state.value().to_string(), offset)
        };
        let stamp = attachments::stamp_now();
        let context = RouteContext {
            // A plain file takes the paths as text.
            mode: if plain_file {
                router::Mode::Plain
            } else {
                router::Mode::Smart
            },
            source: &source,
            selection: offset..offset,
            vault_root: &self.vault_root,
            note_rel: note_rel.as_deref(),
            stamp: &stamp,
            ask_above: crate::note_paste::ask_above_bytes(cx),
        };
        let started = std::time::Instant::now();
        match router::route(&DroppedFiles(paths), &context) {
            Ok(routed) => {
                eprintln!(
                    "Drop: route={:?} offset={} follow_up={} ms={:.1} {}",
                    routed.route,
                    offset,
                    routed.follow_up.is_some(),
                    started.elapsed().as_secs_f64() * 1000.0,
                    routed.detail
                );
                self.apply_routed(input, routed, offset, window, cx);
            }
            Err(error) => {
                eprintln!("Drop: not inserted ({error:?})");
                reader_toast::error("The dropped files couldn’t be read.", window, cx);
            }
        }
    }

    /// Insert a routed result at `offset` (a drop) and start its follow-up.
    fn apply_routed(
        &mut self,
        input: Entity<EditorState>,
        routed: Routed,
        offset: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(text) = &routed.insert {
            input.update(cx, |state, cx| {
                state.replace_source_range(offset..offset, text, window, cx);
            });
        }
        if let Some(notice) = routed.notice {
            show_notice(&notice, window, cx);
        }
        if let Some(follow_up) = routed.follow_up {
            self.run_paste_follow_up(input, follow_up, window, cx);
        }
    }

    /// Start the background part of a paste.
    pub(super) fn run_paste_follow_up(
        &mut self,
        input: Entity<EditorState>,
        follow_up: FollowUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match follow_up {
            FollowUp::Files { plan, anchor } => {
                if plan.ask {
                    ask_then_copy(input, plan, anchor, window, cx);
                } else {
                    copy_files(input, plan, anchor, window, cx);
                }
            }
            FollowUp::Title { url, anchor } => fetch_title(input, url, anchor, window, cx),
            FollowUp::Image {
                image,
                vault_root,
                note_rel,
                stamp,
                anchor,
            } => {
                let work = move || {
                    attachments::save_pasted_image(&vault_root, &note_rel, &image, &stamp)
                        .map(|saved| saved.embed)
                        .map_err(|error| error.user_text())
                };
                run_job(input, anchor, "Saving the pasted image…", work, window, cx);
            }
            FollowUp::RemoteImages {
                images,
                region,
                folder,
                stamp,
            } => download_images(input, images, region, folder, stamp, window, cx),
            FollowUp::Rich { job, anchor } => convert_in_background(input, job, anchor, window, cx),
        }
    }
}

pub(super) fn show_notice(notice: &router::Notice, window: &mut Window, cx: &mut App) {
    if notice.error {
        reader_toast::error(notice.text.clone(), window, cx);
    } else {
        reader_toast::transient(notice.text.clone(), window, cx);
    }
}

/// Above the size threshold: Copy into vault, Link to original file, or
/// Cancel.
fn ask_then_copy(
    input: Entity<EditorState>,
    plan: FilePlan,
    anchor: Anchor,
    window: &mut Window,
    cx: &mut Context<Reader>,
) {
    let largest = plan
        .items
        .iter()
        .filter(|item| matches!(item.action, FileAction::Copy { .. }))
        .max_by_key(|item| item.size);
    let message = match (plan.copies(), largest) {
        (1, Some(item)) => format!(
            "Copy “{}” ({}) into the vault?",
            item.source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            files::size_text(item.size)
        ),
        (count, _) => format!(
            "Copy {count} files ({}) into the vault?",
            files::size_text(plan.copy_bytes)
        ),
    };
    let detail = "A copy goes into the attachment folder and syncs with the vault. \
                  A link to the original file works only on this computer.";
    let answer = window.prompt(
        PromptLevel::Info,
        &message,
        Some(detail),
        &["Copy into Vault", "Link to Original File", "Cancel"],
        cx,
    );
    cx.spawn_in(window, async move |_, cx| {
        let choice = answer.await.ok();
        let _ = cx.update(|window, cx| match choice {
            Some(0) => copy_files(input, plan, anchor, window, cx),
            Some(1) => {
                let text = plan
                    .items
                    .iter()
                    .map(|item| match &item.action {
                        FileAction::Link { text } => text.clone(),
                        FileAction::Copy { .. } => files::original_link(&item.source),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let block = plan.items.len() > 1;
                insert_at_anchor(&input, &anchor, &text, block, false, window, cx);
            }
            _ => eprintln!("Paste: file copy cancelled"),
        });
    })
    .detach();
}

/// Copy the plan's files in the background, then insert every link at once.
fn copy_files(
    input: Entity<EditorState>,
    plan: FilePlan,
    anchor: Anchor,
    window: &mut Window,
    cx: &mut App,
) {
    let total = plan.copy_bytes.max(1);
    let done = Arc::new(AtomicU64::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let label = if plan.copies() == 1 {
        "Copying the file into the vault".to_string()
    } else {
        format!("Copying {} files into the vault", plan.copies())
    };
    progress_toast(label, done.clone(), total, finished.clone(), window, cx);
    let started = std::time::Instant::now();
    let work_plan = plan.clone();
    let progress = done.clone();
    let task = cx.background_executor().spawn(async move {
        let mut copied = Vec::new();
        let mut errors = Vec::new();
        let mut base = 0u64;
        for item in &work_plan.items {
            let FileAction::Copy { file_name } = &item.action else {
                continue;
            };
            let result = files::copy_into(&item.source, &work_plan.folder, file_name, &mut |n| {
                progress.store(base + n, Ordering::Relaxed)
            });
            base += item.size;
            progress.store(base, Ordering::Relaxed);
            match result {
                Ok(name) => copied.push(Some(name)),
                Err(error) => {
                    errors.push(error.user_text(file_name));
                    copied.push(None);
                }
            }
        }
        (copied, errors)
    });
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let (copied, errors) = task.await;
        finished.store(true, Ordering::Relaxed);
        eprintln!(
            "Paste: copied {} of {} files, {} bytes, ms={}",
            copied.iter().filter(|c| c.is_some()).count(),
            copied.len(),
            plan.copy_bytes,
            started.elapsed().as_millis()
        );
        let _ = cx.update_window(handle, |_, window, cx| {
            let links = files::links(&plan, &copied);
            if !links.is_empty() {
                let block = links.len() > 1;
                insert_at_anchor(&input, &anchor, &links.join("\n"), block, false, window, cx);
            }
            if !errors.is_empty() {
                reader_toast::error(errors.join("\n"), window, cx);
            }
        });
    })
    .detach();
}

/// Run `work` in the background and insert its text at `anchor`.
fn run_job(
    input: Entity<EditorState>,
    anchor: Anchor,
    label: &'static str,
    work: impl FnOnce() -> Result<String, String> + Send + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let finished = Arc::new(AtomicBool::new(false));
    progress_toast(
        label.to_string(),
        Arc::new(AtomicU64::new(0)),
        0,
        finished.clone(),
        window,
        cx,
    );
    let task = cx.background_executor().spawn(async move { work() });
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let result = task.await;
        finished.store(true, Ordering::Relaxed);
        let _ = cx.update_window(handle, |_, window, cx| match result {
            Ok(text) => {
                insert_at_anchor(&input, &anchor, &text, false, false, window, cx);
            }
            Err(text) => reader_toast::error(text, window, cx),
        });
    })
    .detach();
}

/// Paste as Titled Link without a title on the clipboard: the page title,
/// fetched in the background, replaces the bare URL if it is still intact.
fn fetch_title(
    input: Entity<EditorState>,
    url: String,
    anchor: Anchor,
    window: &mut Window,
    cx: &mut App,
) {
    let fetched_url = url.clone();
    let task = cx
        .background_executor()
        .spawn(async move { fetch::page_title(&fetched_url) });
    let handle = window.window_handle();
    let started = std::time::Instant::now();
    cx.spawn(async move |cx| {
        let title = task.await;
        eprintln!(
            "Paste: title fetch {} ms={}",
            if title.is_some() { "found" } else { "failed" },
            started.elapsed().as_millis()
        );
        let Some(title) = title else {
            return;
        };
        let _ = cx.update_window(handle, |_, window, cx| {
            input.update(cx, |state, cx| {
                let current = state.value().to_string();
                if let Some(at) = anchor.locate_text(&current, &url) {
                    let link = title::markdown_link(&title, &url);
                    state.replace_source_range(at..at + url.len(), &link, window, cx);
                }
            });
        });
    })
    .detach();
}

/// Insert finished text where the paste was; at the caret, with a toast,
/// when that place can no longer be found. `markdown`: converted Markdown,
/// fitted like a paste of it (blank lines around a block). The region of the
/// inserted text, if it went in.
fn insert_at_anchor(
    input: &Entity<EditorState>,
    anchor: &Anchor,
    text: &str,
    block: bool,
    markdown: bool,
    window: &mut Window,
    cx: &mut App,
) -> Option<Region> {
    let (moved, region) = input.update(cx, |state, cx| {
        let current = state.value().to_string();
        let (at, moved) = match anchor.locate(&current) {
            Some(at) => (at, false),
            None => (state.cursor(), true),
        };
        let fitted = if markdown {
            crate::note_paste::context::fit_markdown(text, block, &current, at..at)
        } else {
            crate::note_paste::context::fit(text, block, &current, at..at)
        };
        let inserted = state.replace_source_range(at..at, &fitted, window, cx);
        if !inserted {
            eprintln!("Paste: background result not inserted (the editor is read-only)");
        }
        let region = inserted.then(|| Region::at(&state.value().to_string(), at, fitted.len()));
        (moved && inserted, region)
    });
    if moved {
        reader_toast::transient(
            "The pasted text went to the caret: the place it was pasted at changed.",
            window,
            cx,
        );
    }
    region
}

/// Rich text too large for the UI thread (#1611): converted (and its
/// `data:`/`file://` images saved) in the background, then inserted at the
/// paste position; its remote images are downloaded after that.
fn convert_in_background(
    input: Entity<EditorState>,
    job: RichJob,
    anchor: Anchor,
    window: &mut Window,
    cx: &mut App,
) {
    let finished = Arc::new(AtomicBool::new(false));
    progress_toast(
        "Converting the pasted text".to_string(),
        Arc::new(AtomicU64::new(0)),
        0,
        finished.clone(),
        window,
        cx,
    );
    let folder = attachments::attachment_folder(&job.vault_root, &job.note_rel);
    let stamp = job.stamp.clone();
    let html_bytes = job.html.len();
    let started = std::time::Instant::now();
    let task = cx
        .background_executor()
        .spawn(async move { router::run_rich_job(job) });
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let done = task.await;
        finished.store(true, Ordering::Relaxed);
        eprintln!(
            "Paste: background rich text bytes={html_bytes} inserted={} convert_ms={:.1} \
             images_saved={} images_failed={} images_remote={} ms={}",
            done.text.len(),
            done.convert_ms,
            done.saved,
            done.failed,
            done.remote.len(),
            started.elapsed().as_millis()
        );
        let _ = cx.update_window(handle, |_, window, cx| {
            if done.text.is_empty() {
                return;
            }
            let region = insert_at_anchor(
                &input,
                &anchor,
                &done.text,
                done.block,
                done.markdown,
                window,
                cx,
            );
            if done.failed > 0 {
                reader_toast::transient(router::failed_images_text(done.failed), window, cx);
            }
            if let Some(region) = region {
                if !done.remote.is_empty() {
                    download_images(input, done.remote, region, folder, stamp, window, cx);
                }
            }
        });
    })
    .detach();
}

/// Download the remote images of pasted rich text (#1611), at most
/// [`download::PARALLEL`] at a time, and replace each one's `![alt](url)`
/// inside the pasted text with the saved file's embed as it arrives. Text the
/// user changed is left alone (the remote image stays), and so is a failed
/// download; a file whose text is gone is removed again.
fn download_images(
    input: Entity<EditorState>,
    images: Vec<RemoteImage>,
    region: Region,
    folder: PathBuf,
    stamp: String,
    window: &mut Window,
    cx: &mut App,
) {
    let total = images.len();
    if total == 0 {
        return;
    }
    let done = Arc::new(AtomicU64::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let label = if total == 1 {
        "Downloading the pasted image".to_string()
    } else {
        format!("Downloading {total} pasted images")
    };
    progress_toast(
        label,
        done.clone(),
        total as u64,
        finished.clone(),
        window,
        cx,
    );
    let images = Arc::new(images);
    let next = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = async_channel::unbounded::<(usize, Result<String, DownloadError>)>();
    for _ in 0..total.min(download::PARALLEL) {
        let images = images.clone();
        let next = next.clone();
        let sender = sender.clone();
        let folder = folder.clone();
        let stamp = stamp.clone();
        cx.background_executor()
            .spawn(async move {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(image) = images.get(index) else {
                        break;
                    };
                    let result = download::save_remote_image(&image.url, &folder, &stamp);
                    if sender.send((index, result)).await.is_err() {
                        break;
                    }
                }
            })
            .detach();
    }
    drop(sender);
    let handle = window.window_handle();
    let started = std::time::Instant::now();
    cx.spawn(async move |cx| {
        let mut region = region;
        let (mut saved, mut failed, mut replaced) = (0usize, 0usize, 0usize);
        while let Ok((index, result)) = receiver.recv().await {
            done.fetch_add(1, Ordering::Relaxed);
            let image = &images[index];
            let name = match result {
                Ok(name) => name,
                Err(error) => {
                    failed += 1;
                    eprintln!("Paste: image download failed ({error})");
                    continue;
                }
            };
            saved += 1;
            let embed = files::wikilink(&name);
            let hits = cx
                .update_window(handle, |_, window, cx| {
                    input.update(cx, |state, cx| {
                        let before = state.value().to_string();
                        let hits = region.occurrences(&before, &image.placeholder);
                        for hit in hits.iter().rev() {
                            state.replace_source_range(hit.clone(), &embed, window, cx);
                        }
                        if !hits.is_empty() {
                            let after = state.value().to_string();
                            region = region.after_replacing(&before, &hits, &embed, &after);
                        }
                        hits.len()
                    })
                })
                .unwrap_or(0);
            if hits == 0 {
                // The remote image was edited away: no file without a link.
                let _ = std::fs::remove_file(folder.join(&name));
            }
            replaced += hits;
        }
        finished.store(true, Ordering::Relaxed);
        eprintln!(
            "Paste: images downloaded={saved} failed={failed} replaced={replaced} ms={}",
            started.elapsed().as_millis()
        );
    })
    .detach();
}

/// A toast with progress for background work that takes longer than
/// [`PROGRESS_AFTER`]; removed when `finished` is set. `total == 0`: no
/// percentage.
fn progress_toast(
    label: String,
    done: Arc<AtomicU64>,
    total: u64,
    finished: Arc<AtomicBool>,
    window: &mut Window,
    cx: &mut App,
) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(PROGRESS_AFTER).await;
        if finished.load(Ordering::Relaxed) {
            return;
        }
        let shown_done = done.clone();
        let key = cx.update_window(handle, |_, window, cx| {
            reader_toast::push(
                gpui_component::notification::Notification::new().content(move |_, _, _| {
                    let text = if total > 0 {
                        let percent = shown_done.load(Ordering::Relaxed).min(total) * 100 / total;
                        format!("{label}… {percent}%")
                    } else {
                        format!("{label}…")
                    };
                    div()
                        .debug_selector(|| "paste-progress-toast".into())
                        .text_sm()
                        .child(text)
                        .into_any_element()
                }),
                None,
                window,
                cx,
            )
        });
        let Ok(key) = key else {
            return;
        };
        while !finished.load(Ordering::Relaxed) {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            if cx
                .update_window(handle, |_, window, _| window.refresh())
                .is_err()
            {
                return;
            }
        }
        let _ = cx.update_window(handle, |_, window, cx| {
            reader_toast::remove(key, window, cx)
        });
    })
    .detach();
}
