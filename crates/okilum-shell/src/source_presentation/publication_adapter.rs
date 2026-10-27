//! Explicitly constructed, UNSELECTED current-root adapter. No production factory
//! or Reader/Brain scheduler calls this constructor. It does not implement the
//! legacy retained-remap/raw-tail or note-link navigation activation contracts.
use super::*;
use gpui_component::input::projection::WrapAffinity;
use okilum_core::source_classifier::presentation::{
    Affinity, Context, NativeActive, Publication, PublicationId, View,
};

#[cfg(test)]
thread_local! {
    static PREPARED_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn core_active(active: &ActiveSource) -> NativeActive {
    NativeActive {
        anchor: active.anchor.0,
        head: active.head.0,
        composition: active.composition.as_ref().map(raw_range),
        replacement: active.replacement.as_ref().map(raw_range),
        affinity: match active.affinity {
            WrapAffinity::Start => Affinity::Start,
            WrapAffinity::End => Affinity::End,
        },
    }
}

/// Owns an already prepared immutable trait result. Construction must occur
/// AFTER explicit root/publication compute/adopt, outside the native compose path.
pub(crate) struct PreparedProvider {
    active: ActiveSource,
    projection: Arc<PreparedProjection>,
}
impl PreparedProvider {
    pub(crate) fn new(
        source: SourceSnapshot,
        active: ActiveSource,
        publication: Publication,
        identity: PublicationId,
        colors: Option<ProjectionColors>,
    ) -> Option<Self> {
        let snapshot = core_snapshot(&source);
        let native = core_active(&active);
        let view = publication
            .view(&snapshot, Context::LivePreview, &native, &identity)
            .ok()?;
        let text = Arc::from(view.display());
        let styles = projected_styles(&view, view.styles(), colors)?;
        let line_scales = heading_line_scales(&view, view.styles());
        let markers = projection_markers(view.decorations());
        let tables = view
            .decorations()
            .iter()
            .filter(|m| is_block(&m.kind))
            .map(|m| m.range.clone())
            .collect();
        let blocks = table_blocks(
            &source.text,
            tables,
            std::iter::once(native.anchor.min(native.head)..native.anchor.max(native.head))
                .chain(native.composition.clone())
                .chain(native.replacement.clone()),
        );
        #[cfg(test)]
        PREPARED_BUILDS.with(|v| v.set(v.get() + 1));
        Some(Self {
            active,
            projection: Arc::new(PreparedProjection {
                source,
                snapshot,
                active: native,
                publication,
                identity,
                text,
                styles,
                line_scales,
                markers,
                blocks,
            }),
        })
    }
}
impl ProjectionProvider for PreparedProvider {
    fn compose(
        &self,
        source: &SourceSnapshot,
        active: &ActiveSource,
    ) -> Option<Arc<dyn SourceProjection>> {
        // No parse, lookup, authority creation, reveal preparation or indexing.
        // Epoch is owned separately by native ProjectionSession::compose/pin.
        (source.stamp == self.projection.source.stamp
            && (Arc::ptr_eq(&source.text, &self.projection.source.text)
                || source.text == self.projection.source.text)
            && active == &self.active)
            .then(|| self.projection.clone() as Arc<dyn SourceProjection>)
    }
}

struct PreparedProjection {
    source: SourceSnapshot,
    snapshot: Snapshot,
    active: NativeActive,
    publication: Publication,
    identity: PublicationId,
    text: Arc<str>,
    styles: Vec<ProjectionStyle>,
    line_scales: Vec<LineScale>,
    markers: Vec<ProjectionMarker>,
    blocks: Vec<ProjectionBlock>,
}
impl PreparedProjection {
    fn view(&self) -> Option<View<'_>> {
        self.publication
            .view(
                &self.snapshot,
                Context::LivePreview,
                &self.active,
                &self.identity,
            )
            .ok()
    }
}
impl SourceProjection for PreparedProjection {
    fn source(&self) -> &SourceSnapshot {
        &self.source
    }
    fn text(&self) -> &str {
        &self.text
    }
    fn styles(&self) -> &[ProjectionStyle] {
        &self.styles
    }
    fn line_scales(&self) -> &[LineScale] {
        &self.line_scales
    }
    fn markers(&self) -> &[ProjectionMarker] {
        &self.markers
    }
    fn blocks(&self) -> &[ProjectionBlock] {
        &self.blocks
    }
    fn source_boundary(&self, current: &SourceSnapshot, offset: SourceByte) -> Option<bool> {
        if current.stamp != self.source.stamp
            || (!Arc::ptr_eq(&current.text, &self.source.text) && current.text != self.source.text)
        {
            return None;
        }
        match self.view()?.source_to_display(offset.0) {
            Ok(_) => Some(true),
            Err(okilum_core::source_projection::MapError::InvalidBoundary) => Some(false),
            Err(_) => None,
        }
    }
    fn to_projected(&self, source: SourceByte, _bias: ConcealBias) -> Option<ProjectedByte> {
        self.view()?
            .source_to_display(source.0)
            .ok()
            .map(ProjectedByte)
    }
    fn to_source(&self, projected: ProjectedByte, bias: ConcealBias) -> Option<SourceByte> {
        let bias = match bias {
            ConcealBias::Left => Bias::Left,
            ConcealBias::Right => Bias::Right,
        };
        self.view()?
            .display_to_source(projected.0, bias)
            .ok()
            .map(SourceByte)
    }
    fn marker_scope_is_raw(&self, scope: &Range<SourceByte>) -> bool {
        self.view()
            .and_then(|v| v.scope_is_raw(raw_range(scope)).ok())
            .unwrap_or(true)
    }
}

#[cfg(test)]
#[path = "publication_adapter/tests.rs"]
mod tests;
