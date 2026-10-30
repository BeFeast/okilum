//! Authored image ranges and the same captured resolver used by Reader images.
use super::*;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageReference {
    pub range: Range<usize>,
    pub target: String,
    /// The URL the renderer would use. Missing/ambiguous images have no URL.
    pub url: Option<String>,
}

/// Source-menu lookup never reads the filesystem or invents a basename match.
/// Code examples are excluded by the same Markdown parser as Reader rendering.
pub fn references(source: &str, vault: &Vault, from: &str) -> Vec<ImageReference> {
    let resolver = crate::document_links::IndexedResolver::new(vault);
    let lines: Vec<_> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let arena = Arena::new();
    let root = parse_document(&arena, source, &comrak_options());
    let mut images = Vec::new();
    for node in root.descendants() {
        let data = node.data.borrow();
        let NodeValue::Image(image) = &data.value else {
            continue;
        };
        if node
            .ancestors()
            .skip(1)
            .any(|parent| matches!(parent.data.borrow().value, NodeValue::Image(_)))
        {
            continue;
        }
        let pos = data.sourcepos;
        let Some(start) = lines.get(pos.start.line.saturating_sub(1)) else {
            continue;
        };
        let Some(end) = lines.get(pos.end.line.saturating_sub(1)) else {
            continue;
        };
        let range = start + pos.start.column.saturating_sub(1)..end + pos.end.column;
        if source
            .get(range.clone())
            .is_some_and(|value| value.starts_with("!["))
        {
            images.push(ImageReference {
                range,
                target: image.url.clone(),
                url: resolve(&image.url, vault, &resolver, from),
            });
        }
    }
    // Raw ![[...]] is not a Comrak WikiLink. Use the renderer's own embed
    // grammar and prose boundaries while retaining the untouched byte range.
    let body_start = source.len() - without_frontmatter(source).len();
    for prose in crate::prose::prose_spans(source) {
        for found in embed_regex().find_iter(&source[prose.clone()]) {
            let range = prose.start + found.start()..prose.start + found.end();
            if range.start < body_start {
                continue;
            }
            let escapes = source.as_bytes()[..range.start]
                .iter()
                .rev()
                .take_while(|&&byte| byte == b'\\')
                .count();
            if !escapes.is_multiple_of(2)
                || images
                    .iter()
                    .any(|image| image.range.start <= range.start && image.range.end >= range.end)
            {
                continue;
            }
            let authored = &source[range.clone()];
            let inner = &authored[3..authored.len() - 2];
            let base = crate::document_links::wiki_parts(inner.trim())
                .0
                .split('#')
                .next()
                .unwrap_or("")
                .trim();
            let prepared = preprocess(authored);
            // Notes and drawings keep their existing, separate interactions.
            if !prepared.starts_with("![](") || crate::excalidraw::is_drawing(base) {
                continue;
            }
            let mut target = None;
            let _ = rewrite_source_images_with(&prepared, |url| {
                target = Some(url.to_owned());
                None
            });
            if let Some(target) = target {
                images.push(ImageReference {
                    range,
                    url: resolve(&target, vault, &resolver, from),
                    target,
                });
            }
        }
    }
    images.sort_by_key(|image| image.range.start);
    images
}

