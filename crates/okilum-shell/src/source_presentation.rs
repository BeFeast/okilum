//! App-owned raw classifier bridge: classify off-thread, compose reviewed source mappings.
use std::{
    ops::Range,
    sync::{Arc, Mutex},
};

use gpui_component::input::projection::{
    ActiveSource, ConcealBias, MarkerKind, ProjectedByte, ProjectionMarker, ProjectionProvider,
    ProjectionStyle, SourceByte, SourceProjection, SourceSnapshot,
};
use okilum_core::{
    source_classifier::{self, Classification, RetainedPresentation, Style},
    source_projection::{Active, Bias, Projection, Snapshot},
};

pub const BODY_FONT: &str = "Noto Sans";
pub const CODE_FONT: &str = "Cascadia Code";

/// App theme colors; they do not participate in parsing or source revisions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectionColors {
    pub heading: gpui::Hsla,
    pub link: gpui::Hsla,
    /// Quiet rows such as reference definitions.
    pub muted: gpui::Hsla,
}

// The standalone projection fixture imports this module without app theme colors.
#[allow(dead_code)]
struct ColoredProvider {
    provider: Arc<CachedProvider>,
    colors: ProjectionColors,
}
impl ProjectionProvider for ColoredProvider {
    fn compose(
        &self,
        source: &SourceSnapshot,
        active: &ActiveSource,
    ) -> Option<Arc<dyn SourceProjection>> {
        self.provider
            .compose_colored(source, active, Some(self.colors))
    }
}

pub struct CachedProvider {
    source: SourceSnapshot,
    classified: Classification,
    retained: Mutex<RetainedPresentation>,
    links: Vec<source_classifier::NoteLink>,
}

impl CachedProvider {
    /// Reuse the classification when the theme changes; never parse on the UI thread.
    // The standalone projection fixture deliberately uses the unthemed provider.
    #[allow(dead_code)]
    pub fn with_colors(self: Arc<Self>, colors: ProjectionColors) -> Arc<dyn ProjectionProvider> {
        Arc::new(ColoredProvider {
            provider: self,
            colors,
        })
    }
    /// Call only from a background classification task.
    pub fn classify(source: SourceSnapshot) -> Self {
        let snapshot = core_snapshot(&source);
        let links = if source.text.len() <= source_classifier::MAX_BYTES {
            okilum_core::document_links::parse(&source.text)
                .into_iter()
                .map(|link| source_classifier::NoteLink {
                    label: link.range.clone(),
                    range: link.range,
                    target: link.target,
                    wiki: link.wiki,
                })
                .collect()
        } else {
            Vec::new()
        };
        let classified = source_classifier::classify(&snapshot);
        Self {
            source,
            retained: Mutex::new(RetainedPresentation::new(&classified)),
            classified,
            links,
        }
    }
    // Shared with native_projection216, which only consumes presentation data.
    #[allow(dead_code)]
    pub fn note_links(&self) -> &[source_classifier::NoteLink] {
        &self.links
    }
    // The isolated native example has no limit-status control.
    #[allow(dead_code)]
    pub fn limited(&self) -> bool {
        self.classified.reasons().iter().any(|reason| {
            matches!(
                reason,
                source_classifier::SourceReason::InputLimit
                    | source_classifier::SourceReason::StructureLimit
            )
        })
    }
    pub fn source(&self) -> &SourceSnapshot {
        &self.source
    }
    // Used by the isolated native evidence fixture.
    #[allow(dead_code)]
    pub fn reasons(&self) -> String {
        format!("{:?}", self.classified.reasons())
    }
}

fn core_snapshot(source: &SourceSnapshot) -> Snapshot {
    Snapshot::new(
        source.stamp.document.to_string(),
        source.stamp.generation,
        source.text.clone(),
    )
}
fn raw_range(range: &Range<SourceByte>) -> Range<usize> {
    range.start.0..range.end.0
}

