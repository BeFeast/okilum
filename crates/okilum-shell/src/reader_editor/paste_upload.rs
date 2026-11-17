//! Optional upload of pasted and dropped files (#1073, Settings → Editor).
//!
//! Started only after a paste, a drop or a background paste result went
//! into its note's editor, with the exact text it inserted. Each link that
//! text holds to a file the paste saved is tracked by its byte range through
//! that editor's own edit receipts ([`TrackedLink`]); an edit that touches a
//! link, or a revision the editor cannot relate, loses it for good. A saved
//! file the text does not link (a "keep original" copy) is not uploaded.
//! Once the provider confirms an upload, only links still tracked, with their
//! exact text, are rewritten to the uploaded copy, and only in the editor the
//! paste went into ([`Target`]). The local file is never removed.
//!
//! Turning the setting off cancels the job for good, even if it is turned on
//! again meanwhile: nothing more is sent, and an upload already under way no
//! longer changes the note.
use super::paste_jobs::Target;
use super::*;
use crate::note_upload::{self, TrackedLink, UploadError};
use std::cell::RefCell;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

type Links = Rc<RefCell<Vec<TrackedLink>>>;

/// Start uploading the files `paths` that the text at `span` of `input` (the
/// note right after the insert) links, when the setting is on.
pub(super) fn start(
    target: Target,
    input: &Entity<EditorState>,
    span: Range<usize>,
    paths: &[PathBuf],
    window: &mut Window,
    cx: &mut App,
) {
    if paths.is_empty() {
        return;
    }
    if let Some(pending) = track(target, input, span, paths, window, cx) {
        pending.start(cx);
    }
}

/// An upload decided when the paste went in, run later: a paste whose remote
/// images are still downloading (#1073). Its links are tracked from the
/// insert on, its upload decision is the one taken at the paste, and the
/// downloads add the links they write ([`Pending::add`]). Dropped without
/// [`Pending::start`], it uploads nothing.
pub(super) struct Pending {
    job: Job,
    watch: gpui::Subscription,
}

/// Begin tracking the links to `paths` in `span` of `input`, when the paste's
/// decision ([`Target::upload_decision`], taken at the paste) still holds.
/// `None` otherwise: nothing of this paste will upload.
pub(super) fn track(
    target: Target,
    input: &Entity<EditorState>,
    span: Range<usize>,
    paths: &[PathBuf],
    window: &mut Window,
    cx: &mut App,
) -> Option<Pending> {
    let decision = target.upload_decision();
    if !reader_ui_state::upload_decision_holds(decision, cx) {
        return None;
    }
    let links: Links = Rc::new(RefCell::new(Vec::new()));
    // Follow every edit of this editor until the job ends.
    let followed = links.clone();
    let watch = cx.observe(input, move |input, cx| follow(&followed, &input, cx));
    let pending = Pending {
        job: Job {
            target,
            input: input.clone(),
            links,
            decision,
            handle: window.window_handle(),
        },
        watch,
    };
    pending.add(span, paths, cx);
    Some(pending)
}

impl Pending {
    /// Track the links to `paths` that the text now at `span` of the paste's
    /// editor holds (a download's embed, written just now).
    pub(super) fn add(&self, span: Range<usize>, paths: &[PathBuf], cx: &App) {
        if paths.is_empty() {
            return;
        }
        follow(&self.job.links, &self.job.input, cx);
        let snapshot = self.job.input.read(cx).source_snapshot();
        let found = note_upload::inserted_links(
            &snapshot.text,
            snapshot.text.id(),
            span,
            self.job.target.root(),
            paths,
        );
        self.job.links.borrow_mut().extend(found);
    }

    /// The editor whose text the paste went into.
    pub(super) fn input(&self) -> &Entity<EditorState> {
        &self.job.input
    }

    /// Upload now. Nothing to do when no tracked link remains.
    pub(super) fn start(self, cx: &mut App) {
        if self.job.links.borrow().is_empty() {
            return;
        }
        let Pending { job, watch } = self;
        cx.spawn(async move |cx| {
            run(job, cx).await;
            drop(watch);
        })
        .detach();
    }
}

struct Job {
    target: Target,
    input: Entity<EditorState>,
    links: Links,
    decision: Option<reader_ui_state::UploadDecision>,
    handle: gpui::AnyWindowHandle,
}

/// Bring every tracked link to the editor's current revision.
fn follow(links: &Links, input: &Entity<EditorState>, cx: &App) {
    let text = input.read(cx).source_snapshot().text;
    let now = text.id();
    for link in links.borrow_mut().iter_mut() {
        let edit = text
            .edit_since(link.revision)
            .map(|edit| (edit.old, edit.inserted));
        link.follow(edit, now);
    }
}

/// The files in upload order, each once.
fn by_file(links: &[TrackedLink]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    for link in links {
        if !files.contains(&link.path) {
            files.push(link.path.clone());
        }
    }
    files
}

/// On at the paste, on now, and never turned off in between.
fn enabled(decision: Option<reader_ui_state::UploadDecision>, cx: &gpui::AsyncApp) -> bool {
    cx.update(|cx| reader_ui_state::upload_decision_holds(decision, cx))
}

async fn run(job: Job, cx: &mut gpui::AsyncApp) {
    let started = std::time::Instant::now();
    let provider = note_upload::provider();
    let files = by_file(&job.links.borrow());
    let handle = job.handle;
    let token = note_upload::token(cx).await;
    if !enabled(job.decision, cx) {
        diagnostic_error!("Upload: cancelled before it started (setting turned off)");
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
        // Checked before every send: off means nothing more leaves.
        if !enabled(job.decision, cx) {
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
        if !enabled(job.decision, cx) {
            // Turned off while this one was under way: the note stays local.
            cancelled = true;
            break;
        }
        let remote = note_upload::remote_link(&name, &uploaded_copy.url);
        let hits = cx
            .update_window(handle, |_, window, cx| {
                relink(&job, &path, &remote, window, cx)
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

/// Rewrite the links to `path` that are still tracked and intact, in the
/// editor the paste went into. The number rewritten.
fn relink(
    job: &Job,
    path: &std::path::Path,
    remote: &str,
    window: &mut Window,
    cx: &mut App,
) -> usize {
    // The same editing session only: a reopened editor has its own history,
    // so the inserted links cannot be told apart from copies there.
    if job.target.live(cx).as_ref() != Some(&job.input) {
        return 0;
    }
    let mut hits = 0;
    loop {
        follow(&job.links, &job.input, cx);
        let next = {
            let mut links = job.links.borrow_mut();
            links
                .iter()
                .position(|link| link.path == path)
                .map(|index| links.remove(index))
        };
        let Some(link) = next else {
            break;
        };
        let rewritten = job.input.update(cx, |state, cx| {
            let text = state.source_snapshot().text;
            let Some(range) = link.current(&text) else {
                return false;
            };
            // Read-only: nothing changed.
            state.replace_source_range(range, remote, window, cx)
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
    use ::core::prelude::v1::test;

    #[test]
    fn each_file_uploads_once_in_link_order() {
        let link = |path: &str| TrackedLink {
            path: PathBuf::from(path),
            link: String::new(),
            range: None,
            revision: 0,
        };
        let links = vec![link("/v/b.png"), link("/v/a.png"), link("/v/b.png")];
        assert_eq!(
            by_file(&links),
            vec![PathBuf::from("/v/b.png"), PathBuf::from("/v/a.png")]
        );
    }
}
