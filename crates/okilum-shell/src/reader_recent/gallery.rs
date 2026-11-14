//! Bounded, selection-independent Recent previews. Inventory remains caller-owned.
use super::*;
use super::{thumb_code, thumb_markdown};
use std::collections::HashMap;

const MAX_CARDS: usize = 40;
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Held preview publication: root, relative path and its release and resume signals.
#[cfg(test)]
type PreviewPublicationHold = (
    PathBuf,
    String,
    futures::channel::oneshot::Sender<()>,
    futures::channel::oneshot::Receiver<()>,
);

#[cfg(test)]
thread_local! {
    static PREVIEW_PUBLICATION_HOLD: std::cell::RefCell<Option<PreviewPublicationHold>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
async fn hold_preview_publication(root: &Path, relative: &str) {
    let hold = PREVIEW_PUBLICATION_HOLD.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|(r, p, _, _)| r == root && p == relative)
        {
            slot.take()
        } else {
            None
        }
    });
    if let Some((_, _, reached, release)) = hold {
        let _ = reached.send(());
        let _ = release.await;
    }
}

/// Never resolve a card by its display name, index, or a suffix match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenFile {
    pub root: PathBuf,
    pub relative: String,
}
impl EventEmitter<OpenFile> for Gallery {}

#[derive(Clone)]
enum Preview {
    Loading,
    Image(Arc<RenderImage>),
    Markdown(thumb_markdown::Prepared),
    Code(thumb_code::Prepared),
    Unavailable(String),
}

/// All cache data is derived and bounded to the current inventory. Keep the Gallery
/// entity while its overlay is hidden to reuse previews across openings.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Revision {
    canonical: PathBuf,
    size: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
impl Revision {
    fn read(root: &Path, relative: &str) -> anyhow::Result<Self> {
        let canonical = reader_files::checked_path(root, relative)?;
        let meta = std::fs::metadata(&canonical)?;
        anyhow::ensure!(meta.is_file(), "Not a regular file");
        Ok(Self {
            canonical,
            size: meta.len(),
            modified: meta.modified()?,
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec())
            },
        })
    }
}
#[derive(Clone)]
struct Cached {
    revision: Revision,
    preview: Preview,
}

#[derive(PartialEq, Eq)]
enum PresentationKey {
    Markdown(thumb_markdown::Key),
    Code(thumb_code::Key),
}

struct Work {
    generation: u64,
    jobs: Vec<(String, Option<Cached>)>,
}

/// GPUI dispatches a bound Input action before low-level key capture. Capture
/// those actions on the overlay so the search caret cannot consume card arrows.
pub(super) fn capture_arrow<A: gpui::Action>(
    gallery: Entity<Gallery>,
    key: &'static str,
) -> impl Fn(&A, &mut Window, &mut App) + 'static {
    move |_, window, cx| {
        let columns = super::lifecycle::columns(f32::from(window.viewport_size().width));
        let delta = super::lifecycle::step(key, columns).expect("registered gallery arrow");
        gallery.update(cx, |view, cx| {
            if !view.suspended {
                view.step(delta, cx);
            }
        });
        cx.stop_propagation();
    }
}

pub(crate) struct Gallery {
    root: PathBuf,
    entries: Vec<reader_title_nav::Entry>,
    previews: Vec<Preview>,
    cache: HashMap<String, Cached>,
    presentation_keys: HashMap<String, (PresentationKey, u64)>,
    layout_epoch: u64,
    query: String,
    selected: Option<String>,
    scroll: ScrollHandle,
    generation: u64,
    theme_revision: u64,
    suspended: bool,
    requests: async_channel::Sender<Work>,
    pending: async_channel::Receiver<Work>,
    #[cfg(test)]
    arrow_steps: usize,
    _task: Task<()>,
}
impl Gallery {
    /// Reuse the existing scoped Recent inventory, including its labels and ages.
    /// One worker decodes one card at a time; dropping the gallery cancels the queue.
    pub(crate) fn new(
        root: PathBuf,
        entries: Vec<reader_title_nav::Entry>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (requests, pending) = async_channel::bounded(1);
        let task = Self::worker(root.clone(), pending.clone(), cx);
        let mut this = Self {
            root,
            selected: None,
            scroll: ScrollHandle::new(),
            previews: Vec::new(),
            entries: Vec::new(),
            cache: HashMap::new(),
            presentation_keys: HashMap::new(),
            layout_epoch: 0,
            query: String::new(),
            generation: 0,
            theme_revision: 0,
            suspended: false,
            requests,
            pending,
            #[cfg(test)]
            arrow_steps: 0,
            _task: task,
        };
        this.refresh(entries, &[], cx);
        this
    }

