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
    // Comrak treats ![[...]] as a literal ! followed by a WikiLink. Reuse its
    // verified original range; never scan strings inside code or escaped prose.
    for link in crate::document_links::parse(source) {
        if !link.wiki || !link.exact_range || link.range.start == 0 {
            continue;
        }
        let start = link.range.start - 1;
        if source.as_bytes().get(start) != Some(&b'!')
            || start > 0 && source.as_bytes()[start - 1] == b'\\'
        {
            continue;
        }
        let range = start..link.range.end;
        let Some(authored) = source.get(range.clone()) else {
            continue;
        };
        let prepared = preprocess(authored);
        // Only image embeds survive preprocess as Markdown images. Notes and
        // drawings retain their existing, separate interactions.
        if !prepared.starts_with("![](") || crate::excalidraw::is_drawing(&link.target) {
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
}
