//! Optional upload of pasted and dropped files (#1073, Settings → Editor).
//!
//! Started only after a paste, a drop or a background paste result went
//! into its note's editor, with the exact text it inserted. Each link that
//! text holds to a file the paste saved is remembered by its own place
//! ([`InsertedLink`]); a saved file the text does not link (a "keep
//! original" copy) is not uploaded. Once the provider confirms an upload,
//! only those links are rewritten to the uploaded copy, only while each is
//! still there unchanged, and only in the editor of the note the paste went
//! into ([`Target`]). The local file is never removed.
//!
//! Turning the setting off stops the job: nothing more is sent, and an
//! upload already under way no longer changes the note.
use super::paste_jobs::Target;
use super::*;
use crate::note_upload::{self, InsertedLink, UploadError};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Start uploading the files `paths` that the text at `span` of `note` (the
/// note right after the insert) links, when the setting is on.
pub(super) fn start(
    target: Target,
    note: &str,
    span: Range<usize>,
    paths: &[PathBuf],
    window: &mut Window,
    cx: &mut App,
) {
    if paths.is_empty() || !reader_ui_state::upload_pasted_files(cx) {
        return;
    }
    let links = note_upload::inserted_links(note, span, target.root(), paths);
    if links.is_empty() {
        return;
    }
    let handle = window.window_handle();
    cx.spawn(async move |cx| run(target, links, handle, cx).await)
        .detach();
}

/// The files in upload order, each once, with every link to it.
fn by_file(links: &[InsertedLink]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    for link in links {
        if !files.contains(&link.path) {
            files.push(link.path.clone());
        }
    }
    files
}

fn enabled(cx: &gpui::AsyncApp) -> bool {
    cx.update(|cx| reader_ui_state::upload_pasted_files(cx))
}

async fn run(
    target: Target,
    mut links: Vec<InsertedLink>,
    handle: gpui::AnyWindowHandle,
    cx: &mut gpui::AsyncApp,
) {
    let started = std::time::Instant::now();
    let provider = note_upload::provider();
    let files = by_file(&links);
    let token = note_upload::token(cx).await;
    if !enabled(cx) {
        diagnostic_error!("Upload: cancelled before it started (setting off)");
        return;
    }
    let Some(token) = token else {
        let _ = cx.update_window(handle, |_, window, cx| {
            reader_toast::error(failure_text(&UploadError::NoToken, provider), window, cx)
        });
        diagnostic_error!("Upload: skipped, no token saved");
        return;
    };
    let total = files.len() as u64;
    let done = Arc::new(AtomicU64::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let label = if total == 1 {
        format!("Uploading the file to {}", provider.name())
    } else {
        format!("Uploading {total} files to {}", provider.name())
    };
    let _ = cx.update_window(handle, |_, window, cx| {
        super::paste_jobs::progress_toast(label, done.clone(), total, finished.clone(), window, cx)
    });
    let (mut uploaded, mut relinked, mut kept) = (0usize, 0usize, 0usize);
    let mut failure = None;
    let mut cancelled = false;
    for path in files {
        // Rechecked before every send: off means nothing more leaves.
        if !enabled(cx) {
            cancelled = true;
            break;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let upload_token = token.clone();
        let upload_path = path.clone();
        let result = cx
            .background_executor()
            .spawn(async move { provider.upload(&upload_path, &upload_token) })
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
        if !enabled(cx) {
            // Turned off while this one was under way: the note stays local.
            cancelled = true;
            break;
        }
        let remote = note_upload::remote_link(&name, &uploaded_copy.url);
        let hits = cx
            .update_window(handle, |_, window, cx| {
                relink(&target, &mut links, &path, &remote, window, cx)
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
        "Upload: provider={} files={total} uploaded={uploaded} relinked={relinked} kept_local={kept} failed={} cancelled={cancelled} ms={}",
        provider.name(),
        failure.is_some(),
        started.elapsed().as_millis()
    );
    let _ = cx.update_window(handle, |_, window, cx| {
        if let Some(error) = &failure {
            reader_toast::error(failure_text(error, provider), window, cx);
        } else if cancelled {
            reader_toast::transient(
                "Upload turned off: the note keeps its local links.",
                window,
                cx,
            );
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
}

/// Rewrite the links to `path` that are still intact, in the paste's own
/// note; keep the remaining links' anchors exact. The number rewritten.
fn relink(
    target: &Target,
    links: &mut Vec<InsertedLink>,
    path: &std::path::Path,
    remote: &str,
    window: &mut Window,
    cx: &mut App,
) -> usize {
    let Some(input) = target.live(cx) else {
        return 0;
    };
    let mut hits = 0;
    while let Some(index) = links.iter().position(|link| link.path == path) {
        let link = links.remove(index);
        let rewritten = input.update(cx, |state, cx| {
            let before = state.value().to_string();
            let Some(range) = link.locate(&before) else {
                return false;
            };
            if !state.replace_source_range(range.clone(), remote, window, cx) {
                // Read-only now: nothing changed.
                return false;
            }
            let after = state.value().to_string();
            note_upload::reanchor(links, &before, range, remote.len(), &after);
            true
        });
        hits += usize::from(rewritten);
    }
    hits
}

fn failure_text(error: &UploadError, provider: &dyn note_upload::Provider) -> String {
    format!(
        "{} The note keeps the local file.",
        error.user_text(provider.name())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_file_uploads_once_in_link_order() {
        let anchor = crate::note_paste::anchor::Anchor::at("", 0);
        let link = |path: &str| InsertedLink {
            path: PathBuf::from(path),
            anchor: anchor.clone(),
            link: String::new(),
        };
        let links = vec![link("/v/b.png"), link("/v/a.png"), link("/v/b.png")];
        assert_eq!(
            by_file(&links),
            vec![PathBuf::from("/v/b.png"), PathBuf::from("/v/a.png")]
        );
    }
}
