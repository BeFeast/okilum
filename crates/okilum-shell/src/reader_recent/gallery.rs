//! Bounded, selection-independent Recent previews. Inventory remains caller-owned.
use super::*;
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

enum Preview {
    Loading,
    Image(Arc<RenderImage>),
    Text(String),
    Unavailable(String),
}

pub(crate) struct Gallery {
    root: PathBuf,
    entries: Vec<reader_title_nav::Entry>,
    previews: Vec<Preview>,
    query: String,
    selected: Option<String>,
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
        let mut seen = std::collections::HashSet::new();
        let entries: Vec<_> = entries
            .into_iter()
            .filter(|entry| !entry.folder && seen.insert(entry.path.clone()))
            .take(MAX_CARDS)
            .collect();
        let paths: Vec<_> = entries.iter().map(|entry| entry.path.clone()).collect();
        let source_root = root.clone();
        let task = cx.spawn(async move |this, cx| {
            for (index, relative) in paths.into_iter().enumerate() {
                let root = source_root.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { load(&root, &relative) })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.previews[index] = result.unwrap_or_else(|error| {
                            Preview::Unavailable(format!("Preview unavailable: {error}"))
                        });
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            root,
            selected: entries.first().map(|entry| entry.path.clone()),
            previews: entries.iter().map(|_| Preview::Loading).collect(),
            entries,
            query: String::new(),
            _task: task,
        }
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
        if let Some(relative) = &self.selected {
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
                        let preview = match &self.previews[index] {
                            Preview::Loading => div().child("Loading preview…").into_any_element(),
                            Preview::Image(image) => img(image.clone())
                                .w_full()
                                .h_full()
                                .object_fit(ObjectFit::Contain)
                                .into_any_element(),
                            Preview::Text(text) => div()
                                .text_xs()
                                .overflow_hidden()
                                .child(text.clone())
                                .into_any_element(),
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
                            .child(div().text_sm().truncate().child(entry.label.clone()))
                            .child(div().text_xs().truncate().child(entry.path.clone()))
                            .when_some(entry.age.clone(), |card, age| {
                                card.child(div().text_xs().child(age))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = Some(this.entries[index].path.clone());
                                this.accept(cx);
                            }))
                    })),
            )
    }
}

fn load(root: &Path, relative: &str) -> anyhow::Result<Preview> {
    let path = reader_files::checked_path(root, relative)?;
    let before = std::fs::metadata(&path)?;
    anyhow::ensure!(before.is_file(), "Not a regular file");
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let preview = if matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
    ) {
        anyhow::ensure!(
            before.len() <= MAX_BYTES,
            "Image exceeds preview size limit"
        );
        let mut decoder = image::ImageReader::open(&path)?.with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(64 * 1024 * 1024);
        decoder.limits(limits);
        let image = decoder.decode()?.thumbnail(480, 288).into_rgba8();
        Preview::Image(Arc::new(RenderImage::new(vec![image::Frame::new(image)])))
    } else if matches!(
        extension.as_str(),
        "md" | "txt"
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
        // A bounded source excerpt is an explicit fallback, not a rendered Markdown claim.
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow::anyhow!("Text preview is not UTF-8"))?;
        Preview::Text(format!(
            "Source preview\n{}",
            text.lines().take(10).collect::<Vec<_>>().join("\n")
        ))
    } else {
        Preview::Unavailable("No preview for this file type".into())
    };
    let after = std::fs::metadata(reader_files::checked_path(root, relative)?)?;
    anyhow::ensure!(
        before.len() == after.len() && before.modified()? == after.modified()?,
        "File changed while loading preview"
    );
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_image_is_bounded_and_duplicate_names_keep_exact_identity() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("nested")).unwrap();
        image::RgbaImage::new(960, 576)
            .save(fixture.path().join("image.png"))
            .unwrap();
        assert!(matches!(
            load(fixture.path(), "image.png").unwrap(),
            Preview::Image(_)
        ));
        std::fs::write(fixture.path().join("note.md"), "root text").unwrap();
        std::fs::write(fixture.path().join("nested/note.md"), "nested text").unwrap();
        let Preview::Text(root) = load(fixture.path(), "note.md").unwrap() else {
            panic!()
        };
        let Preview::Text(nested) = load(fixture.path(), "nested/note.md").unwrap() else {
            panic!()
        };
        assert!(root.contains("root text"));
        assert!(nested.contains("nested text"));
        assert!(load(fixture.path(), "missing.md").is_err());
        assert!(load(fixture.path(), "../outside.md").is_err());
    }
}