impl ProjectionProvider for CachedProvider {
    fn compose(
        &self,
        source: &SourceSnapshot,
        active: &ActiveSource,
    ) -> Option<Arc<dyn SourceProjection>> {
        self.compose_colored(source, active, None)
    }
}
impl CachedProvider {
    fn compose_colored(
        &self,
        source: &SourceSnapshot,
        active: &ActiveSource,
        colors: Option<ProjectionColors>,
    ) -> Option<Arc<dyn SourceProjection>> {
        // #214's stricter cap is a presentation limit, not a buffer limit.
        if source.text.len() > source_classifier::MAX_BYTES {
            return None;
        }
        let current = core_snapshot(source);
        let mut retained = self.retained.lock().ok()?;
        if retained.snapshot() != &current {
            *retained = retained.remap(&current)?;
        }
        let selection = active.anchor.0.min(active.head.0)..active.anchor.0.max(active.head.0);
        let mut reveal = Active {
            selection: Some(selection),
            composition: active.composition.as_ref().map(raw_range),
        };
        // Independently validate exact native ranges before taking their union.
        // An enclosing union must never hide an invalid/subgrapheme endpoint.
        retained.validate_active(&reveal).ok()?;
        if let Some(replacement) = active.replacement.as_ref().map(raw_range) {
            retained
                .validate_active(&Active {
                    selection: Some(replacement.clone()),
                    composition: None,
                })
                .ok()?;
            reveal.composition = Some(match reveal.composition {
                Some(composition) => {
                    composition.start.min(replacement.start)..composition.end.max(replacement.end)
                }
                None => replacement,
            });
        }
        let reveal_snapshot = retained.prepare_reveal(&reveal).ok()?;
        let projection = reveal_snapshot.projection().clone();
        let styles = projected_styles(&projection, retained.styles(), colors)?;
        Some(Arc::new(MappedProjection {
            source: source.clone(),
            markers: self
                .classified
                .decorations_for(&current)
                .unwrap_or_default()
                .iter()
                .map(|m| ProjectionMarker {
                    range: SourceByte(m.range.start)..SourceByte(m.range.end),
                    scope: SourceByte(m.scope.start)..SourceByte(m.scope.end),
                    kind: match m.kind {
                        source_classifier::decorations::Kind::Unordered { depth } => {
                            MarkerKind::Bullet { depth }
                        }
                        source_classifier::decorations::Kind::Quote { depth } => {
                            MarkerKind::Quote { depth }
                        }
                        source_classifier::decorations::Kind::ThematicBreak => MarkerKind::Rule,
                        source_classifier::decorations::Kind::CodeBlock => MarkerKind::CodeBlock,
                    },
                })
                .collect(),
            reveal: reveal_snapshot,
            styles,
        }))
    }
}

struct MappedProjection {
    markers: Vec<ProjectionMarker>,
    source: SourceSnapshot,
    reveal: source_classifier::RevealSnapshot,
    styles: Vec<ProjectionStyle>,
}
impl SourceProjection for MappedProjection {
    fn markers(&self) -> &[ProjectionMarker] {
        &self.markers
    }
    fn marker_scope_is_raw(&self, scope: &Range<SourceByte>) -> bool {
        self.reveal
            .is_raw(self.reveal.projection().snapshot(), &raw_range(scope))
    }

    fn source(&self) -> &SourceSnapshot {
        &self.source
    }
    fn text(&self) -> &str {
        self.reveal.projection().display()
    }
    fn styles(&self) -> &[ProjectionStyle] {
        &self.styles
    }
    fn to_projected(&self, source: SourceByte, _bias: ConcealBias) -> Option<ProjectedByte> {
        self.reveal
            .projection()
            .source_to_display(self.reveal.projection().snapshot(), source.0)
            .ok()
            .map(ProjectedByte)
    }
    fn to_source(&self, projected: ProjectedByte, bias: ConcealBias) -> Option<SourceByte> {
        let bias = match bias {
            ConcealBias::Left => Bias::Left,
            ConcealBias::Right => Bias::Right,
        };
        self.reveal
            .projection()
            .display_to_source(self.reveal.projection().snapshot(), projected.0, bias)
            .ok()
            .map(SourceByte)
    }
}

