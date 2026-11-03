//! Window-local card height hints keep the Board's single scroll surface and
//! natural lane flow. Only viewport cards (plus overdraw) build controls/text.
//! Canonical cards, selection, source refresh and actions stay in Board.
use super::*;
use gpui::{
    AvailableSpace, ContentMask, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId,
    Style,
};

pub(super) struct Height {
    value: Pixels,
    focused: Option<FocusHandle>,
    bounds: Option<Bounds<Pixels>>,
    scroll: Point<Pixels>,
}
pub(super) struct Layout {
    height: Entity<Height>,
    child: Option<AnyElement>,
}
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
    fn build(&self, cx: &mut App) -> Option<AnyElement> {
        self.board
            .update(cx, |board, cx| match &self.source {
                Source::Issue(scope, column, binding) => board.card(scope, *column, binding, cx),
                Source::Local(card) => board.local_card(card, cx),
            })
            .ok()
    }
    fn scroll(&self, cx: &App) -> Point<Pixels> {
        self.board
            .upgrade()
            .map(|board| board.read(cx).scroll.offset())
            .unwrap_or_default()
    }
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
    type RequestLayoutState = Layout;
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
        let state = window.use_keyed_state((self.id.clone(), "height"), cx, |_, _| Height {
            value: px(192.),
            focused: None,
            bounds: None,
            scroll: Point::default(),
        });
        let height = state.read(cx);
        let focused = height
            .focused
            .as_ref()
            .is_some_and(|focus| focus.contains_focused(window, cx));
        // Previous layout bounds plus the current physical scroll delta select
        // the viewport before layout. Visible cards join the ONE parent layout
        // pass; they do not each run an independent Taffy root on every resize.
        let viewport = Bounds::new(Point::default(), window.viewport_size()).dilate(px(400.));
        let predicted = height.bounds.map(|bounds| {
            Bounds::new(bounds.origin + self.scroll(cx) - height.scroll, bounds.size)
        });
        let mut child = (self.selected
            || focused
            || predicted.is_some_and(|bounds| bounds.intersects(&viewport)))
        .then(|| self.build(cx))
        .flatten();
        if let Some(element) = &mut child {
            return (
                element.request_layout(window, cx),
                Layout {
                    height: state,
                    child,
                },
            );
        }
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = state.read(cx).value.into();
        style.min_size.width = px(0.).into();
        style.flex_shrink = 0.;
        (
            window.request_layout(style, None, cx),
            Layout {
                height: state,
                child,
            },
        )
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
        let scroll = self.scroll(cx);
        state.height.update(cx, |height, _| {
            height.bounds = Some(bounds);
            height.scroll = scroll;
        });
        let focused = state
            .height
            .read(cx)
            .focused
            .as_ref()
            .is_some_and(|focus| focus.contains_focused(window, cx));
        if !self.selected && !focused && !bounds.intersects(&viewport.dilate(px(400.))) {
            return None;
        }
        if let Some(mut child) = state.child.take() {
            state
                .height
                .update(cx, |height, _| height.value = bounds.size.height);
            if !self.selected && !focused && !bounds.intersects(&viewport) {
                return None;
            }
            let focused = child.prepaint(window, cx);
            state
                .height
                .update(cx, |height, _| height.focused = focused);
            return Some(child);
        }
        // First paint or a jump beyond the previous bounds: refine immediately,
        // retaining overdraw and a same-frame fallback instead of blank cards.
        let mut element = self.build(cx)?;
        let measured = element.layout_as_root(
            size(
                AvailableSpace::Definite(bounds.size.width),
                AvailableSpace::MaxContent,
            ),
            window,
            cx,
        );
        if (measured.height - state.height.read(cx).value).abs() > px(0.5) {
            state
                .height
                .update(cx, |height, _| height.value = measured.height);
            let board = self.board.clone();
            let schedule = board
                .update(cx, |board, _| {
                    !std::mem::replace(&mut board.height_refinement_pending, true)
                })
                .unwrap_or(false);
            if schedule {
                window.defer(cx, move |_, cx| {
                    let _ = board.update(cx, |board, cx| {
                        board.height_refinement_pending = false;
                        cx.notify();
                    });
                });
            }
        }
        // Overdraw is for height refinement, not invisible interaction trees.
        // A card entering the viewport still builds and prepaints in this frame.
        if !self.selected && !focused && !bounds.intersects(&viewport) {
            return None;
        }
        // Refinement can change a lane's total height on the next frame.
        // Contain this frame until the shared outer scroll surface relays it.
        let focused = window.with_content_mask(Some(ContentMask { bounds }), |window| {
            element.prepaint_at(bounds.origin, window, cx)
        });
        state
            .height
            .update(cx, |height, _| height.focused = focused);
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