    /// Call after watcher/inventory changes and whenever the overlay is reopened.
    /// Renames must map verified old-inventory paths to final new-inventory paths,
    /// never guessed from labels. Caller composes consecutive rename events.
    /// Absence of such evidence is deletion + addition, consistent with path identity.
    pub(crate) fn refresh(
        &mut self,
        entries: Vec<reader_title_nav::Entry>,
        renames: &[(String, String)],
        cx: &mut Context<Self>,
    ) {
        self.suspended = false;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let old_visible: Vec<_> = self
            .visible()
            .into_iter()
            .map(|i| self.entries[i].path.clone())
            .collect();
        let mut seen = std::collections::HashSet::new();
        self.entries = entries
            .into_iter()
            .filter(|entry| !entry.folder && seen.insert(entry.path.clone()))
            .take(MAX_CARDS)
            .collect();
        let visible: Vec<_> = self
            .visible()
            .into_iter()
            .map(|i| self.entries[i].path.clone())
            .collect();
        self.selected =
            selection::reconcile(self.selected.as_deref(), &old_visible, &visible, renames);
        self.cache
            .retain(|path, _| self.entries.iter().any(|entry| &entry.path == path));
        self.presentation_keys
            .retain(|path, _| self.entries.iter().any(|entry| &entry.path == path));
        // No stale image is presented as current while background revision checks run.
        self.previews = self.entries.iter().map(|_| Preview::Loading).collect();
        let jobs: Vec<_> = self
            .entries
            .iter()
            .map(|entry| (entry.path.clone(), self.cache.get(&entry.path).cloned()))
            .collect();
        // A single long-lived worker and a one-request mailbox coalesce refreshes.
        // Replacing an active CPU decode would allow overlapping cancelled workers.
        while self.pending.try_recv().is_ok() {}
        let _ = self.requests.try_send(Work { generation, jobs });
        cx.notify();
    }

    /// Explicit recovery uses the last accepted scoped inventory. Refresh verifies
    /// revisions off-thread before reusing any image, and invalidates older jobs.
    /// Coalesce with remaining loading cards so Retry cannot strand their queue.
    pub(crate) fn retry(&mut self, relative: &str, cx: &mut Context<Self>) {
        if self.suspended {
            return;
        }
        let Some(index) = self.entries.iter().position(|entry| entry.path == relative) else {
            return;
        };
        if !matches!(self.previews[index], Preview::Unavailable(_)) {
            return;
        }
        self.cache.remove(relative);
        self.presentation_keys.remove(relative);
        self.refresh(self.entries.clone(), &[], cx);
    }

