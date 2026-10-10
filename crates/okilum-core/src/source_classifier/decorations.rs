//! Read-only paint metadata from the classifier's existing, guarded AST.
//! No projection regions, formatting acceptance or reveal scopes are modified.
use super::{Context, MAX_STYLES_AND_REASONS};
use comrak::nodes::{AstNode, ListType, NodeValue};
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Unordered {
        depth: usize,
    },
    Quote {
        depth: usize,
    },
    ThematicBreak,
    /// A top-level fenced or indented code block: a quiet row background only.
    /// Its literal contents are never classified.
    CodeBlock,
    /// A bullet task item's `- [ ]` / `- [x]`, painted as a checkbox (S5, #1034).
    /// Its scope is the marker alone: the checkbox stays while the caret is in
    /// the task text and reveals only with the caret inside the marker.
    Task {
        checked: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub range: Range<usize>,
    /// AST container bounds; the shared policy adapter decides reveal visibility.
    pub scope: Range<usize>,
    pub kind: Kind,
}

pub(super) fn extract<'a>(root: &'a AstNode<'a>, context: &Context<'_>) -> Option<Vec<Marker>> {
    let mut markers = Vec::new();
    for node in root.descendants() {
        let value = &node.data.borrow().value;
        match value {
            NodeValue::Item(list) if list.list_type == ListType::Bullet => {
                let scope = context.range(node)?;
                let raw = context.source.get(scope.clone())?;
                if raw.as_bytes().first().copied() != Some(list.bullet_char)
                    || !matches!(list.bullet_char, b'-' | b'+' | b'*')
                    || raw
                        .as_bytes()
                        .get(1)
                        .is_some_and(|b| !b.is_ascii_whitespace())
                {
                    return None;
                }
                // Task items parse as `TaskItem` and never reach this arm; keep
                // the raw guard so checkbox paint cannot change if that differs.
                let body = raw.get(1..)?.trim_start_matches([' ', '\t']);
                if ["[ ]", "[x]", "[X]"]
                    .iter()
                    .any(|prefix| body.starts_with(prefix))
                {
                    continue;
                }
                let depth = node
                    .ancestors()
                    .filter(|ancestor| matches!(ancestor.data.borrow().value, NodeValue::List(_)))
                    .count();
                markers.push(Marker {
                    range: scope.start..scope.start + 1,
                    scope,
                    kind: Kind::Unordered { depth },
                });
            }
            NodeValue::TaskItem(symbol) => {
                // An unexpected shape leaves this task raw; it does not cost the
                // note its other decorations.
                if let Some(marker) = task_marker(node, context, symbol.is_some()) {
                    markers.push(marker);
                }
            }
            NodeValue::BlockQuote => {
                // An unsupported quote prefix leaves that quote raw; it does not
                // cost the rest of the note its decorations.
                markers.extend(quote_markers(node, context).unwrap_or_default());
            }
            NodeValue::CodeBlock(_)
                if node
                    .parent()
                    .is_some_and(|p| matches!(p.data.borrow().value, NodeValue::Document)) =>
            {
                // An odd block costs only its own background, not the note's
                // other decorations.
                let Some(range) = context.range(node) else {
                    continue;
                };
                let start = context.lines[node.data.borrow().sourcepos.start.line - 1];
                let Some(text) = context.source.get(start..range.end) else {
                    continue;
                };
                // End on the last non-blank line: an indented block's AST range
                // also swallows trailing whitespace-only lines.
                let mut end = start;
                let mut offset = start;
                for line in text.split_inclusive('\n') {
                    if !line.trim().is_empty() {
                        end = offset + line.trim_end_matches(['\r', '\n']).len();
                    }
                    offset += line.len();
                }
                if end == start {
                    continue;
                }
                let range = start..end;
                markers.push(Marker {
                    range: range.clone(),
                    scope: range,
                    kind: Kind::CodeBlock,
                });
            }
            NodeValue::ThematicBreak => {
                let scope = context.range(node)?;
                let raw = context.source.get(scope.clone())?;
                let mut delimiters = raw.bytes().filter(|b| !matches!(b, b' ' | b'\t'));
                let delimiter = delimiters.next()?;
                if !matches!(delimiter, b'-' | b'_' | b'*') {
                    return None;
                }
                let mut count = 1;
                for byte in delimiters {
                    if byte != delimiter {
                        return None;
                    }
                    count += 1;
                }
                if count < 3 {
                    return None;
                }
                markers.push(Marker {
                    range: scope.clone(),
                    scope,
                    kind: Kind::ThematicBreak,
                });
            }
            _ => {}
        }
        if markers.len() > MAX_STYLES_AND_REASONS {
            return None;
        }
    }
    validate_ranges(&markers, context)?;
    markers.sort_by_key(|marker| marker.range.start);
    if markers
        .windows(2)
        .any(|pair| pair[0].range.end > pair[1].range.start)
    {
        return None;
    }
    Some(markers)
}