fn resolve(
    target: &str,
    vault: &Vault,
    resolver: &crate::document_links::IndexedResolver<'_>,
    from: &str,
) -> Option<String> {
    if target.contains("://") {
        let url = url::Url::parse(target).ok()?;
        return matches!(url.scheme(), "http" | "https" | "file").then(|| url.into());
    }
    image_file_url(target, vault, resolver, from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_and_markdown_images_match_the_renderer_without_fs_reads() {
        let fixture = tempfile::tempdir().unwrap();
        let mut vault = Vault::from_note_paths(["note.md".into()]);
        vault.root = fixture.path().canonicalize().unwrap();
        vault.inventory_complete = true;
        vault.entries.push(crate::vault::VaultEntry {
            path: "attachments/Привет image.png".into(),
            kind: crate::vault::EntryKind::Attachment,
        });
        let source = "![[attachments/Привет image.png|120]]\n\n![alt](attachments/Привет%20image.png)\n\n`![[ignored.png]]`\n\n```\n![x](ignored.png)\n```\n";
        let images = references(source, &vault, "note.md");
        assert_eq!(images.len(), 2);
        let expected = url::Url::from_file_path(vault.root.join("attachments/Привет image.png"))
            .unwrap()
            .to_string();
        for image in images {
            assert!(source[image.range].starts_with("!["));
            assert_eq!(image.url.as_deref(), Some(expected.as_str()));
        }
        let rendered = rewrite_source_images(&preprocess(source), &vault, "note.md");
        assert_eq!(rendered.matches(&expected).count(), 2);
        for literal in [
            r"\![[attachments/Привет image.png]]",
            "`![[attachments/Привет image.png]]`",
            "    ![[attachments/Привет image.png]]\n",
            "---\nimage: '![[attachments/Привет image.png]]'\n---\n# Body\n",
            "![[note.md]]",
            "![[drawing.excalidraw]]",
        ] {
            assert!(
                references(literal, &vault, "note.md").is_empty(),
                "{literal}"
            );
        }
        for source in [
            r"\\![[attachments/Привет image.png]]",
            "| Image |\n| --- |\n| ![[attachments/Привет image.png\\|120]] |\n",
        ] {
            let images = references(source, &vault, "note.md");
            assert_eq!(images.len(), 1, "{source}");
            assert!(source[images[0].range.clone()].starts_with("![["));
            assert_eq!(images[0].url.as_deref(), Some(expected.as_str()));
            let rendered = rewrite_source_images(&preprocess(source), &vault, "note.md");
            assert!(rendered.contains(&expected), "{source}");
        }
    }

    #[test]
    fn missing_ambiguous_and_remote_images_keep_their_resolution_contract() {
        let fixture = tempfile::tempdir().unwrap();
        let mut vault = Vault::from_note_paths(["note.md".into()]);
        vault.root = fixture.path().canonicalize().unwrap();
        vault.inventory_complete = true;
        for path in ["a/duplicate.png", "b/duplicate.png"] {
            vault.entries.push(crate::vault::VaultEntry {
                path: path.into(),
                kind: crate::vault::EntryKind::Attachment,
            });
        }
        let source =
            "![[missing.png]]\n![[duplicate.png]]\n![remote](https://example.invalid/a%20b.png)\n";
        let images = references(source, &vault, "note.md");
        assert_eq!(images.len(), 3);
        assert!(images[0].url.is_none() && images[1].url.is_none());
        assert_eq!(
            images[2].url.as_deref(),
            Some("https://example.invalid/a%20b.png")
        );
    }

    #[test]
    fn reference_image_ranges_preserve_unicode_crlf_and_ignore_plain_links() {
        let fixture = tempfile::tempdir().unwrap();
        let mut vault = Vault::from_note_paths(["notes/current.md".into()]);
        vault.root = fixture.path().canonicalize().unwrap();
        vault.inventory_complete = true;
        vault.entries.push(crate::vault::VaultEntry {
            path: "notes/pictures/фото space.png".into(),
            kind: crate::vault::EntryKind::Attachment,
        });
        let source = "---\r\nimage: '![metadata](ignored.png)'\r\n---\r\n\r\nПривет [plain](pictures/фото%20space.png)\r\n\r\nПривет ![copy][asset]\r\n\r\n[asset]: <pictures/фото%20space.png>\r\n";
        let images = references(source, &vault, "notes/current.md");
        assert_eq!(images.len(), 1);
        assert_eq!(&source[images[0].range.clone()], "![copy][asset]");
        let expected = url::Url::from_file_path(vault.root.join("notes/pictures/фото space.png"))
            .unwrap()
            .to_string();
        assert_eq!(images[0].url.as_deref(), Some(expected.as_str()));
        let rendered = rewrite_source_images(&preprocess(source), &vault, "notes/current.md");
        assert_eq!(rendered.matches(&expected).count(), 1);
        assert_eq!(std::fs::read_dir(&vault.root).unwrap().count(), 0);
    }
}
