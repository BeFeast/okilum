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
#[derive(Clone)]
enum Source {
    Issue(okilum_t3::board::Scope, Column, Arc<Binding>),
    Local(okilum_t3::board::local::Card),
}

#[derive(Clone)]
pub(super) struct Card {
    id: ElementId,
    board: WeakEntity<Board>,
    source: Source,
    selected: bool,
    height: Option<Entity<Height>>,
}
impl Card {
    fn overdraw(&self, cx: &App) -> Pixels {
        if self
            .board
            .upgrade()
            .is_some_and(|board| board.read(cx).resizing)
        {
            px(0.)
        } else {
            px(400.)
        }
    }
    #[cfg(test)]
    fn count_layout(&self, cx: &App) {
        if let Some(board) = self.board.upgrade() {
            *board
                .read(cx)
                .frame_layout_work
                .borrow_mut()
                .last_mut()
                .expect("Board render frame") += 1;
        }
    }
    fn build(&self, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        self.board
            .update(cx, |board, cx| match &self.source {
                Source::Issue(scope, column, binding) => {
                    board.card(scope, *column, binding, window, cx)
                }
                Source::Local(card) => board.local_card(card, window, cx),
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
            height: None,
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
            height: None,
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
    type RequestLayoutState = AnyElement;
    type PrepaintState = bool;
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
    ) -> (LayoutId, AnyElement) {
        #[cfg(test)]
        self.count_layout(cx);
        let mut child = self
            .build(window, cx)
            .unwrap_or_else(|| div().into_any_element());
        (child.request_layout(window, cx), child)
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let state = self.height.as_ref().expect("lane-owned height hint");
        let scroll = self.scroll(cx);
        let focused = state
            .read(cx)
            .focused
            .as_ref()
            .is_some_and(|focus| focus.contains_focused(window, cx));
        state.update(cx, |height, _| {
            height.value = bounds.size.height;
            height.bounds = Some(bounds);
            height.scroll = scroll;
        });
        // Overdraw refines layout without building invisible interaction trees.
        if !self.selected && !focused && !bounds.intersects(&window.content_mask().bounds) {
            return false;
        }
        let focused = child.prepaint(window, cx);
        state.update(cx, |height, _| height.focused = focused);
        true
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child: &mut AnyElement,
        visible: &mut bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        if *visible {
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                child.paint(window, cx)
            });
        }
    }
}

/// Contiguous offscreen runs share one layout node. Height/focus state belongs
/// to the lane's canonical card IDs, not to whichever viewport currently mounts.
#[derive(IntoElement)]
pub(super) struct Lane {
    id: ElementId,
    cards: Vec<Card>,
}
impl Lane {
    pub(super) fn new(column: Column, cards: Vec<Card>) -> Self {
        Self {
            id: ("board-virtual-lane", column as usize).into(),
            cards,
        }
    }
}
impl RenderOnce for Lane {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let states = window.use_keyed_state((self.id.clone(), "heights"), cx, |_, _| {
            std::collections::HashMap::<ElementId, Entity<Height>>::new()
        });
        let ids: std::collections::HashSet<_> =
            self.cards.iter().map(|card| card.id.clone()).collect();
        states.update(cx, |states, _| states.retain(|id, _| ids.contains(id)));
        let overdraw = self
            .cards
            .first()
            .map(|card| card.overdraw(cx))
            .unwrap_or(px(400.));
        let viewport = self
            .cards
            .first()
            .and_then(|card| card.board.upgrade())
            .map(|board| board.read(cx).scroll.bounds())
            .filter(|bounds| bounds.size.height > px(0.))
            .unwrap_or_else(|| Bounds::new(Point::default(), window.viewport_size()))
            .dilate(overdraw);
        let mut root = v_flex()
            .id(self.id)
            .w_full()
            .min_w_0()
            .min_h_0()
            .flex_shrink_0()
            .gap_3();
        let mut hidden = Vec::new();
        let mut initial_y = px(0.);
        for mut card in self.cards {
            let state = states.update(cx, |states, cx| {
                states
                    .entry(card.id.clone())
                    .or_insert_with(|| {
                        cx.new(|_| Height {
                            value: px(192.),
                            focused: None,
                            bounds: None,
                            scroll: Point::default(),
                        })
                    })
                    .clone()
            });
            let height = state.read(cx);
            let focused = height
                .focused
                .as_ref()
                .is_some_and(|focus| focus.contains_focused(window, cx));
            let predicted = height
                .bounds
                .map(|bounds| {
                    Bounds::new(bounds.origin + card.scroll(cx) - height.scroll, bounds.size)
                })
                .unwrap_or_else(|| {
                    Bounds::new(
                        point(px(0.), initial_y),
                        size(window.viewport_size().width, height.value),
                    )
                });
            initial_y += height.value + px(12.);
            let mounted = card.selected || focused || predicted.intersects(&viewport);
            card.height = Some(state);
            if mounted {
                if !hidden.is_empty() {
                    root = root.child(Space {
                        cards: std::mem::take(&mut hidden),
                    });
                }
                root = root.child(card);
            } else {
                hidden.push(card);
            }
        }
        if !hidden.is_empty() {
            root = root.child(Space { cards: hidden });
        }
        root
    }
}

struct Space {
    cards: Vec<Card>,
}
impl IntoElement for Space {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Space {
    type RequestLayoutState = ();
    type PrepaintState = Vec<(Bounds<Pixels>, AnyElement)>;
    fn id(&self) -> Option<ElementId> {
        None
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
    ) -> (LayoutId, ()) {
        #[cfg(test)]
        if let Some(card) = self.cards.first() {
            card.count_layout(cx);
        }
        let height = self
            .cards
            .iter()
            .map(|card| card.height.as_ref().unwrap().read(cx).value)
            .fold(px(0.), |sum, height| sum + height)
            + px(12. * self.cards.len().saturating_sub(1) as f32);
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = height.into();
        style.flex_shrink = 0.;
        (window.request_layout(style, None, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let overdraw = self
            .cards
            .first()
            .map(|card| card.overdraw(cx))
            .unwrap_or(px(400.));
        let viewport = window.content_mask().bounds.dilate(overdraw);
        let mut y = bounds.top();
        let mut painted = Vec::new();
        for card in &self.cards {
            let state = card.height.as_ref().unwrap();
            let height = state.read(cx).value;
            let estimated = Bounds::new(point(bounds.left(), y), size(bounds.size.width, height));
            let scroll = card.scroll(cx);
            state.update(cx, |state, _| {
                state.bounds = Some(estimated);
                state.scroll = scroll;
            });
            y += height + px(12.);
            if !estimated.intersects(&viewport) {
                continue;
            }
            // A jump or a large height refinement can cross the predicted range.
            // Render it now, then fold its measured height into the shared pass.
            let mut element = card.clone().into_any_element();
            let measured = element.layout_as_root(
                size(
                    AvailableSpace::Definite(bounds.size.width),
                    AvailableSpace::MaxContent,
                ),
                window,
                cx,
            );
            if (measured.height - height).abs() > px(0.5) {
                let board = card.board.clone();
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
            window.with_content_mask(Some(ContentMask { bounds: estimated }), |window| {
                element.prepaint_at(estimated.origin, window, cx);
            });
            painted.push((estimated, element));
        }
        painted
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        elements: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for (bounds, element) in elements {
            window.with_content_mask(Some(ContentMask { bounds: *bounds }), |window| {
                element.paint(window, cx)
            });
        }
    }
}
