//! Optional upload of pasted and dropped files (#1073, Settings → Editor).
//!
//! Runs after a paste or drop has saved its files and inserted their links.
//! Only files the note actually links are uploaded (a "keep original" copy
//! beside a converted image is not). Once the provider confirms an upload,
//! every link the paste wrote to that file is replaced with the uploaded
//! copy's link, in the note's live editor. The local file is never removed;
//! a failed upload, a closed note or an edited link leaves the local link.
use super::*;
use crate::note_upload::{self, UploadError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Start uploading the files a paste or drop just saved, when the setting is
/// on. A no-op otherwise.
pub(super) fn start(
    reader: &Reader,
    paths: &[PathBuf],
    window: &mut Window,
    cx: &mut Context<Reader>,
) {
    if paths.is_empty() || !reader_ui_state::upload_pasted_files(cx) {
        return;
    }
    let Some(editing) = reader.editing.as_ref() else {
        return;
    };
    let input = editing.input.clone();
    let note = editing.store.path().to_path_buf();
    let root = reader.vault_root.clone();
    let paths = paths.to_vec();
    // Spawned: the first poll comes after the paste has inserted its links.
    cx.spawn_in(window, async move |reader, cx| {
        let started = std::time::Instant::now();
        let candidates: Vec<(PathBuf, Vec<String>)> = cx
            .update(|_, cx| {
                let text = input.read(cx).value().to_string();
                paths
                    .iter()
                    .filter_map(|path| {
                        let links = note_upload::local_links(&root, path);
                        let linked = !note_upload::link_ranges(&text, &links).is_empty();
                        linked.then(|| (path.clone(), links))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if candidates.is_empty() {
            return;
        }
        let provider = note_upload::provider();
        let Some(token) = note_upload::token(cx).await else {
            let _ = cx.update(|window, cx| {
                reader_toast::error(failure_text(&UploadError::NoToken, provider), window, cx)
            });
            diagnostic_error!("Upload: skipped, no token saved");
            return;
        };
        let total = candidates.len() as u64;
        let done = Arc::new(AtomicU64::new(0));
        let finished = Arc::new(AtomicBool::new(false));
        let label = if total == 1 {
            format!("Uploading the file to {}", provider.name())
        } else {
            format!("Uploading {total} files to {}", provider.name())
        };
        let _ = cx.update(|window, cx| {
            super::paste_jobs::progress_toast(label, done.clone(), total, finished.clone(), window, cx)
        });
        let (mut uploaded, mut relinked, mut kept) = (0usize, 0usize, 0usize);
        let mut failure = None;
        for (path, links) in candidates {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let upload_token = token.clone();
            let result = cx
                .background_executor()
                .spawn(async move { provider.upload(&path, &upload_token) })
                .await;
            done.fetch_add(1, Ordering::Relaxed);
            let uploaded_copy = match result {
                Ok(uploaded_copy) => uploaded_copy,
                Err(error) => {
                    let stop = matches!(error, UploadError::NoToken | UploadError::Unauthorized);
                    failure = Some(error);
                    if stop {
                        break;
                    }
                    continue;
                }
            };
            uploaded += 1;
            let remote = note_upload::remote_link(&name, &uploaded_copy.url);
            let hits = cx
                .update(|window, cx| {
                    let Some(live) = live_editor(&reader, &input, &note, cx) else {
                        return 0;
                    };
                    live.update(cx, |state, cx| {
                        let text = state.value().to_string();
                        let ranges = note_upload::link_ranges(&text, &links);
                        // Last first: earlier ranges stay valid.
                        for range in &ranges {
                            if !state.replace_source_range(range.clone(), &remote, window, cx) {
                                return 0;
                            }
                        }
                        ranges.len()
                    })
                })
                .unwrap_or(0);
            if hits == 0 {
                kept += 1;
            }
            relinked += hits;
        }
        finished.store(true, Ordering::Relaxed);
        // Counts and time only: never a URL, a path or the token.
        diagnostic_error!(
            "Upload: provider={} files={total} uploaded={uploaded} relinked={relinked} kept_local={kept} failed={} ms={}",
            provider.name(),
            failure.is_some(),
            started.elapsed().as_millis()
        );
        let _ = cx.update(|window, cx| {
            if let Some(error) = &failure {
                reader_toast::error(failure_text(error, provider), window, cx);
            } else if kept > 0 {
                reader_toast::transient(
                    format!(
                        "Uploaded to {}, but the note was closed or its link changed, so it still links the local file.",
                        provider.name()
                    ),
                    window,
                    cx,
                );
            } else if uploaded > 0 {
                reader_toast::transient(
                    format!(
                        "Uploaded to {}. The note links the uploaded copy; the local file stays.",
                        provider.name()
                    ),
                    window,
                    cx,
                );
            }
        });
    })
    .detach();
}

fn failure_text(error: &UploadError, provider: &dyn note_upload::Provider) -> String {
    format!(
        "{} The note keeps the local file.",
        error.user_text(provider.name())
    )
}

/// The editor of the note the paste went into: the same editing session, or
/// the same note opened again. `None` once that note is no longer edited.
fn live_editor(
    reader: &WeakEntity<Reader>,
    input: &Entity<EditorState>,
    note: &std::path::Path,
    cx: &App,
) -> Option<Entity<EditorState>> {
    let reader = reader.upgrade()?;
    let editing = reader.read(cx).editing.as_ref()?;
    (editing.input == *input || editing.store.path() == note).then(|| editing.input.clone())
}
