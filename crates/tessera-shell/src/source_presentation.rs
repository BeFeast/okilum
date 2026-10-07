//! App-owned raw classifier bridge: classify off-thread, compose reviewed source mappings.
use std::{ops::Range, sync::Arc};

use gpui_component::input::projection::{
    ActiveSource, ConcealBias, ProjectedByte, ProjectionProvider, ProjectionStyle, SourceByte,
    SourceProjection, SourceSnapshot,
};
use tessera_core::{
    source_classifier::{self, Classification, Style},
    source_projection::{self, Active, Bias, Projection, Snapshot},
};

pub const BODY_FONT: &str = "Noto Sans";
pub const CODE_FONT: &str = "Cascadia Code";

pub struct CachedProvider {
    source: SourceSnapshot,
    classified: Classification,
    links: Vec<source_classifier::NoteLink>,
}

impl CachedProvider {
    /// Call only from a background classification task.
    pub fn classify(source: SourceSnapshot) -> Self {
        let snapshot = core_snapshot(&source);
        let links = if source.text.len() <= source_classifier::MAX_BYTES {
            tessera_core::document_links::parse(&source.text)
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
        Self {
            source,
            classified: source_classifier::classify(&snapshot),
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
        if self.source.stamp != source.stamp || self.source.text != source.text {
            return None;
        }
        // #214's stricter cap is a presentation limit, not a buffer limit.
        if source.text.len() > source_classifier::MAX_BYTES {
            return None;
        }
        let snapshot = self.classified.snapshot();
        let selection = active.anchor.0.min(active.head.0)..active.anchor.0.max(active.head.0);
        let mut reveal = Active {
            selection: Some(selection),
            composition: active.composition.as_ref().map(raw_range),
        };
        // Independently validate exact native ranges before taking their union.
        // An enclosing union must never hide an invalid/subgrapheme endpoint.
        let mut projection =
            source_projection::project(snapshot, self.classified.plan(), &reveal).ok()?;
        if let Some(replacement) = active.replacement.as_ref().map(raw_range) {
            source_projection::project(
                snapshot,
                self.classified.plan(),
                &Active {
                    selection: Some(replacement.clone()),
                    composition: None,
                },
            )
            .ok()?;
            reveal.composition = Some(match reveal.composition {
                Some(composition) => {
                    composition.start.min(replacement.start)..composition.end.max(replacement.end)
                }
                None => replacement,
            });
            projection =
                source_projection::project(snapshot, self.classified.plan(), &reveal).ok()?;
        }
        let styles = projected_styles(&projection, self.classified.styles_for(snapshot).ok()?)?;
        Some(Arc::new(MappedProjection {
            source: source.clone(),
            projection,
            styles,
        }))
    }
}

struct MappedProjection {
    source: SourceSnapshot,
    projection: Projection,
    styles: Vec<ProjectionStyle>,
}
impl SourceProjection for MappedProjection {
    fn source(&self) -> &SourceSnapshot {
        &self.source
    }
    fn text(&self) -> &str {
        self.projection.display()
    }
    fn styles(&self) -> &[ProjectionStyle] {
        &self.styles
    }
    fn to_projected(&self, source: SourceByte, _bias: ConcealBias) -> Option<ProjectedByte> {
        self.projection
            .source_to_display(self.projection.snapshot(), source.0)
            .ok()
            .map(ProjectedByte)
    }
    fn to_source(&self, projected: ProjectedByte, bias: ConcealBias) -> Option<SourceByte> {
        let bias = match bias {
            ConcealBias::Left => Bias::Left,
            ConcealBias::Right => Bias::Right,
        };
        self.projection
            .display_to_source(self.projection.snapshot(), projected.0, bias)
            .ok()
            .map(SourceByte)
    }
}

/// Sweep canonical nested style intervals into disjoint projected metric runs.
fn projected_styles(
    projection: &Projection,
    styles: &[source_classifier::StyleSpan],
) -> Option<Vec<ProjectionStyle>> {
    let mut events = Vec::new();
    for style in styles {
        let slot = match style.style {
            Style::Heading(_) | Style::Strong => 0,
            Style::Emphasis => 1,
            Style::Strike => 2,
            Style::Code => 3,
            // Link labels are concealed, but this generic metric API has no
            // color/underline field. Do not invent metric-changing link styles.
            Style::Link | Style::WikiLink => continue,
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
    let mut counts = [0i32; 4];
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
                bold: counts[0] > 0,
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
    fn every_stale_identity_falls_back() {
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
                    generation: 10,
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