    fn worker(
        root: PathBuf,
        pending: async_channel::Receiver<Work>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            while let Ok(Work { generation, jobs }) = pending.recv().await {
                for (relative, cached) in jobs {
                    if !this
                        .update(cx, |this, _| this.generation == generation)
                        .unwrap_or(false)
                    {
                        break;
                    }
                    let (source_root, source_relative) = locate(&root, &relative);
                    let result = cx
                        .background_executor()
                        .spawn(async move { load_cached(&source_root, &source_relative, cached) })
                        .await;
                    #[cfg(test)]
                    hold_preview_publication(&root, &relative).await;
                    let applied = this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        let Some(index) =
                            this.entries.iter().position(|entry| entry.path == relative)
                        else {
                            return false;
                        };
                        match result {
                            Ok(cached) => {
                                this.previews[index] = cached.preview.clone();
                                this.cache.insert(relative, cached);
                            }
                            Err(error) => {
                                this.cache.remove(&relative);
                                this.previews[index] =
                                    Preview::Unavailable(format!("Preview unavailable: {error}"));
                            }
                        }
                        cx.notify();
                        true
                    });
                    if !matches!(applied, Ok(true)) {
                        break;
                    }
                }
            }
        })
    }

    /// Parent calls on theme/font configuration changes. Source caches survive;
    /// adapter layout keys incorporate the new theme revision and current fonts.
    pub(crate) fn appearance_changed(&mut self, cx: &mut Context<Self>) {
        self.theme_revision = self.theme_revision.wrapping_add(1);
        cx.notify();
    }

    /// Closing retains derived previews but stops queued/obsolete source work.
    pub(crate) fn suspend(&mut self, cx: &mut Context<Self>) {
        self.suspended = true;
        self.generation = self.generation.wrapping_add(1);
        while self.pending.try_recv().is_ok() {}
        cx.notify();
    }

    pub(crate) fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.query = query;
        let visible = self.visible();
        if !visible
            .iter()
            .any(|&i| Some(&self.entries[i].path) == self.selected.as_ref())
        {
            self.selected = visible.first().map(|&i| self.entries[i].path.clone());
        }
        if let Some(index) = visible
            .iter()
            .position(|&i| Some(&self.entries[i].path) == self.selected.as_ref())
        {
            self.scroll.scroll_to_item(index);
        }
        cx.notify();
    }
    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
    pub(crate) fn selected_path(&self) -> Option<&str> {
        self.selected.as_deref()
    }
    fn visible(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.path.to_lowercase().contains(&query)
                    || entry.label.to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect()
    }
    /// Caller maps arrows using the current viewport's number of card columns.
    pub(crate) fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.arrow_steps += 1;
        }
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let current = visible
            .iter()
            .position(|&i| Some(&self.entries[i].path) == self.selected.as_ref())
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(visible.len() as isize) as usize;
        self.selected = Some(self.entries[visible[next]].path.clone());
        self.scroll.scroll_to_item(next);
        cx.notify();
    }
    pub(crate) fn accept(&self, cx: &mut Context<Self>) {
        if let Some(relative) = self.selected.as_ref().filter(|path| {
            self.visible()
                .into_iter()
                .any(|i| &self.entries[i].path == *path)
        }) {
            cx.emit(OpenFile {
                root: self.root.clone(),
                relative: relative.clone(),
            });
        }
    }
}
impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let max_height = (f32::from(window.viewport_size().height) - 224.).clamp(80., 640.);
        let palette = brand::palette(cx);
        let visible = self.visible();
        for &index in &visible {
            let key = match &self.previews[index] {
                Preview::Markdown(prepared) => Some(PresentationKey::Markdown(prepared.key(
                    224,
                    self.theme_revision,
                    cx,
                ))),
                Preview::Code(prepared) => Some(PresentationKey::Code(prepared.key(
                    224,
                    self.theme_revision,
                    cx,
                ))),
                _ => None,
            };
            if let Some(key) = key {
                let path = &self.entries[index].path;
                if !self
                    .presentation_keys
                    .get(path)
                    .is_some_and(|(old, _)| *old == key)
                {
                    self.layout_epoch = self.layout_epoch.wrapping_add(1);
                    self.presentation_keys
                        .insert(path.clone(), (key, self.layout_epoch));
                }
            }
        }
        v_flex()
            .id("recent-gallery")
            .gap_3()
            .p_4()
            .bg(palette.surface)
            .text_color(palette.text)
            .child(div().text_sm().child("Recent files"))
            .when(visible.is_empty(), |view| {
                view.child("No recent files match this search.")
            })
            .child(
                div()
                    .id("recent-gallery-grid")
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .max_h(px(max_height))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(visible.into_iter().map(|index| {
                        let entry = &self.entries[index];
                        let relative = entry.path.clone();
                        let preview = match &self.previews[index] {
                            Preview::Loading => div().child("Loading preview…").into_any_element(),
                            Preview::Image(image) => img(image.clone())
                                .w_full()
                                .h_full()
                                .object_fit(ObjectFit::Contain)
                                .into_any_element(),
                            Preview::Code(prepared) => thumb_code::render(
                                ("recent-code-thumbnail", self.presentation_keys[&entry.path].1),
                                prepared, 224, 144, cx,
                            ),
                            Preview::Markdown(prepared) => thumb_markdown::render(
                                (
                                    "recent-markdown-thumbnail",
                                    self.presentation_keys[&entry.path].1,
                                ),
                                prepared,
                                224,
                                144,
                                cx,
                            ),
                            Preview::Unavailable(reason) => {
                                let retry_path = relative.clone();
                                v_flex().gap_2().child(div().text_xs().child(reason.clone()))
                                    .child(Button::new(("recent-preview-retry", index)).small().label("Retry preview")
                                        .debug_selector(move || format!("recent-preview-retry-{index}"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.retry(&retry_path, cx);
                                        })))
                                    .into_any_element()
                            }
                        };
                        v_flex()
                            .id(("recent-gallery-card", index))
                            .debug_selector(move || format!("recent-gallery-card-{index}"))
                            .w(px(240.))
                            .p_2()
                            .gap_2()
                            .rounded(px(8.))
                            .cursor_pointer()
                            .when(self.selected.as_ref() == Some(&entry.path), |card| {
                                card.bg(palette.selected)
                            })
                            .hover(|card| card.bg(palette.selected))
                            .child(div().debug_selector(move || format!("recent-gallery-preview-{index}"))
                                .w_full().h(px(144.)).overflow_hidden().child(preview))
                            .when(matches!(&self.previews[index], Preview::Markdown(prepared) if prepared.partial), |card| {
                                card.child(div().text_xs().text_color(palette.text_muted).child("Partial preview"))
                            })
                            .child(
                                h_flex()
                                    .gap_2()
                                    .min_w_0()
                                    .child(div().flex_1().min_w_0().text_sm().truncate().child(entry.label.clone()))
                                    .when(entry.unsaved, |row| {
                                        row.child(
                                            div()
                                                .debug_selector(move || format!("recent-gallery-unsaved-{index}"))
                                                .flex_none()
                                                .px_1()
                                                .rounded(px(4.))
                                                .border_1()
                                                .border_color(palette.border)
                                                .text_xs()
                                                .text_color(palette.text_muted)
                                                .child("Unsaved"),
                                        )
                                    }),
                            )
                            .child(div().text_xs().truncate().child(folder_line(entry)))
                            .when_some(entry.age.clone(), |card, age| {
                                card.child(div().text_xs().child(age))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this
                                    .visible()
                                    .into_iter()
                                    .any(|i| this.entries[i].path == relative)
                                {
                                    this.selected = Some(relative.clone());
                                    this.accept(cx);
                                }
                            }))
                    })),
            )
    }
}

