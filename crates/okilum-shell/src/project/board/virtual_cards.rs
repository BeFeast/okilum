//! Window-local card height hints keep the Board's single scroll surface and
//! natural lane flow. Only viewport cards (plus overdraw) build controls/text.
//! Canonical cards, selection, source refresh and actions stay in Board.
use super::*;
use gpui::{
    AvailableSpace, ContentMask, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId,
    Style,
};

pub(super) struct Height(Pixels);
enum Source {
    Issue(okilum_t3::board::Scope, Column, Arc<Binding>),
    Local(okilum_t3::board::local::Card),
}

pub(super) struct Card {
    id: ElementId,
    board: WeakEntity<Board>,
    source: Source,
    selected: bool,
}
impl Card {
    pub(super) fn issue(
        board: &Entity<Board>,
        scope: &okilum_t3::board::Scope,
        column: Column,
        binding: Arc<Binding>,
        selected: bool,
    ) -> Self {
        Self {
            id: ("board-virtual-issue", scope.number as usize).into(),
            board: board.downgrade(),
            source: Source::Issue(scope.clone(), column, binding),
            selected,
        }
    }
    pub(super) fn local(
        board: &Entity<Board>,
        card: &okilum_t3::board::local::Card,
        selected: bool,
    ) -> Self {
        Self {
            id: SharedString::from(format!("board-virtual-local-{}", card.id)).into(),
            board: board.downgrade(),
            source: Source::Local(card.clone()),
            selected,
        }
    }
}
impl IntoElement for Card {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Card {
    type RequestLayoutState = Entity<Height>;
    type PrepaintState = Option<AnyElement>;
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // Estimates are layout hints, never a cap on authored text. A visible
        // card always measures its complete, existing production renderer.
        let state =
            window.use_keyed_state((self.id.clone(), "height"), cx, |_, _| Height(px(192.)));
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = state.read(cx).0.into();
        style.min_size.width = px(0.).into();
        style.flex_shrink = 0.;
        (window.request_layout(style, None, cx), state)
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let viewport = window.content_mask().bounds;
        if !self.selected && !bounds.intersects(&viewport.dilate(px(400.))) {
            return None;
        }
        let mut element = self
            .board
            .update(cx, |board, cx| match &self.source {
                Source::Issue(scope, column, binding) => board.card(scope, *column, binding, cx),
                Source::Local(card) => board.local_card(card, cx),
            })
            .ok()?;
        let measured = element.layout_as_root(
            size(
                AvailableSpace::Definite(bounds.size.width),
                AvailableSpace::MaxContent,
            ),
            window,
            cx,
        );
        if (measured.height - state.read(cx).0).abs() > px(0.5) {
            state.update(cx, |height, _| height.0 = measured.height);
            let board = self.board.clone();
            window.defer(cx, move |_, cx| {
                let _ = board.update(cx, |_, cx| cx.notify());
            });
        }
        // Refinement can change a lane's total height on the next frame.
        // Contain this frame until the shared outer scroll surface relays it.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            element.prepaint_at(bounds.origin, window, cx);
        });
        Some(element)
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        element: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(element) = element {
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                element.paint(window, cx)
            });
        }
    }
}
