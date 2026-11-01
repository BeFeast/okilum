//! Bounded, selection-independent Recent previews. Inventory remains caller-owned.
use super::thumb_markdown;
use super::*;
use std::collections::HashMap;
use std::io::Read;

const MAX_CARDS: usize = 40;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const TEXT_BYTES: u64 = 4096;

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
    Code(String),
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

struct Work {
    generation: u64,
    jobs: Vec<(String, Option<Cached>)>,
}

pub(crate) struct Gallery {
    root: PathBuf,
    entries: Vec<reader_title_nav::Entry>,
    previews: Vec<Preview>,
    cache: HashMap<String, Cached>,
    code_states: HashMap<String, Entity<TextViewState>>,
    markdown_keys: HashMap<String, (thumb_markdown::Key, u64)>,
    layout_epoch: u64,
    query: String,
    selected: Option<String>,
    generation: u64,
    theme_revision: u64,
    requests: async_channel::Sender<Work>,
    pending: async_channel::Receiver<Work>,
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
            previews: Vec::new(),
            entries: Vec::new(),
            cache: HashMap::new(),
            code_states: HashMap::new(),
            markdown_keys: HashMap::new(),
            layout_epoch: 0,
            query: String::new(),
            generation: 0,
            theme_revision: 0,
            requests,
            pending,
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
        self.code_states
            .retain(|path, _| self.entries.iter().any(|entry| &entry.path == path));
        self.markdown_keys
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
                    let root = root.clone();
                    let source_relative = relative.clone();
                    let result = cx
                        .background_executor()
                        .spawn(async move { load_cached(&root, &source_relative, cached) })
                        .await;
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
                                if let Preview::Code(source) = &cached.preview {
                                    let unchanged = this
                                        .cache
                                        .get(&relative)
                                        .is_some_and(|old| old.revision == cached.revision);
                                    if !unchanged || !this.code_states.contains_key(&relative) {
                                        let configured = code_view(TextView::markdown(
                                            "recent-code-prepare",
                                            "",
                                        ));
                                        let state = cx.new(|cx| {
                                            let mut state =
                                                TextViewState::markdown("", cx).selectable(false);
                                            configured.prepare_state(&mut state, cx);
                                            state.set_text(source, cx);
                                            state
                                        });
                                        this.code_states.insert(relative.clone(), state);
                                    }
                                } else {
                                    this.code_states.remove(&relative);
                                }
                                this.previews[index] = cached.preview.clone();
                                this.cache.insert(relative, cached);
                            }
                            Err(error) => {
                                this.cache.remove(&relative);
                                this.code_states.remove(&relative);
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

    pub(crate) fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.query = query;
        let visible = self.visible();
        if !visible
            .iter()
            .any(|&i| Some(&self.entries[i].path) == self.selected.as_ref())
        {
            self.selected = visible.first().map(|&i| self.entries[i].path.clone());
        }
        cx.notify();
    }
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
    /// Parent maps Left/Right to +/-1 and Up/Down to +/-3 in the overlay key context.
    pub(crate) fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = brand::palette(cx);
        let visible = self.visible();
        for &index in &visible {
            if let Preview::Markdown(prepared) = &self.previews[index] {
                let key = prepared.key(224, self.theme_revision, cx);
                let path = &self.entries[index].path;
                if !self
                    .markdown_keys
                    .get(path)
                    .is_some_and(|(old, _)| *old == key)
                {
                    self.layout_epoch = self.layout_epoch.wrapping_add(1);
                    self.markdown_keys
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
                    .max_h(px(640.))
                    .overflow_y_scroll()
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
                            Preview::Code(_) => self
                                .code_states
                                .get(&entry.path)
                                .map(|state| {
                                    code_view(TextView::new(state))
                                        .style(reader_fonts::text_style(cx))
                                        .font_family(reader_fonts::code(cx))
                                        .text_size(px(12.))
                                        .w_full()
                                        .into_any_element()
                                })
                                .unwrap_or_else(|| {
                                    div().child("Loading preview…").into_any_element()
                                }),
                            Preview::Markdown(prepared) => thumb_markdown::render(
                                (
                                    "recent-markdown-thumbnail",
                                    self.markdown_keys[&entry.path].1,
                                ),
                                prepared,
                                224,
                                144,
                                cx,
                            ),
                            Preview::Unavailable(reason) => {
                                div().text_xs().child(reason.clone()).into_any_element()
                            }
                        };
                        v_flex()
                            .id(("recent-gallery-card", index))
                            .w(px(240.))
                            .p_2()
                            .gap_2()
                            .rounded(px(8.))
                            .cursor_pointer()
                            .when(self.selected.as_ref() == Some(&entry.path), |card| {
                                card.bg(palette.selected)
                            })
                            .hover(|card| card.bg(palette.selected))
                            .child(div().w_full().h(px(144.)).overflow_hidden().child(preview))
                            .when(matches!(&self.previews[index], Preview::Markdown(prepared) if prepared.partial), |card| {
                                card.child(div().text_xs().text_color(palette.text_muted).child("Partial preview"))
                            })
                            .child(div().text_sm().truncate().child(entry.label.clone()))
                            .child(div().text_xs().truncate().child(entry.path.clone()))
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

/// Parse code as a fenced Markdown block using the Reader's installed highlighter.
/// No Reader plugins: thumbnails cannot dispatch links, embeds, or diagram jobs.
fn code_view(view: TextView) -> TextView {
    view.code_block_language(reader_code_language::resolver())
        .selectable(false)
        .on_link_click(|_, _, _, _| {})
}

mod text_source {
    /// Ignore only an incomplete final UTF-8 character at the byte-read bound.
    pub(super) fn prefix(bytes: &[u8]) -> Result<&str, std::str::Utf8Error> {
        match std::str::from_utf8(bytes) {
            Ok(text) => Ok(text),
            Err(error) if error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()])
            }
            Err(error) => Err(error),
        }
    }
    pub(super) fn fenced(source: &str, extension: &str) -> String {
        // Source may itself contain fences. A longer fence keeps every byte code.
        let longest = source.split(|c| c != '~').map(str::len).max().unwrap_or(0);
        let fence = "~".repeat(longest.max(2) + 1);
        let language = match extension {
            "js" => "javascript",
            "ts" => "typescript",
            "py" => "python",
            "rs" => "rust",
            "yml" => "yaml",
            "txt" | "log" => "text",
            other => other,
        };
        format!("{fence}{language}\n{source}\n{fence}")
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn bounded_utf8_does_not_corrupt_or_reject_complete_prefix() {
            assert_eq!(prefix(&[b'a', 0xf0, 0x9f]).unwrap(), "a");
            assert_eq!(prefix("café".as_bytes()).unwrap(), "café");
            assert!(prefix(&[b'a', 0xff, b'b']).is_err());
        }
        #[test]
        fn source_cannot_escape_highlighted_code_block() {
            let code = "const text = `~~~\n# not a heading\n~~~~`;";
            let rendered = fenced(code, "js");
            assert!(rendered.starts_with("~~~~~javascript\n"));
            assert!(rendered.ends_with("\n~~~~~"));
            assert!(rendered.contains(code));
        }
    }
}

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
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let preview = if matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
    ) {
        anyhow::ensure!(before.size <= MAX_BYTES, "Image exceeds preview size limit");
        let mut decoder = image::ImageReader::open(&path)?.with_guessed_format()?;
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
    } else if matches!(
        extension.as_str(),
        "txt"
            | "json"
            | "rs"
            | "js"
            | "ts"
            | "py"
            | "toml"
            | "yaml"
            | "yml"
            | "css"
            | "html"
            | "log"
    ) {
        let mut bytes = Vec::new();
        std::fs::File::open(&path)?
            .take(TEXT_BYTES)
            .read_to_end(&mut bytes)?;
        let text = text_source::prefix(&bytes)?;
        let text = text.lines().take(24).collect::<Vec<_>>().join("\n");
        Preview::Code(text_source::fenced(&text, &extension))
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
mod tests {
    use super::*;
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
        let Preview::Code(root) = load(fixture.path(), "note.txt").unwrap() else {
            panic!()
        };
        let Preview::Code(nested) = load(fixture.path(), "nested/note.txt").unwrap() else {
            panic!()
        };
        assert!(root.contains("root text"));
        assert!(nested.contains("nested text"));
        assert!(load(fixture.path(), "missing.md").is_err());
        assert!(load(fixture.path(), "../outside.md").is_err());
    }
}