/// Where a card's file is read from. Drafts and files opened on their own
/// (#1528) are listed by absolute path: their folder is the root, so the same
/// contained reads apply. Vault cards stay relative to the vault root.
fn locate(root: &Path, path: &str) -> (PathBuf, String) {
    let file = Path::new(path);
    match (file.is_absolute(), file.parent(), file.file_name()) {
        (true, Some(parent), Some(name)) => {
            (parent.to_path_buf(), name.to_string_lossy().into_owned())
        }
        _ => (root.to_path_buf(), path.to_owned()),
    }
}

/// The card's folder line: the vault-relative path, or for an absolute path
/// its folder with the home folder shortened to «~».
fn folder_line(entry: &reader_title_nav::Entry) -> String {
    let file = Path::new(&entry.path);
    if entry.unsaved {
        return "Unsaved draft".into();
    }
    if !file.is_absolute() {
        return entry.path.clone();
    }
    let parent = file.parent().unwrap_or(file);
    match reader_untitled::home()
        .and_then(|home| parent.strip_prefix(home).ok().map(Path::to_path_buf))
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => parent.display().to_string(),
    }
}

#[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
fn load(root: &Path, relative: &str) -> anyhow::Result<Preview> {
    Ok(load_cached(root, relative, None)?.preview)
}

fn load_cached(root: &Path, relative: &str, cached: Option<Cached>) -> anyhow::Result<Cached> {
    let before = Revision::read(root, relative)?;
    if let Some(cached) = cached.filter(|cached| cached.revision == before) {
        anyhow::ensure!(
            before == Revision::read(root, relative)?,
            "File changed while checking preview"
        );
        return Ok(cached);
    }
    let path = &before.canonical;
    let extension = Path::new(relative)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let preview = if matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
    ) {
        anyhow::ensure!(before.size <= MAX_BYTES, "Image exceeds preview size limit");
        let mut decoder = image::ImageReader::open(path)?.with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(64 * 1024 * 1024);
        decoder.limits(limits);
        use image::ImageDecoder as _;
        let mut decoder = decoder.into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut decoded = image::DynamicImage::from_decoder(decoder)?;
        decoded.apply_orientation(orientation);
        let mut image = decoded.thumbnail(480, 288).into_rgba8();
        // GPUI RenderImage frames use BGRA, while the canonical image remains RGBA.
        for pixel in image.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        Preview::Image(Arc::new(RenderImage::new(vec![image::Frame::new(image)])))
    } else if extension == "md" {
        Preview::Markdown(thumb_markdown::load(root, relative)?)
    } else if reader_code_file::eligible(relative) || matches!(extension.as_str(), "txt" | "log") {
        Preview::Code(thumb_code::load(root, relative)?)
    } else {
        Preview::Unavailable("No preview for this file type".into())
    };
    anyhow::ensure!(
        before == Revision::read(root, relative)?,
        "File changed while loading preview"
    );
    Ok(Cached {
        revision: before,
        preview,
    })
}