/// One `>` marker per quote line. The first line's AST column fixes where this
/// quote's delimiter sits; later lines carry it at that column after only spaces
/// and parent delimiters, or are lazy continuations without one. Each marker
/// reveals as its own `> ` element, not the whole quote (#868).
fn quote_markers<'a>(node: &'a AstNode<'a>, context: &Context<'_>) -> Option<Vec<Marker>> {
    let scope = context.range(node)?;
    let source = context.source.as_bytes();
    if source.get(scope.start) != Some(&b'>') {
        return None;
    }
    let depth = node
        .ancestors()
        .filter(|ancestor| matches!(ancestor.data.borrow().value, NodeValue::BlockQuote))
        .count();
    let pos = node.data.borrow().sourcepos;
    let column = scope.start - *context.lines.get(pos.start.line - 1)?;
    let mut markers = Vec::new();
    for line in pos.start.line..=pos.end.line {
        let start = *context.lines.get(line - 1)?;
        // The first line's offset is AST-proven (its prefix may hold a list
        // marker). Later lines: this quote's delimiter is the depth-th `>`
        // after only spaces and parent delimiters, near the first column.
        let offset = if line == pos.start.line {
            Some(scope.start)
        } else {
            let prefix_end = source[start..]
                .iter()
                .position(|&b| b != b' ' && b != b'>')
                .map_or(source.len(), |i| start + i);
            (start..prefix_end)
                .filter(|&i| source[i] == b'>')
                .nth(depth - 1)
                .filter(|&i| (i - start).abs_diff(column) <= 3)
        };
        let Some(offset) = offset.filter(|&offset| offset < scope.end) else {
            continue;
        };
        let end = offset + 1 + usize::from(source.get(offset + 1) == Some(&b' '));
        // A delimiter joined to a combining mark cannot be replaced alone.
        if ![offset, offset + 1, end]
            .into_iter()
            .all(|i| context.boundary(i))
        {
            return None;
        }
        markers.push(Marker {
            range: offset..offset + 1,
            scope: offset..end.min(scope.end),
            kind: Kind::Quote { depth },
        });
    }
    Some(markers)
}

/// Marker/scopes must be grapheme-aligned and scopes must form a laminar family.
/// Equal scopes are expected for multiple explicit delimiters in one quote.
fn validate_ranges(markers: &[Marker], context: &Context<'_>) -> Option<()> {
    for marker in markers {
        if marker.range.start >= marker.range.end
            || marker.scope.start >= marker.scope.end
            || marker.range.start < marker.scope.start
            || marker.range.end > marker.scope.end
            || ![
                marker.range.start,
                marker.range.end,
                marker.scope.start,
                marker.scope.end,
            ]
            .into_iter()
            .all(|offset| context.boundary(offset))
        {
            return None;
        }
    }
    let mut scopes: Vec<_> = markers.iter().map(|m| m.scope.clone()).collect();
    scopes.sort_by_key(|scope| (scope.start, std::cmp::Reverse(scope.end)));
    scopes.dedup();
    let mut parents: Vec<Range<usize>> = Vec::new();
    for scope in scopes {
        while parents
            .last()
            .is_some_and(|parent| parent.end <= scope.start)
        {
            parents.pop();
        }
        if parents.last().is_some_and(|parent| scope.end > parent.end) {
            return None;
        }
        parents.push(scope);
    }
    Some(())
}

