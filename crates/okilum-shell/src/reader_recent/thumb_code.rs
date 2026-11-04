//! Bounded highlighted source cards. Gallery owns the sole worker and cache.
use crate::{reader_code_file, reader_code_language, reader_files, reader_fonts, reader_ui_state};
use gpui::{div, prelude::*, px, AnyElement, App, ElementId};
use gpui_component::{highlighter::LanguageRegistry, text::TextView, ActiveTheme as _};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

// Exact existing bounded UTF-8/BOM codec and escaping fence builder. No new reader.
use crate::reader_plain_text::source;

mod policy {
    use std::path::PathBuf;
    pub(super) const BYTES: usize = 8192;
    pub(super) const LINES: usize = 128;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub(crate) struct Key {
        pub(super) root: PathBuf,
        pub(super) relative: String,
        pub(super) resolved: PathBuf,
        pub(super) revision: [u8; 32],
        pub(super) language: Option<&'static str>,
        pub(super) grammar: Option<[u8; 32]>,
        pub(super) width: u32,
        pub(super) height: u32,
        pub(super) theme_revision: u64,
        pub(super) note_font: String,
        pub(super) code_font: String,
        pub(super) font_size: u32,
    }
    pub(super) fn dimensions(width: u32, height: u32) -> (u32, u32) {
        (width.clamp(1, 1024), height.clamp(1, 1024))
    }
    pub(super) fn prefix(text: &str) -> (&str, bool) {
        let mut end = text.len().min(BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if let Some((offset, _)) = text[..end].match_indices('\n').nth(LINES - 1) {
            end = offset + 1;
        }
        (&text[..end], end < text.len())
    }
    pub(super) fn fallback(language: Option<&str>, grammar: bool) -> Option<&'static str> {
        match (language, grammar) {
            (Some("text" | "plaintext"), _) => Some("Plain text preview"),
            (_, true) => None,
            _ => Some("Syntax highlighting unavailable; showing plain text"),
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn bounded_source_keeps_authored_code_and_utf8() {
            let code = "fn main() {\r\n\tprintln!(\"**not Markdown**\");\r\n}\r\n";
            assert_eq!(prefix(code), (code, false));
            let large = format!("{}Жtail", "a".repeat(BYTES - 1));
            assert_eq!(prefix(&large), (&large[..BYTES - 1], true));
            assert_eq!(prefix(&"line\n".repeat(1000)).0.lines().count(), LINES);
            assert_eq!(dimensions(0, u32::MAX), (1, 1024));
        }
        #[test]
        fn missing_grammar_never_claims_highlighting() {
            assert!(fallback(Some("rust"), true).is_none());
            assert!(fallback(Some("rust"), false)
                .unwrap()
                .contains("unavailable"));
            assert!(fallback(None, false).unwrap().contains("plain text"));
            assert_eq!(fallback(Some("text"), false), Some("Plain text preview"));
        }
        #[test]
        fn exact_source_and_presentation_inputs_separate_cache_entries() {
            let key = Key {
                root: "/vault".into(),
                relative: "one/code.rs".into(),
                resolved: "/vault/one/code.rs".into(),
                revision: [1; 32],
                language: Some("rust"),
                grammar: Some([2; 32]),
                width: 224,
                height: 144,
                theme_revision: 1,
                note_font: "Note".into(),
                code_font: "Code".into(),
                font_size: 18f32.to_bits(),
            };
            let mut variants = vec![];
            let mut other = key.clone();
            other.root = "/other".into();
            variants.push(other);
            let mut other = key.clone();
            other.relative = "two/code.rs".into();
            variants.push(other);
            let mut other = key.clone();
            other.resolved = "/vault/target.rs".into();
            variants.push(other);
            let mut other = key.clone();
            other.revision[0] += 1;
            variants.push(other);
            let mut other = key.clone();
            other.language = Some("python");
            variants.push(other);
            let mut other = key.clone();
            other.grammar = None;
            variants.push(other);
            let mut other = key.clone();
            other.width += 1;
            variants.push(other);
            let mut other = key.clone();
            other.height += 1;
            variants.push(other);
            let mut other = key.clone();
            other.theme_revision += 1;
            variants.push(other);
            let mut other = key.clone();
            other.note_font = "Changed".into();
            variants.push(other);
            let mut other = key.clone();
            other.code_font = "Changed".into();
            variants.push(other);
            let mut other = key.clone();
            other.font_size = 20f32.to_bits();
            variants.push(other);
            assert_eq!(key, key.clone());
            for other in variants {
                assert_ne!(key, other);
            }
        }
    }
}
pub(crate) use policy::Key;

#[derive(Clone)]
pub(crate) struct Prepared {
    root: PathBuf,
    relative: String,
    resolved: PathBuf,
    revision: [u8; 32],
    language: Option<&'static str>,
    text: String,
    pub(crate) partial: bool,
}
impl Prepared {
    /// Matches the gallery's standard 144px-high preview. No disk I/O.
    pub(crate) fn key(&self, width_px: u32, theme_revision: u64, cx: &App) -> Key {
        self.key_for_size(width_px, 144, theme_revision, cx)
    }
    pub(crate) fn key_for_size(
        &self,
        width_px: u32,
        height_px: u32,
        theme_revision: u64,
        cx: &App,
    ) -> Key {
        let (width, height) = policy::dimensions(width_px, height_px);
        Key {
            root: self.root.clone(),
            relative: self.relative.clone(),
            resolved: self.resolved.clone(),
            revision: self.revision,
            language: self.language,
            grammar: grammar(self.language),
            width,
            height,
            theme_revision,
            note_font: reader_fonts::note(cx).to_string(),
            code_font: reader_fonts::code(cx).to_string(),
            font_size: reader_ui_state::font_size(cx).to_bits(),
        }
    }
}

/// Call only from the gallery's existing background worker. The caller retains
/// its outer revision/generation checks and invalidates this data on source events.
/// Oversize files show a marked prefix; binary/invalid encoding returns an error
/// for the existing visible Unavailable card. No lossy decoding or source writes.
pub(crate) fn load(root: &Path, relative: &str) -> anyhow::Result<Prepared> {
    anyhow::ensure!(
        !relative.is_empty()
            && Path::new(relative)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "Invalid vault-relative source identity"
    );
    let root = root.canonicalize()?;
    let resolved = reader_files::checked_path(&root, relative)?;
    let metadata = std::fs::metadata(&resolved)?;
    anyhow::ensure!(metadata.is_file(), "Not a regular file");
    let before = stamp(&metadata)?;
    let source::Preview::Content { text, partial } = source::load(&resolved)? else {
        anyhow::bail!("Source encoding or binary content is unsupported");
    };
    let after_path = reader_files::checked_path(&root, relative)?;
    anyhow::ensure!(
        resolved == after_path && before == stamp(&std::fs::metadata(&after_path)?)?,
        "Source changed while preparing thumbnail"
    );
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    digest.update(before.as_bytes());
    let revision = digest.finalize().into();
    let (prefix, clipped) = policy::prefix(&text);
    Ok(Prepared {
        root,
        relative: relative.into(),
        resolved,
        revision,
        // Exact existing filename/extension mapping; never sniff source content.
        language: reader_code_file::language(relative).or_else(|| {
            Path::new(relative).extension().and_then(|ext| {
                (ext.eq_ignore_ascii_case("txt") || ext.eq_ignore_ascii_case("log"))
                    .then_some("text")
            })
        }),
        text: prefix.into(),
        partial: partial || clipped,
    })
}

fn stamp(metadata: &std::fs::Metadata) -> anyhow::Result<String> {
    let mut stamp = format!(
        "{}:{:?}:{:?}",
        metadata.len(),
        metadata.modified()?,
        metadata.created()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        stamp.push_str(&format!(
            ":{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec()
        ));
    }
    Ok(stamp)
}

/// Require both the installed grammar and a nonempty highlighting query. Include
/// the query in the presentation key so newly installed/changed support invalidates
/// an earlier plain fallback without rereading the source.
fn grammar(language: Option<&str>) -> Option<[u8; 32]> {
    crate::source_presentation::code_languages::install();
    let config = LanguageRegistry::singleton().language(language?)?;
    (config.language.is_some() && !config.highlights.trim().is_empty())
        .then(|| Sha256::digest(config.highlights.as_bytes()).into())
}

fn fenced(text: &str, language: &str) -> String {
    let mut fenced = source::fenced(text);
    let end = fenced
        .find('\n')
        .expect("existing fence builder emits a first line");
    fenced.insert_str(end, language);
    fenced
}

/// Actual GPUI syntax highlighting through the same installed resolver as Reader.
/// No Reader plugins: source cannot become a link, image, embed or diagram job.
/// Notices render here so unsupported/oversize data cannot silently look complete.
pub(crate) fn render(
    id: impl Into<ElementId>,
    prepared: &Prepared,
    width_px: u32,
    height_px: u32,
    cx: &mut App,
) -> AnyElement {
    let (width, height) = policy::dimensions(width_px, height_px);
    let supported = grammar(prepared.language).is_some();
    let language = if supported {
        prepared.language.unwrap_or("text")
    } else {
        "text"
    };
    let view = TextView::markdown(id, fenced(&prepared.text, language))
        .code_block_language(reader_code_language::resolver())
        .code_block_actions(|_, _, _| div().into_any_element())
        .style(reader_fonts::text_style(cx))
        .font_family(reader_fonts::code(cx))
        .text_size(px(reader_ui_state::font_size(cx)))
        .text_color(cx.theme().foreground)
        .selectable(false)
        .scrollable(false)
        .on_link_click(|_, _, _, _| {})
        .w_full();
    div()
        .w(px(width as f32))
        .h(px(height as f32))
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(cx.theme().background)
        .when_some(
            policy::fallback(prepared.language, supported),
            |card, notice| {
                card.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(notice),
                )
            },
        )
        .when(prepared.partial, |card| {
            card.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Preview limited to first 8 KiB / 128 lines"),
            )
        })
        .child(view)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn code_identity_uses_existing_filename_mapping_without_sniffing() {
        assert_eq!(
            reader_code_file::language("nested/CONFIG.JSON"),
            Some("json")
        );
        assert_eq!(reader_code_file::language("nested/main.rs"), Some("rust"));
        assert_eq!(
            reader_code_file::language("nested/CMakeLists.txt"),
            Some("cmake")
        );
        assert_eq!(reader_code_file::language("nested/unknown.custom"), None);
    }
    #[test]
    fn authored_fences_and_markdown_stay_literal_code() {
        let code =
            "let source = \"````\n# no heading\n[no link](https://example.test)\n````\";\r\n";
        let rendered = fenced(code, "rust");
        assert!(rendered.starts_with("`````rust\n"));
        assert!(rendered.ends_with("\n`````\n"));
        assert!(rendered.contains(code));
        assert_eq!(rendered.lines().filter(|line| *line == "`````").count(), 1);
    }
}