/// Pure reconciliation is also exercised without starting a native GPUI harness.
mod selection {
    pub(super) fn reconcile(
        selected: Option<&str>,
        old: &[String],
        visible: &[String],
        renames: &[(String, String)],
    ) -> Option<String> {
        let mapped = |path: &str| -> String {
            let mut matches = renames.iter().filter(|(from, _)| from == path);
            match (matches.next(), matches.next()) {
                (Some((_, to)), None)
                    if renames.iter().filter(|(_, target)| target == to).count() == 1 =>
                {
                    to.clone()
                }
                _ => path.to_owned(),
            }
        };
        if let Some(path) = selected {
            let path = mapped(path);
            if visible.contains(&path) {
                return Some(path);
            }
        }
        // Deleting a different card never moves selection. When the selected
        // card is gone, prefer its surviving successor, then predecessor.
        if let Some(index) = selected.and_then(|path| old.iter().position(|p| p == path)) {
            for path in old[index + 1..].iter().chain(old[..index].iter().rev()) {
                let path = mapped(path);
                if visible.contains(&path) {
                    return Some(path);
                }
            }
        }
        visible.first().cloned()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        fn paths(values: &[&str]) -> Vec<String> {
            values.iter().map(|p| (*p).into()).collect()
        }
        #[test]
        fn deletion_preserves_identity_and_uses_nearest_survivor() {
            let old = paths(&["a.md", "nested/a.md", "c.md"]);
            assert_eq!(
                reconcile(
                    Some("nested/a.md"),
                    &old,
                    &paths(&["nested/a.md", "c.md"]),
                    &[]
                )
                .as_deref(),
                Some("nested/a.md")
            );
            assert_eq!(
                reconcile(Some("nested/a.md"), &old, &paths(&["a.md", "c.md"]), &[]).as_deref(),
                Some("c.md")
            );
            assert_eq!(
                reconcile(Some("c.md"), &old, &paths(&["a.md"]), &[]).as_deref(),
                Some("a.md")
            );
            assert_eq!(reconcile(Some("c.md"), &old, &[], &[]), None);
        }
        #[test]
        fn only_unambiguous_explicit_rename_preserves_selected_identity() {
            let old = paths(&["a.md", "nested/a.md"]);
            let visible = paths(&["a.md", "renamed.md"]);
            assert_eq!(
                reconcile(
                    Some("nested/a.md"),
                    &old,
                    &visible,
                    &[("nested/a.md".into(), "renamed.md".into())]
                )
                .as_deref(),
                Some("renamed.md")
            );
            assert_eq!(
                reconcile(Some("nested/a.md"), &old, &visible, &[]).as_deref(),
                Some("a.md")
            );
            assert_eq!(
                reconcile(
                    Some("nested/a.md"),
                    &old,
                    &visible,
                    &[
                        ("nested/a.md".into(), "renamed.md".into()),
                        ("nested/a.md".into(), "a.md".into())
                    ]
                )
                .as_deref(),
                Some("a.md")
            );
        }
    }
}

#[cfg(test)]
mod mounted_tests {
    use super::*;
    use ::core::prelude::v1::test;

    struct Fixture {
        reader: Entity<Reader>,
        root: PathBuf,
        originals: Vec<(String, Vec<u8>)>,
        _directory: tempfile::TempDir,
    }

    fn draw(visual: &mut VisualTestContext) {
        visual.run_until_parked();
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
    }

    fn notes(paths: &[&str]) -> Arc<Vec<okilum_core::vault::Note>> {
        Arc::new(
            paths
                .iter()
                .map(|path| okilum_core::vault::Note {
                    path: (*path).into(),
                    title: (*path).into(),
                })
                .collect(),
        )
    }