/// `- [ ]` of a bullet task item, exactly as written: bullet, whitespace,
/// `[`, one of ` xX`, `]`, then whitespace or the end of the line.
fn task_marker<'a>(node: &'a AstNode<'a>, context: &Context<'_>, checked: bool) -> Option<Marker> {
    let scope = context.range(node)?;
    let raw = context.source.get(scope.clone())?;
    let bytes = raw.as_bytes();
    if !matches!(bytes.first(), Some(b'-' | b'+' | b'*')) {
        return None;
    }
    let gap = bytes[1..]
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count();
    let open = 1 + gap;
    if gap == 0 || bytes.get(open) != Some(&b'[') || bytes.get(open + 2) != Some(&b']') {
        return None;
    }
    let mark = *bytes.get(open + 1)?;
    if !matches!(mark, b' ' | b'x' | b'X') || (mark != b' ') != checked {
        return None;
    }
    if bytes
        .get(open + 3)
        .is_some_and(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
    {
        return None;
    }
    let range = scope.start..scope.start + open + 3;
    Some(Marker {
        range: range.clone(),
        scope: range,
        kind: Kind::Task { checked },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        source_classifier::classify,
        source_projection::{MapError, Snapshot},
    };

    fn snapshot(text: &str, revision: u64) -> Snapshot {
        Snapshot::new("decorations", revision, text)
    }

    #[test]
    fn ast_distinguishes_markers_from_frontmatter_setext_and_fences() {
        let source = snapshot("---\ntitle: test\n---\n\nHeading\n---\n\n```md\n* raw\n> raw\n---\n```\n\n* actual\n\ntext\n\n***\n", 1);
        let classified = classify(&source);
        let markers = classified.decorations_for(&source).unwrap();
        assert_eq!(markers.len(), 3, "{markers:?}");
        assert_eq!(markers[0].kind, Kind::CodeBlock);
        assert_eq!(
            &source.source()[markers[0].range.clone()],
            "```md\n* raw\n> raw\n---\n```"
        );
        assert!(matches!(markers[1].kind, Kind::Unordered { depth: 1 }));
        assert_eq!(markers[2].kind, Kind::ThematicBreak);
    }

    #[test]
    fn nested_lists_quotes_and_task_controls() {
        let source = snapshot(
            "- one\n  - two\n    - three\n\n> quote\n> continuation\n\n1. ordered\n\n- [ ] task\n",
            1,
        );
        let classified = classify(&source);
        let markers = classified.decorations_for(&source).unwrap();
        assert_eq!(
            markers.iter().map(|m| m.kind.clone()).collect::<Vec<_>>(),
            vec![
                Kind::Unordered { depth: 1 },
                Kind::Unordered { depth: 2 },
                Kind::Unordered { depth: 3 },
                Kind::Quote { depth: 1 },
                Kind::Quote { depth: 1 },
                Kind::Task { checked: false },
            ]
        );
        for marker in markers {
            assert!(source.source().get(marker.range.clone()).is_some());
        }
    }

    #[test]
    fn task_markers_cover_bullet_and_box_and_reveal_only_inside() {
        let text = "- [ ] open\n* [x] done\n+\t[X] tab\n- [ ]\n1. [ ] ordered\n- [y] not a task\n";
        let source = snapshot(text, 1);
        let classified = classify(&source);
        let tasks: Vec<_> = classified
            .decorations_for(&source)
            .unwrap()
            .iter()
            .filter_map(|m| match m.kind {
                Kind::Task { checked } => {
                    Some((&text[m.range.clone()], checked, m.scope == m.range))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            tasks,
            [
                ("- [ ]", false, true),
                ("* [x]", true, true),
                ("+\t[X]", true, true),
                ("- [ ]", false, true)
            ]
        );
    }

    #[test]
    fn delayed_metadata_requires_exact_revision_and_bytes() {
        let source = snapshot("- проверка\r\n", 1);
        let classified = classify(&source);
        assert_eq!(classified.decorations_for(&source).unwrap().len(), 1);
        for stale in [snapshot("- проверка\r\n", 2), snapshot("- changed\r\n", 1)] {
            assert!(matches!(
                classified.decorations_for(&stale),
                Err(MapError::StaleSnapshot)
            ));
        }
    }
    #[test]
    fn metadata_on_off_preserves_projection_styles_reasons_and_source() {
        use crate::source_classifier::RetainedPresentation;
        use crate::source_projection::Active;
        let source = snapshot(
            "# Heading\n\n**bold** [[note|alias]]\n\n- list\n\n> quote\n\n***\n",
            1,
        );
        let on = classify(&source);
        assert!(!on.decorations_for(&source).unwrap().is_empty());
        let mut off = on.clone();
        off.decorations.clear();
        assert_eq!(format!("{:?}", on.plan()), format!("{:?}", off.plan()));
        assert_eq!(
            on.styles_for(&source).unwrap(),
            off.styles_for(&source).unwrap()
        );
        assert_eq!(on.reasons(), off.reasons());
        let a = RetainedPresentation::new(&on);
        let b = RetainedPresentation::new(&off);
        for offset in 0..=source.source().len() {
            let active = Active {
                selection: Some(offset..offset),
                composition: None,
            };
            let left = a.project(&active).unwrap();
            let right = b.project(&active).unwrap();
            assert_eq!(left.display(), right.display());
            for byte in 0..=source.source().len() {
                assert_eq!(
                    left.source_to_display(&source, byte),
                    right.source_to_display(&source, byte)
                );
            }
        }
        assert_eq!(
            source.copy_source(0..source.source().len()).unwrap(),
            source.source()
        );
    }

    #[test]
    fn ambiguity_and_existing_guards_return_empty_metadata() {
        for text in ["- x\0\n", "---\nunterminated\n- x\n"] {
            let source = snapshot(text, 1);
            assert!(classify(&source)
                .decorations_for(&source)
                .unwrap()
                .is_empty());
        }
        let text = "- x\n".repeat(super::super::MAX_BYTES / 4 + 1);
        let source = snapshot(&text, 1);
        assert!(classify(&source)
            .decorations_for(&source)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn quotes_in_list_items_reveal_per_marker_and_unsupported_quotes_stay_raw() {
        let text = "- > quote\n- a\n  - b\n    > deep **q**\n    > more\n    lazy\n\n- >\u{301}joined\n\n> top\n>\n";
        let source = snapshot(text, 1);
        let classified = classify(&source);
        let markers = classified.decorations_for(&source).unwrap();
        let quotes: Vec<_> = markers
            .iter()
            .filter(|m| matches!(m.kind, Kind::Quote { .. }))
            .map(|m| (&text[..m.range.start], &text[m.scope.clone()]))
            .map(|(before, scope)| (before.rsplit('\n').next().unwrap(), scope))
            .collect();
        // The quote joined to a combining mark is skipped alone; its bullet and
        // the other quotes keep their decorations.
        assert_eq!(
            quotes,
            [
                ("- ", "> "),
                ("    ", "> "),
                ("    ", "> "),
                ("", "> "),
                ("", ">")
            ]
        );
        let bullets = markers
            .iter()
            .filter(|m| matches!(m.kind, Kind::Unordered { .. }))
            .count();
        assert_eq!(bullets, 4);
    }

    #[test]
    fn quote_continuations_find_their_own_delimiter_near_the_first_column() {
        // (text, expected (offset, depth) per quote marker)
        for (text, expected) in [
            ("> a\n > b\n", vec![(0, 1), (5, 1)]),
            (" > a\n> b\n", vec![(1, 1), (5, 1)]),
            (">  > a\n> > b\n", vec![(0, 1), (3, 2), (7, 1), (9, 2)]),
            ("> > a\n>  > b\n", vec![(0, 1), (2, 2), (6, 1), (9, 2)]),
            // Line 2's only `>` belongs to the outer quote.
            ("> > a\n  > c\n", vec![(0, 1), (2, 2), (8, 1)]),
            (">>a\n", vec![(0, 1), (1, 2)]),
        ] {
            let source = snapshot(text, 1);
            let classified = classify(&source);
            let got: Vec<_> = classified
                .decorations_for(&source)
                .unwrap()
                .iter()
                .filter_map(|m| match m.kind {
                    Kind::Quote { depth } => Some((m.range.start, depth)),
                    _ => None,
                })
                .collect();
            assert_eq!(got, expected, "{text:?}");
        }
    }

    #[test]
    fn top_level_code_blocks_get_a_background_and_container_code_does_not() {
        let text = "para\n\n    indented\n    code\n    \n\n~~~\n**x**\n\n~~~\n\n- item\n\n      in list\n";
        let source = snapshot(text, 1);
        let classified = classify(&source);
        let blocks: Vec<_> = classified
            .decorations_for(&source)
            .unwrap()
            .iter()
            .filter(|m| m.kind == Kind::CodeBlock)
            .map(|m| &text[m.range.clone()])
            .collect();
        // Trailing whitespace-only lines of an indented block are not code;
        // blank lines inside a fence are.
        assert_eq!(blocks, ["    indented\n    code", "~~~\n**x**\n\n~~~"]);
    }

    #[test]
    fn nested_quote_delimiters_use_ast_scopes_and_keep_lazy_content() {
        let source = snapshot(
            "> outer\r\n> > inner\r\n> >続き\r\n\r\n> lazy\ncontinuation\n",
            1,
        );
        let classified = classify(&source);
        let markers = classified.decorations_for(&source).unwrap();
        assert!(!markers.is_empty());
        assert_eq!(
            markers
                .iter()
                .filter(|m| m.kind == Kind::Quote { depth: 2 })
                .count(),
            2
        );
        for marker in markers {
            assert_eq!(&source.source()[marker.range.clone()], ">");
            assert!(
                marker.scope.start <= marker.range.start && marker.range.end <= marker.scope.end
            );
        }
    }
    #[test]
    fn marker_and_scope_endpoints_require_graphemes_and_containment() {
        let context = Context::new("a\u{301}bcdef");
        let valid = Marker {
            range: 3..4,
            scope: 0..8,
            kind: Kind::ThematicBreak,
        };
        assert!(validate_ranges(std::slice::from_ref(&valid), &context).is_some());
        for (range, scope) in [
            (1..3, 0..8),
            (0..1, 0..8),
            (3..4, 1..8),
            (3..4, 0..1),
            (3..4, 4..8),
            (3..9, 0..9),
            (4..4, 0..8),
        ] {
            let invalid = Marker {
                range,
                scope,
                ..valid.clone()
            };
            assert!(validate_ranges(&[invalid], &context).is_none());
        }
    }

    #[test]
    fn scopes_allow_nesting_equality_and_siblings_but_not_crossing() {
        let context = Context::new("0123456789");
        let marker = |range, scope| Marker {
            range,
            scope,
            kind: Kind::Quote { depth: 1 },
        };
        assert!(validate_ranges(
            &[
                marker(0..1, 0..10),
                marker(2..3, 2..5),
                marker(3..4, 2..5),
                marker(7..8, 7..9)
            ],
            &context
        )
        .is_some());
        assert!(validate_ranges(&[marker(0..1, 0..6), marker(4..5, 4..8)], &context).is_none());
    }
    #[test]
    fn quote_marker_joined_to_combining_mark_stays_raw() {
        let valid = snapshot("> quote\n", 1);
        assert_eq!(classify(&valid).decorations_for(&valid).unwrap().len(), 1);
        let joined = snapshot(">\u{301}quote\n", 1);
        // Only the joined quote stays raw.
        assert!(classify(&joined)
            .decorations_for(&joined)
            .unwrap()
            .is_empty());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn canonical_save_roundtrip_preserves_authored_markers_and_line_endings() {
        use crate::file_editor::FileEditor;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("markers.md");
        let source = "- проверка\r\n  - nested\r\n\r\n> quote\r\n\r\n***\r\n";
        std::fs::write(&path, source).unwrap();
        let current = snapshot(source, 1);
        assert!(!classify(&current)
            .decorations_for(&current)
            .unwrap()
            .is_empty());
        let mut editor = FileEditor::open(&path, &root.path().join("state")).unwrap();
        let edited = format!("{source}positive control\r\n");
        editor.set_text(edited.clone()).unwrap();
        editor.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), edited.as_bytes());
        editor
            .set_text(current.copy_source(0..source.len()).unwrap())
            .unwrap();
        editor.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), source.as_bytes());
    }
}
