//! Theme-independent code captures, computed only by the background worker.
use super::*;
use gpui::SharedString;
use gpui_base::input::HighlightStyleResolver;
use gpui_component::highlighter::{HighlightTheme, LanguageRegistry, SyntaxHighlighter};
use gpui_component::Rope;

#[derive(Default)]
pub(super) struct CodeSyntax {
    source: Option<SourceSnapshot>,
    spans: Vec<(Range<usize>, SharedString)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::input::projection::SourceStamp;

    fn source(text: String) -> SourceSnapshot {
        SourceSnapshot {
            stamp: SourceStamp {
                document: 1991,
                generation: 1,
            },
            text: text.into(),
        }
    }

    #[test]
    fn csharp_aliases_and_popular_code_match_reader_colors_in_both_themes() {
        for (language, body) in [
            ("csharp", "class Demo { public int N = 7; }\n"),
            ("c#", "class Demo { public int N = 7; }\n"),
            ("cs", "class Demo { public int N = 7; }\n"),
            ("C#", "class Demo { public int N = 7; }\n"),
            ("rust", "fn run() { let value = 7; }\n"),
            ("python", "def run():\n    return 7\n"),
            ("js", "const value = 7;\n"),
        ] {
            let note = source(format!("# Heading\n\n```{language}\n{body}```\n\nprose\n"));
            let provider = Arc::new(CachedProvider::classify(note.clone()));
            let config = LanguageRegistry::singleton()
                .language(&okilum_core::code_language::canonical(language))
                .expect("positive control: tested language grammar is registered");
            assert!(
                !config.highlights.trim().is_empty(),
                "positive control: {language} has a nonempty highlight query"
            );
            let at = note.text.find(body).unwrap();
            let active = ActiveSource {
                anchor: SourceByte(at),
                head: SourceByte(at),
                ..Default::default()
            };
            for theme in [
                HighlightTheme::default_light(),
                HighlightTheme::default_dark(),
            ] {
                let mut reader =
                    SyntaxHighlighter::new(&okilum_core::code_language::canonical(language));
                reader.update(None, &Rope::from_str(body), None);
                let expected = reader
                    .styles(&(0..body.len()), &*theme)
                    .into_iter()
                    .find_map(|(range, style)| range.contains(&0).then_some(style.color).flatten())
                    .unwrap_or_else(|| {
                        panic!("positive control: Reader colors {language} keyword")
                    });
                let colors = ProjectionColors {
                    heading: gpui::rgb(0).into(),
                    link: gpui::rgb(0).into(),
                    muted: gpui::rgb(0).into(),
                };
                let projection = provider
                    .clone()
                    .with_code_syntax(colors, CODE_FONT.to_owned(), theme)
                    .compose(&note, &active)
                    .unwrap();
                assert!(projection.blocks().is_empty(), "active code stays editable");
                let projected = projection
                    .to_projected(SourceByte(at), ConcealBias::Left)
                    .unwrap()
                    .0;
                let actual = projection
                    .styles()
                    .iter()
                    .rev()
                    .filter(|style| {
                        style.range.start.0 <= projected && projected < style.range.end.0
                    })
                    .find_map(|style| style.color);
                assert_eq!(
                    actual,
                    Some(expected),
                    "{language}: Live and Reader share syntax colors"
                );
                assert!(projection.styles().iter().any(|style| {
                    style.range.start.0 <= projected
                        && projected < style.range.end.0
                        && style.font_family.as_deref() == Some(CODE_FONT)
                }));
                let prose = projection.text().find("prose").unwrap();
                assert!(
                    !projection.styles().iter().any(|style| {
                        style.range.start.0 <= prose
                            && prose < style.range.end.0
                            && style.color.is_some()
                    }),
                    "code colors do not escape into prose"
                );
                assert_eq!(&*projection.source().text, &*note.text);
            }
        }
    }

