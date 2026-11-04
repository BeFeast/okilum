//! Real, clipped Reader Markdown for Recent cards; no screenshots or source writes.
//! Caller owns jobs, cancellation and cache eviction. Never call `load` in render.
use crate::{markdown_plugins, reader_files, reader_fonts, reader_ui_state, MarkdownImage};
use gpui::{div, prelude::*, px, AnyElement, App, ElementId};
use gpui_component::{
    text::{SelectionFormat, TextView},
    ActiveTheme as _,
};
use okilum_core::{
    render::{reader_document_from_source, ReaderDocument},
    vault::Vault,
};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

// Reuse the existing bounded, lossless UTF-8 reader, including BOM/binary handling.
use crate::reader_plain_text::source;

mod policy {
    use std::path::PathBuf;

    pub(super) const RENDER_BYTES: usize = 8192;
    pub(super) const RENDER_LINES: usize = 160;

    /// Includes presentation dimensions, not just a note name or an mtime.
    /// Caller must advance theme_revision for every palette/style change.
    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub(crate) struct Key {
        pub(super) root: PathBuf,
        pub(super) relative: String,
        pub(super) resolved: PathBuf,
        pub(super) revision: [u8; 32],
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

    /// Keep authored Markdown, not a stripped-text or generated-line imitation.
    pub(super) fn prefix(text: &str) -> (&str, bool) {
        let mut end = text.len().min(RENDER_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if let Some((offset, _)) = text[..end].match_indices('\n').nth(RENDER_LINES - 1) {
            end = offset + 1;
        }
        (&text[..end], end < text.len())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn markdown_is_preserved_and_prefix_work_is_bounded() {
            let markdown =
                "# Heading\n\n**Bold** and [link](note.md)\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";
            assert_eq!(prefix(markdown), (markdown, false));
            let unicode = format!("{}Жtail", "a".repeat(RENDER_BYTES - 1));
            let (prefix, partial) = self::super::prefix(&unicode);
            assert!(partial);
            assert_eq!(prefix.len(), RENDER_BYTES - 1);
            assert_eq!(
                super::prefix(&"x\n".repeat(1000)).0.lines().count(),
                RENDER_LINES
            );
            assert_eq!(dimensions(0, u32::MAX), (1, 1024));
        }
        #[test]
        fn cache_identity_changes_for_every_render_input() {
            let key = Key {
                root: "/vault".into(),
                relative: "a/note.md".into(),
                resolved: "/vault/a/note.md".into(),
                revision: [1; 32],
                width: 224,
                height: 144,
                theme_revision: 1,
                note_font: "Note".into(),
                code_font: "Code".into(),
                font_size: 18f32.to_bits(),
            };
            let mut variants = vec![];
            let mut other = key.clone();
            other.root = "/another".into();
            variants.push(other);
            let mut other = key.clone();
            other.relative = "b/note.md".into();
            variants.push(other);
            let mut other = key.clone();
            other.resolved = "/vault/target.md".into();
            variants.push(other);
            let mut other = key.clone();
            other.revision[0] = 2;
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
            other.note_font = "Other".into();
            variants.push(other);
            let mut other = key.clone();
            other.code_font = "Other".into();
            variants.push(other);
            let mut other = key.clone();
            other.font_size = 20f32.to_bits();
            variants.push(other);
            for other in variants {
                assert_ne!(key, other);
            }
            assert_eq!(key, key.clone());
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
    document: ReaderDocument,
    pub(crate) partial: bool,
}
impl Prepared {
    /// Key for the gallery's standard 144px-high preview.
    /// Source events must schedule `load` again: this key describes captured bytes,
    /// and deliberately does no disk I/O when the gallery paints.
    pub(crate) fn key(&self, width_px: u32, theme_revision: u64, cx: &App) -> Key {
        self.key_for_size(width_px, 144, theme_revision, cx)
    }

    /// For other heights, pass the same dimensions used by `render`.
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
            width,
            height,
            theme_revision,
            note_font: reader_fonts::note(cx).to_string(),
            code_font: reader_fonts::code(cx).to_string(),
            font_size: reader_ui_state::font_size(cx).to_bits(),
        }
    }
}

/// Background-only preparation. Reads at most the existing reader's 64 KiB + 1,
/// renders at most 8 KiB/160 lines, and never hydrates other notes or image assets.
/// Errors are an unavailable card, never a successful empty/text-only thumbnail.
pub(crate) fn load(root: &Path, relative: &str) -> anyhow::Result<Prepared> {
    anyhow::ensure!(
        !relative.is_empty()
            && Path::new(relative)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "Invalid vault-relative note identity"
    );
    anyhow::ensure!(
        Path::new(relative)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            && !okilum_core::excalidraw::is_drawing(relative),
        "Not a Markdown note"
    );
    let root = root.canonicalize()?;
    let resolved = reader_files::checked_path(&root, relative)?;
    let before = std::fs::metadata(&resolved)?;
    anyhow::ensure!(before.is_file(), "Not a regular file");
    let before_stamp = stamp(&before)?;
    let source::Preview::Content { text, partial } = source::load(&resolved)? else {
        anyhow::bail!("Markdown source is not valid UTF-8 text");
    };
    let after_path = reader_files::checked_path(&root, relative)?;
    let after = std::fs::metadata(&after_path)?;
    anyhow::ensure!(
        resolved == after_path && before_stamp == stamp(&after)?,
        "Note changed while preparing thumbnail"
    );
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    digest.update(before_stamp.as_bytes());
    let revision = digest.finalize().into();
    let (body, clipped) = policy::prefix(&text);
    // A provisional inventory keeps note/block embeds pending instead of reading
    // arbitrary linked notes. This is the existing Reader's bounded-preview mode.
    let mut vault = Vault::from_note_paths([relative.to_owned()]);
    vault.root = root.clone();
    let document = reader_document_from_source(&vault, relative, body);
    anyhow::ensure!(
        document.rendered.len() <= 128 * 1024,
        "Rendered thumbnail exceeds limit"
    );
    Ok(Prepared {
        root,
        relative: relative.into(),
        resolved,
        revision,
        document,
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

/// Paint native Markdown directly into a card. No GPU readback, screenshot,
/// network fetch, source edit, or click-driven link navigation.
/// Assets remain explicit unavailable placeholders; linked-note embeds stay pending.
pub(crate) fn render(
    id: impl Into<ElementId>,
    prepared: &Prepared,
    width_px: u32,
    height_px: u32,
    cx: &mut App,
) -> AnyElement {
    let (width, height) = policy::dimensions(width_px, height_px);
    let view = markdown_plugins(
        TextView::markdown(id, prepared.document.rendered.clone()),
        Arc::new(|_, _, _, _| {}),
        Arc::new(|_| Some(MarkdownImage::Unavailable)),
        SelectionFormat::Plain,
    )
    .code_block_actions(|_, _, _| div().into_any_element())
    .style(reader_fonts::text_style(cx))
    .font_family(reader_fonts::note(cx))
    .text_size(px(reader_ui_state::font_size(cx)))
    .text_color(cx.theme().foreground)
    .selectable(false)
    .scrollable(false)
    .w_full();
    div()
        .w(px(width as f32))
        .h(px(height as f32))
        .overflow_hidden()
        .bg(cx.theme().background)
        .child(view)
        .into_any_element()
}