/// Sweep canonical nested style intervals into disjoint projected metric runs.
fn projected_styles(
    projection: &Projection,
    styles: &[source_classifier::StyleSpan],
    colors: Option<ProjectionColors>,
) -> Option<Vec<ProjectionStyle>> {
    let mut events = Vec::new();
    for style in styles {
        let slot = match style.style {
            Style::Strong => 0,
            Style::Heading(_) => 4,
            Style::Emphasis => 1,
            Style::Strike => 2,
            Style::Code => 3,
            Style::Link | Style::WikiLink => 5,
            Style::Definition => 6,
        };
        let start = projection
            .source_to_display(projection.snapshot(), style.range.start)
            .ok()?;
        let end = projection
            .source_to_display(projection.snapshot(), style.range.end)
            .ok()?;
        if start < end {
            events.extend([(start, slot, 1i32), (end, slot, -1)]);
        }
    }
    events.sort_unstable();
    let mut counts = [0i32; 7];
    let mut runs = Vec::new();
    let mut index = 0;
    while index < events.len() {
        let start = events[index].0;
        while index < events.len() && events[index].0 == start {
            counts[events[index].1] += events[index].2;
            index += 1;
        }
        if counts.iter().any(|&n| n < 0) {
            return None;
        }
        let Some(&(end, _, _)) = events.get(index) else {
            break;
        };
        if start < end && counts.iter().any(|&n| n != 0) {
            runs.push(ProjectionStyle {
                range: ProjectedByte(start)..ProjectedByte(end),
                font_family: (counts[3] > 0).then(|| CODE_FONT.to_owned()),
                color: colors.and_then(|colors| {
                    // Links retain their affordance inside bold headings.
                    if counts[5] > 0 {
                        Some(colors.link)
                    } else if counts[4] > 0 {
                        Some(colors.heading)
                    } else if counts[6] > 0 {
                        Some(colors.muted)
                    } else {
                        None
                    }
                }),
                bold: counts[0] > 0 || counts[4] > 0,
                italic: counts[1] > 0,
                strikethrough: counts[2] > 0,
            });
        }
    }
    Some(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::input::projection::SourceStamp;
    fn source(text: &str) -> SourceSnapshot {
        SourceSnapshot {
            stamp: SourceStamp {
                document: 5,
                generation: 9,
            },
            text: Arc::from(text),
        }
    }
    fn inactive(source: &SourceSnapshot) -> ActiveSource {
        ActiveSource {
            anchor: SourceByte(source.text.len()),
            head: SourceByte(source.text.len()),
            ..ActiveSource::default()
        }
    }
    #[test]
    fn decoration_inventory_is_exact_revision_only_and_pinned() {
        let source = source("- list\n\n> quote\n\n***\n\nend");
        let provider = CachedProvider::classify(source.clone());
        let idle = inactive(&source);
        let projection = provider.compose(&source, &idle).unwrap();
        assert_eq!(projection.markers().len(), 3);
        assert_eq!(projection.text(), source.text.as_ref());
        let old_ranges: Vec<_> = projection
            .markers()
            .iter()
            .map(|m| m.range.clone())
            .collect();
        for change_bytes in [false, true] {
            let mut current = source.clone();
            current.stamp.generation += 1;
            if change_bytes {
                current.text = Arc::from("- list\n\n> quote\n\n***\n\nended");
            }
            if let Some(updated) = provider.compose(&current, &inactive(&current)) {
                assert!(
                    updated.markers().is_empty(),
                    "retained projection must not remap decoration metadata"
                );
            }
            assert_eq!(
                projection
                    .markers()
                    .iter()
                    .map(|m| m.range.clone())
                    .collect::<Vec<_>>(),
                old_ranges
            );
        }
    }

    #[test]
    fn quote_markers_reveal_as_their_own_element_not_the_whole_quote() {
        use gpui_component::input::projection::{LayoutStamp, MarkerKind, PinnedProjection};
        let source = source("> first **line**\n> second line\n");
        let provider = CachedProvider::classify(source.clone());
        let stamp = LayoutStamp {
            source: source.stamp,
            presentation: 1,
        };
        let raw_quotes = |caret: usize| {
            let active = ActiveSource {
                anchor: SourceByte(caret),
                head: SourceByte(caret),
                ..ActiveSource::default()
            };
            let pin = PinnedProjection {
                stamp,
                projection: provider.compose(&source, &active).unwrap(),
            };
            pin.projection
                .markers()
                .iter()
                .filter(|m| matches!(m.kind, MarkerKind::Quote { .. }))
                .map(|m| pin.marker_scope_is_raw(stamp, &source, &m.scope, &active))
                .collect::<Vec<_>>()
        };
        let second = source.text.find("> second").unwrap();
        // A caret inside quote text reveals no delimiter, even in a long quote.
        assert_eq!(raw_quotes(second + 6), [false, false]);
        // At its own `> ` element only that delimiter is revealed.
        assert_eq!(raw_quotes(second), [false, true]);
        assert_eq!(raw_quotes(second + 2), [false, true]);
        assert_eq!(raw_quotes(0), [true, false]);
    }

    #[test]
    fn pinned_marker_policy_uses_its_projection_and_only_allows_raw_safety_override() {
        use gpui_component::input::projection::{LayoutStamp, PinnedProjection};
        let source = source("[label](destination) tail e\u{301}");
        let provider = CachedProvider::classify(source.clone());
        let idle = inactive(&source);
        let stamp = LayoutStamp {
            source: source.stamp,
            presentation: 17,
        };
        let pin = PinnedProjection {
            stamp,
            projection: provider.compose(&source, &idle).unwrap(),
        };
        let scope = SourceByte(0)..SourceByte(20);
        let text = pin.projection.text().to_owned();
        assert!(!pin.marker_scope_is_raw(stamp, &source, &scope, &idle));
        let active = ActiveSource {
            anchor: SourceByte(2),
            head: SourceByte(3),
            ..idle.clone()
        };
        assert!(pin.marker_scope_is_raw(stamp, &source, &scope, &active));
        for safety in [
            ActiveSource {
                composition: Some(SourceByte(2)..SourceByte(3)),
                ..idle.clone()
            },
            ActiveSource {
                replacement: Some(SourceByte(2)..SourceByte(3)),
                ..idle.clone()
            },
            ActiveSource {
                composition: Some(SourceByte(usize::MAX)..SourceByte(usize::MAX)),
                ..idle.clone()
            },
        ] {
            assert!(pin.marker_scope_is_raw(stamp, &source, &scope, &safety));
        }
        let next = provider.compose(&source, &active).unwrap();
        assert_ne!(next.text(), text);
        // Adoption elsewhere cannot replace this frame's policy or hit map.
        assert_eq!(pin.projection.text(), text);
        assert!(!pin.marker_scope_is_raw(stamp, &source, &scope, &idle));
        assert!(pin.marker_scope_is_raw(
            LayoutStamp {
                presentation: 18,
                ..stamp
            },
            &source,
            &scope,
            &idle
        ));
        let mut stale = source.clone();
        stale.text = Arc::from("same stamp but wrong source");
        assert!(pin.marker_scope_is_raw(stamp, &stale, &scope, &idle));
        stale = source.clone();
        stale.stamp.generation += 1;
        assert!(pin.marker_scope_is_raw(stamp, &stale, &scope, &idle));
        let inside_grapheme = SourceByte(source.text.len() - 2)..SourceByte(source.text.len());
        assert!(pin.marker_scope_is_raw(stamp, &source, &inside_grapheme, &idle));
    }

    #[test]
    fn cached_projection_maps_raw_source_and_nested_styles() {
        let source = source("***nested*** and `code`\n\nend");
        let provider = CachedProvider::classify(source.clone());
        let projection = provider.compose(&source, &inactive(&source)).unwrap();
        assert_eq!(projection.text(), "nested and code\n\nend");
        assert_eq!(
            projection.styles()[0].range,
            ProjectedByte(0)..ProjectedByte(6)
        );
        assert!(projection.styles()[0].bold && projection.styles()[0].italic);
        assert_eq!(
            projection.styles()[1].font_family.as_deref(),
            Some(CODE_FONT)
        );
        assert_eq!(
            projection.to_source(ProjectedByte(0), ConcealBias::Left),
            Some(SourceByte(0))
        );
        assert_eq!(
            projection.to_source(ProjectedByte(0), ConcealBias::Right),
            Some(SourceByte(3))
        );
    }
    #[test]
    fn themed_links_and_headings_preserve_exact_mapping_and_nested_weight() {
        let source = source("# [Heading](https://example.com)\n\n**[web](https://example.com)** and [[Note|wiki]]\n\nend");
        let provider = Arc::new(CachedProvider::classify(source.clone()));
        let active = inactive(&source);
        let plain = provider.compose(&source, &active).unwrap();
        for colors in [
            ProjectionColors {
                heading: gpui::rgb(0x18202a).into(),
                link: gpui::rgb(0x005ca8).into(),
                muted: gpui::rgb(0x6b7280).into(),
            },
            ProjectionColors {
                heading: gpui::rgb(0xf2f4f8).into(),
                link: gpui::rgb(0x8dc8ff).into(),
                muted: gpui::rgb(0x6b7280).into(),
            },
        ] {
            let projection = provider
                .clone()
                .with_colors(colors)
                .compose(&source, &active)
                .unwrap();
            assert_eq!(projection.text(), plain.text());
            assert_eq!(projection.source().text.as_ref(), source.text.as_ref());
            for label in ["Heading", "web", "wiki"] {
                let offset = projection.text().find(label).unwrap();
                let style = projection
                    .styles()
                    .iter()
                    .find(|style| style.range.start.0 <= offset && offset < style.range.end.0)
                    .unwrap();
                assert_eq!(style.color, Some(colors.link));
                if label != "wiki" {
                    assert!(style.bold);
                }
            }
            for (offset, _) in projection.text().char_indices() {
                for bias in [ConcealBias::Left, ConcealBias::Right] {
                    assert_eq!(
                        projection.to_source(ProjectedByte(offset), bias),
                        plain.to_source(ProjectedByte(offset), bias)
                    );
                }
            }
        }
        let source = self::source("# Plain heading\n\nend");
        let colors = ProjectionColors {
            heading: gpui::rgb(0x18202a).into(),
            link: gpui::rgb(0x005ca8).into(),
            muted: gpui::rgb(0x6b7280).into(),
        };
        let projection = Arc::new(CachedProvider::classify(source.clone()))
            .with_colors(colors)
            .compose(&source, &inactive(&source))
            .unwrap();
        assert!(projection
            .styles()
            .iter()
            .any(|style| style.bold && style.color == Some(colors.heading)));
    }

    #[test]
    fn reference_definitions_keep_their_rows_and_render_muted() {
        let source = source("- see [docs][Id]\n\n[Id]: https://example.com\n");
        let colors = ProjectionColors {
            heading: gpui::rgb(0x18202a).into(),
            link: gpui::rgb(0x005ca8).into(),
            muted: gpui::rgb(0x6b7280).into(),
        };
        let projection = Arc::new(CachedProvider::classify(source.clone()))
            .with_colors(colors)
            .compose(&source, &inactive(&source))
            .unwrap();
        assert_eq!(
            projection.text(),
            "- see docs\n\n[Id]: https://example.com\n"
        );
        let color_at = |label: &str| {
            let offset = projection.text().find(label).unwrap();
            projection
                .styles()
                .iter()
                .find(|style| style.range.start.0 <= offset && offset < style.range.end.0)
                .and_then(|style| style.color)
        };
        assert_eq!(color_at("docs"), Some(colors.link));
        assert_eq!(color_at("[Id]:"), Some(colors.muted));
    }

    #[test]
    fn themed_native_fixture_projects_without_fallback() {
        let source = source(
            r###"# A quiet place to write

Okilum keeps **the exact Markdown** while showing *readable formatting*.

## Links and code

Visit [[Target|another note]] or [the project](https://github.com/BeFeast/okilum).
Keep `inline code`, ~~finished thoughts~~ and Unicode: Привет 🧠 é.

## Plain files, safe edits

Selection, copy and Undo use your original text. Source is one click away.

[This intentionally long link label stays blue while wrapping across the editor width, so the second line must preserve the same color and selection behavior](https://example.com). Ordinary text follows.

---

This paragraph remains ordinary Markdown.
"###,
        );
        let provider = Arc::new(CachedProvider::classify(source.clone()));
        let projection = provider
            .with_colors(ProjectionColors {
                heading: gpui::black(),
                link: gpui::rgb(0x005ca8).into(),
                muted: gpui::rgb(0x6b7280).into(),
            })
            .compose(&source, &inactive(&source))
            .expect("native fixture must project");
        assert!(!projection.text().contains("[[Target|"));
        assert!(!projection.text().contains("**the exact Markdown**"));
    }

    #[test]
    fn foreign_or_inconsistent_identity_falls_back() {
        let source = source("**bold**\n\nend");
        let provider = CachedProvider::classify(source.clone());
        for changed in [
            SourceSnapshot {
                stamp: SourceStamp {
                    document: 6,
                    ..source.stamp
                },
                ..source.clone()
            },
            SourceSnapshot {
                stamp: SourceStamp {
                    generation: 8,
                    ..source.stamp
                },
                ..source.clone()
            },
            SourceSnapshot {
                text: Arc::from("**other**\n\nend"),
                ..source.clone()
            },
        ] {
            assert!(provider.compose(&changed, &inactive(&changed)).is_none());
        }
    }
    #[test]
    fn edits_keep_unchanged_projection_until_background_adoption() {
        let original = source("TOP text\n\n**bold** [label](destination)\n\nend");
        let provider = CachedProvider::classify(original.clone());
        for step in 1..=100 {
            let changed = SourceSnapshot {
                stamp: SourceStamp {
                    generation: original.stamp.generation + step,
                    ..original.stamp
                },
                text: Arc::from(
                    original
                        .text
                        .replace("TOP text", &format!("TOP текст{step}")),
                ),
            };
            let projection = provider.compose(&changed, &inactive(&changed)).unwrap();
            assert!(projection.text().contains("bold label"));
            assert_eq!(projection.source().stamp, changed.stamp);
            assert_eq!(projection.source().text, changed.text);
            let fresh = CachedProvider::classify(changed.clone());
            assert_eq!(
                projection.text(),
                fresh.compose(&changed, &inactive(&changed)).unwrap().text()
            );
        }
    }

    #[test]
    fn explicit_replacement_reveals_without_changing_selection() {
        let source = source("**first**\n\n**second**\n\nend");
        let provider = CachedProvider::classify(source.clone());
        let active = ActiveSource {
            replacement: Some(SourceByte(3)..SourceByte(4)),
            ..inactive(&source)
        };
        let projection = provider.compose(&source, &active).unwrap();
        assert_eq!(projection.text(), "**first**\n\nsecond\n\nend");
        assert_eq!(active.anchor, SourceByte(source.text.len()));
    }
    #[test]
    fn subgrapheme_composition_cannot_be_hidden_by_replacement_union() {
        let source = source("**e\u{301}**\n\nend");
        let provider = CachedProvider::classify(source.clone());
        let active = ActiveSource {
            composition: Some(SourceByte(3)..SourceByte(5)),
            replacement: Some(SourceByte(0)..SourceByte(7)),
            ..inactive(&source)
        };
        assert!(provider.compose(&source, &active).is_none());
    }
    #[test]
    fn larger_input_is_full_source_fallback() {
        let source = source(&"x".repeat(source_classifier::MAX_BYTES + 1));
        let provider = CachedProvider::classify(source.clone());
        assert!(provider.compose(&source, &inactive(&source)).is_none());
        assert_eq!(source.text.len(), source_classifier::MAX_BYTES + 1);
    }
}