    #[test]
    fn semantic_span_api_clips_utf8_and_stale_source_never_gets_old_token_offsets() {
        let mut highlighter = SyntaxHighlighter::new("rust");
        let code = "fn café() { let n = 7; }\n";
        highlighter.update(None, &Rope::from_str(code), None);
        let spans = highlighter.semantic_spans(&(0..code.len()));
        assert!(spans
            .iter()
            .any(|(range, name)| range.contains(&0) && name.as_ref() == "keyword"));
        let inside = code.find('é').unwrap() + 1;
        for (range, _) in highlighter.semantic_spans(&(inside..code.len())) {
            assert!(code.is_char_boundary(range.start) && code.is_char_boundary(range.end));
        }
        assert!(highlighter
            .semantic_spans(&(code.len()..code.len()))
            .is_empty());

        let note = source(format!("```rust\n{code}```\n\nend"));
        let provider = CachedProvider::classify(note.clone());
        let map = provider
            .retained
            .lock()
            .unwrap()
            .prepare_reveal(&Active::default())
            .unwrap()
            .projection()
            .clone();
        let mut styles = Vec::new();
        provider
            .syntax
            .project(&note, &map, &HighlightTheme::default_light(), &mut styles);
        assert!(!styles.is_empty(), "exact-snapshot positive control");
        let changed = SourceSnapshot {
            stamp: SourceStamp {
                generation: 2,
                ..note.stamp
            },
            text: format!("prefix\n{}", note.text).into(),
        };
        styles.clear();
        provider.syntax.project(
            &changed,
            &map,
            &HighlightTheme::default_light(),
            &mut styles,
        );
        assert!(styles.is_empty(), "pending edits never reuse stale offsets");
    }
}

impl CodeSyntax {
    pub(super) fn new(source: &SourceSnapshot, classified: &Classification) -> Self {
        super::code_languages::install();
        if source.text.len() > source_classifier::MAX_BYTES {
            return Self::default();
        }
        let snapshot = core_snapshot(source);
        let blocks: Vec<_> = classified
            .styles_for(&snapshot)
            .unwrap_or_default()
            .iter()
            .filter(|style| style.style == Style::CodeBlock)
            .map(|style| style.range.clone())
            .collect();
        if blocks.is_empty() {
            return Self::default();
        }

        // Reader labels and Markdown injections resolve the same authored name.
        // Register the actual spelling too, so mixed-case aliases work without
        // modifying authored info strings or parser input.
        let registry = LanguageRegistry::singleton();
        for block in &blocks {
            let Some(header) = source
                .text
                .get(block.clone())
                .and_then(|s| s.lines().next())
            else {
                continue;
            };
            let Some(start) = header.find(['`', '~']) else {
                continue;
            };
            let fence = header.as_bytes()[start];
            let width = header.as_bytes()[start..]
                .iter()
                .take_while(|b| **b == fence)
                .count();
            if width < 3 {
                continue;
            }
            let Some(authored) = header[start + width..].split_whitespace().next() else {
                continue;
            };
            let canonical = okilum_core::code_language::canonical(authored);
            if let Some(config) = registry.language(&canonical) {
                registry.register(authored, &config);
            }
        }

        let text = Rope::from_str(&source.text);
        let mut highlighter = SyntaxHighlighter::new("markdown");
        highlighter.update(None, &text, None);
        let mut spans = Vec::new();
        'captures: for (range, name) in highlighter.semantic_spans(&(0..source.text.len())) {
            let first = blocks.partition_point(|block| block.end <= range.start);
            for block in blocks[first..]
                .iter()
                .take_while(|block| block.start < range.end)
            {
                let clipped = range.start.max(block.start)..range.end.min(block.end);
                if clipped.is_empty() {
                    continue;
                }
                if spans.len() == source_classifier::MAX_STYLES_AND_REASONS {
                    break 'captures;
                }
                spans.push((clipped, name.clone()));
            }
        }
        Self {
            source: Some(source.clone()),
            spans,
        }
    }

    pub(super) fn project(
        &self,
        source: &SourceSnapshot,
        map: &impl PresentationMap,
        theme: &HighlightTheme,
        styles: &mut Vec<ProjectionStyle>,
    ) {
        if !self
            .source
            .as_ref()
            .is_some_and(|old| old.stamp == source.stamp && old.text == source.text)
        {
            // A pending edit must never receive token offsets from an old tree.
            return;
        }
        for (range, name) in &self.spans {
            if styles.len() >= source_classifier::MAX_STYLES_AND_REASONS {
                break;
            }
            let Some(start) = map.map_source(range.start) else {
                continue;
            };
            let Some(end) = map.map_source(range.end) else {
                continue;
            };
            let Some(style) = theme.style(name) else {
                continue;
            };
            if start < end {
                styles.push(ProjectionStyle {
                    range: ProjectedByte(start)..ProjectedByte(end),
                    font_family: None,
                    color: style.color,
                    bold: style
                        .font_weight
                        .is_some_and(|weight| weight >= gpui::FontWeight::BOLD),
                    italic: style.font_style == Some(gpui::FontStyle::Italic),
                    strikethrough: style.strikethrough.is_some(),
                });
            }
        }
    }
}