    fn mount(cx: &mut TestAppContext, broken: bool) -> (Fixture, &mut VisualTestContext) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        for (path, source) in [
            ("start.md", "# Start"),
            (
                "a.md",
                "# Alpha\n\n**Bold** and [inert](https://example.invalid)\n",
            ),
            ("nested/a.md", "# Nested\n\nDifferent source\n"),
            (
                "code.rs",
                "fn main() { diagnostic_println!(\"**literal**\"); }\r\n",
            ),
        ] {
            std::fs::write(root.join(path), source).unwrap();
        }
        if broken {
            std::fs::write(root.join("picture.png"), b"not an image").unwrap();
        } else {
            image::RgbaImage::from_pixel(12, 12, image::Rgba([7, 8, 9, 255]))
                .save(root.join("picture.png"))
                .unwrap();
        }
        let originals = ["start.md", "a.md", "nested/a.md", "code.rs", "picture.png"]
            .into_iter()
            .map(|p| (p.to_owned(), std::fs::read(root.join(p)).unwrap()))
            .collect();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_fonts::install(cx);
            reader_ui_state::install(&directory.path().join("state"), cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        session_directory: Some(directory.path().join("state")),
                        index_dir: Some(directory.path().join("index")),
                        panel_settings_override: Some(directory.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));
        draw(visual);
        reader.update_in(visual, |reader, window, cx| {
            reader.sidebar.recent = ["a.md", "code.rs", "picture.png", "nested/a.md"]
                .into_iter()
                .map(|p| (p.to_owned(), 0))
                .collect();
            reader.quick_open.inventory = Some(notes(&[
                "start.md",
                "a.md",
                "code.rs",
                "picture.png",
                "nested/a.md",
            ]));
            reader.focus_handle.focus(window, cx);
            cx.notify();
        });
        draw(visual);
        (
            Fixture {
                reader,
                root,
                originals,
                _directory: directory,
            },
            visual,
        )
    }

    fn open(f: &Fixture, visual: &mut VisualTestContext) -> Entity<Gallery> {
        let trigger = visual
            .debug_bounds("title-recent")
            .expect("existing Recent trigger painted");
        visual.simulate_click(trigger.center(), Modifiers::default());
        draw(visual);
        assert!(
            visual.debug_bounds("recent-gallery-overlay").is_some(),
            "real trigger mounted gallery, not the old menu"
        );
        f.reader.update_in(visual, |reader, window, cx| {
            assert!(reader.title_nav.menu.is_none());
            let session = reader.recent_switcher.gallery.as_ref().unwrap();
            assert!(session.input.read(cx).focus_handle(cx).is_focused(window));
            session.view.clone()
        })
    }

    fn unchanged(f: &Fixture) {
        for (p, bytes) in &f.originals {
            assert_eq!(
                &std::fs::read(f.root.join(p)).unwrap(),
                bytes,
                "canonical {p} unchanged"
            );
        }
    }

    #[gpui::test]
    fn recent35_real_trigger_paints_all_adapters_and_opens_exact_duplicate(
        cx: &mut TestAppContext,
    ) {
        let (f, visual) = mount(cx, false);
        let gallery = open(&f, visual);
        gallery.read_with(visual, |g, _| {
            assert!(matches!(g.previews[0], Preview::Markdown(_)));
            assert!(matches!(g.previews[1], Preview::Code(_)));
            assert!(matches!(g.previews[2], Preview::Image(_)));
            assert!(g.presentation_keys.contains_key("a.md"));
            assert!(g.presentation_keys.contains_key("code.rs"));
        });
        for selector in [
            "recent-gallery-preview-0",
            "recent-gallery-preview-1",
            "recent-gallery-preview-2",
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .expect("real adapter preview painted");
            assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
        }
        visual.simulate_input("a.md");
        draw(visual);
        let before = gallery.read_with(visual, |g, _| {
            assert_eq!(g.query, "a.md", "typed search was published");
            let visible = g
                .visible()
                .into_iter()
                .map(|i| g.entries[i].path.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                visible,
                ["a.md", "nested/a.md"],
                "actual accepted order, not label order"
            );
            assert_eq!(
                g.selected_path(),
                Some("a.md"),
                "known initial exact selection"
            );
            g.arrow_steps
        });
        f.reader.update_in(visual, |r, window, cx| {
            assert!(
                r.recent_switcher
                    .gallery
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "search owns keyboard focus before Right"
            );
        });
        visual.simulate_keystrokes("right");
        draw(visual);
        gallery.read_with(visual, |g, _| {
            assert_eq!(
                g.arrow_steps,
                before + 1,
                "one real Right performs exactly one gallery step"
            );
            assert_eq!(g.selected_path(), Some("nested/a.md"))
        });
        visual.simulate_event(ModifiersChangedEvent::default());
        draw(visual);
        f.reader.read_with(visual, |r, _| {
            assert_eq!(
                r.current_rel, "start.md",
                "highlight and Control release are not navigation"
            );
            assert!(r.recent_switcher.gallery_open);
        });
        visual.simulate_keystrokes("enter");
        draw(visual);
        f.reader.read_with(visual, |r, _| {
            assert_eq!(
                r.selected_file(),
                "nested/a.md",
                "exact duplicate identity accepted"
            );
            assert!(!r.recent_switcher.gallery_open);
        });
        assert!(visual.debug_bounds("recent-gallery-overlay").is_none());
        unchanged(&f);
    }

    #[gpui::test]
    fn recent35_retry_button_and_hidden_reply_validate_revision_without_navigation(
        cx: &mut TestAppContext,
    ) {
        let (f, visual) = mount(cx, true);
        let gallery = open(&f, visual);
        gallery.read_with(visual, |g, _| {
            assert!(
                matches!(g.previews[2], Preview::Unavailable(_)),
                "decoder failure positive control"
            );
            assert_eq!(g.selected_path(), Some("a.md"));
        });
        let replacement = f.root.join("replacement.png");
        image::RgbaImage::from_pixel(12, 12, image::Rgba([1, 2, 3, 255]))
            .save(&replacement)
            .unwrap();
        std::fs::rename(replacement, f.root.join("picture.png")).unwrap();
        let (reached, mut waiting) = futures::channel::oneshot::channel();
        let (release, held) = futures::channel::oneshot::channel();
        PREVIEW_PUBLICATION_HOLD.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some((f.root.clone(), "picture.png".into(), reached, held));
        });
        let retry = visual
            .debug_bounds("recent-preview-retry-2")
            .expect("actual unavailable Retry painted");
        visual.simulate_click(retry.center(), Modifiers::default());
        draw(visual);
        assert_eq!(
            waiting.try_recv().unwrap(),
            Some(()),
            "real replacement decode reached publication hold"
        );
        f.reader.read_with(visual, |r, _| {
            assert_eq!(
                r.current_rel, "start.md",
                "Retry must not bubble to card accept"
            )
        });
        visual.simulate_keystrokes("escape");
        draw(visual);
        let generation = gallery.read_with(visual, |g, _| {
            assert!(g.suspended);
            g.generation
        });
        release.send(()).unwrap();
        draw(visual);
        gallery.read_with(visual, |g, _| {
            assert_eq!(g.generation, generation);
            assert!(
                !g.cache.contains_key("picture.png"),
                "obsolete hidden decode was not installed"
            );
        });
        let reopened = open(&f, visual);
        assert_eq!(
            gallery, reopened,
            "one retained Gallery owns cache and worker"
        );
        gallery.read_with(visual, |g, _| {
            let Preview::Image(image) = &g.previews[2] else {
                panic!("reopen must validate replacement bytes")
            };
            assert_eq!(&image.as_bytes(0).unwrap()[..4], &[3, 2, 1, 255]);
            assert_eq!(g.selected_path(), Some("a.md"));
        });
        visual.simulate_keystrokes("escape");
        draw(visual);
        f.reader.update_in(visual, |r, window, cx| {
            assert_eq!(r.current_rel, "start.md");
            assert!(
                r.content.read(cx).focus_handle().is_focused(window),
                "Escape restores document focus"
            );
        });
        for (path, bytes) in &f.originals {
            if path != "picture.png" {
                assert_eq!(&std::fs::read(f.root.join(path)).unwrap(), bytes);
            }
        }
    }

    #[gpui::test]
    fn recent35_inventory_preparation_theme_and_font_preserve_exact_cached_identity(
        cx: &mut TestAppContext,
    ) {
        let (f, visual) = mount(cx, false);
        let gallery = open(&f, visual);
        let (before_key, revision, image) = gallery.read_with(visual, |g, _| {
            let Preview::Image(image) = &g.previews[2] else {
                panic!("decoded image positive control")
            };
            (
                g.presentation_keys["a.md"].1,
                g.cache["a.md"].revision.clone(),
                image.clone(),
            )
        });
        visual.update(|window, cx| {
            let next = if cx.theme().is_dark() {
                gpui_component::ThemeMode::Light
            } else {
                gpui_component::ThemeMode::Dark
            };
            Theme::change(next, Some(window), cx);
        });
        draw(visual);
        let theme_key = gallery.read_with(visual, |g, _| {
            let epoch = g.presentation_keys["a.md"].1;
            assert!(
                epoch > before_key,
                "actual theme change invalidated rendered layout"
            );
            epoch
        });
        visual.update(|_, cx| {
            let next_size = if reader_ui_state::base_font_size(cx) < 22. {
                23.
            } else {
                18.
            };
            reader_ui_state::set_reading(next_size, 800., cx);
            reader_ui_state::set_selected_font(reader_fonts::Role::Note, brand::MONO_FONT, cx);
            assert_eq!(
                reader_ui_state::base_font_size(cx),
                next_size,
                "positive control: real reading preference changed"
            );
            assert_eq!(
                reader_fonts::note(cx).as_ref(),
                brand::MONO_FONT,
                "positive control: requested embedded font resolves"
            );
        });
        draw(visual);
        gallery.read_with(visual, |g, _| {
            assert!(
                g.presentation_keys["a.md"].1 > theme_key,
                "font updates independently invalidated layout identity"
            );
            assert_eq!(
                g.cache["a.md"].revision, revision,
                "appearance is not a source revision"
            );
            let Preview::Image(after) = &g.previews[2] else {
                panic!()
            };
            assert!(Arc::ptr_eq(&image, after));
            assert_eq!(g.selected_path(), Some("a.md"));
        });
        f.reader.update(visual, |r, cx| {
            r.quick_open.inventory = None;
            cx.notify();
        });
        draw(visual);
        gallery.read_with(visual, |g, _| {
            assert!(g.suspended, "missing inventory is preparation");
            assert_eq!(g.selected_path(), Some("a.md"), "not evidence of deletion");
        });
        f.reader.update(visual, |r, cx| {
            r.quick_open.inventory = Some(notes(&["code.rs", "picture.png", "nested/a.md"]));
            cx.notify();
        });
        draw(visual);
        gallery.read_with(visual, |g, _| {
            assert!(!g.suspended);
            assert_eq!(
                g.selected_path(),
                Some("code.rs"),
                "accepted deletion falls to next surviving exact identity"
            );
            assert!(!g.cache.contains_key("a.md"));
        });
        unchanged(&f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[gpui::test]
    fn retry_recovers_replaced_unreadable_image_without_changing_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::test_init);
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("broken.png");
        std::fs::write(&path, b"invalid image").unwrap();
        let entries = vec![reader_title_nav::Entry {
            path: "broken.png".into(),
            label: "broken".into(),
            location: None,
            age: None,
            folder: false,
            unsaved: false,
        }];
        let gallery = cx.new(|cx| Gallery::new(fixture.path().into(), entries, cx));
        cx.run_until_parked();
        gallery.read_with(cx, |view, _| {
            assert!(
                matches!(view.previews[0], Preview::Unavailable(_)),
                "positive control: real decoder rejected invalid bytes"
            );
            assert_eq!(view.selected_path(), Some("broken.png"));
        });
        let replacement = fixture.path().join("replacement.png");
        image::RgbaImage::from_pixel(20, 20, image::Rgba([7, 8, 9, 255]))
            .save(&replacement)
            .unwrap();
        std::fs::rename(replacement, &path).unwrap();
        gallery.update(cx, |view, cx| view.retry("broken.png", cx));
        cx.run_until_parked();
        gallery.read_with(cx, |view, _| {
            let Preview::Image(image) = &view.previews[0] else {
                panic!("Retry must decode the replacement")
            };
            assert_eq!(&image.as_bytes(0).unwrap()[..4], &[9, 8, 7, 255]);
            assert_eq!(view.selected_path(), Some("broken.png"));
        });
        gallery.update(cx, |view, cx| {
            view.previews[0] = Preview::Unavailable("fixture failure after recovery".into());
            view.suspend(cx);
            let generation = view.generation;
            view.retry("broken.png", cx);
            assert_eq!(view.generation, generation, "closed galleries reject Retry");
        });
    }

    #[test]
    fn cache_reuses_image_and_invalidates_same_path_replacement() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("image.png");
        image::RgbaImage::from_pixel(20, 20, image::Rgba([1, 2, 3, 255]))
            .save(&path)
            .unwrap();
        let first = load_cached(fixture.path(), "image.png", None).unwrap();
        let second = load_cached(fixture.path(), "image.png", Some(first.clone())).unwrap();
        let (Preview::Image(a), Preview::Image(b)) = (&first.preview, &second.preview) else {
            panic!()
        };
        assert!(
            Arc::ptr_eq(a, b),
            "positive control: unchanged image reused its decoded frame"
        );
        assert_eq!(&a.as_bytes(0).unwrap()[..4], &[3, 2, 1, 255]);
        let replacement = fixture.path().join("replacement.png");
        image::RgbaImage::from_pixel(20, 20, image::Rgba([4, 5, 6, 255]))
            .save(&replacement)
            .unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let third = load_cached(fixture.path(), "image.png", Some(second)).unwrap();
        assert_ne!(first.revision, third.revision);
        let Preview::Image(c) = &third.preview else {
            panic!()
        };
        assert!(
            !Arc::ptr_eq(a, c),
            "replacement must not reuse the old bitmap"
        );
        std::fs::remove_file(path).unwrap();
        assert!(load_cached(fixture.path(), "image.png", Some(third)).is_err());
    }

    #[test]
    fn real_image_is_bounded_and_duplicate_names_keep_exact_identity() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("nested")).unwrap();
        image::RgbaImage::new(960, 576)
            .save(fixture.path().join("image.png"))
            .unwrap();
        let Preview::Image(thumbnail) = load(fixture.path(), "image.png").unwrap() else {
            panic!("real image must produce a bitmap")
        };
        assert_eq!(thumbnail.size(0).width.0, 480);
        assert_eq!(thumbnail.size(0).height.0, 288);
        std::fs::write(fixture.path().join("note.txt"), "root text").unwrap();
        std::fs::write(fixture.path().join("nested/note.txt"), "nested text").unwrap();
        let root = load_cached(fixture.path(), "note.txt", None).unwrap();
        let nested = load_cached(fixture.path(), "nested/note.txt", None).unwrap();
        assert!(matches!(root.preview, Preview::Code(_)));
        assert!(matches!(nested.preview, Preview::Code(_)));
        assert_eq!(
            root.revision.canonical,
            fixture.path().join("note.txt").canonicalize().unwrap()
        );
        assert_eq!(
            nested.revision.canonical,
            fixture
                .path()
                .join("nested/note.txt")
                .canonicalize()
                .unwrap()
        );
        assert_ne!(root.revision, nested.revision);
        assert!(load(fixture.path(), "missing.md").is_err());
        assert!(load(fixture.path(), "../outside.md").is_err());
    }
}
