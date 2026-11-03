//! Window-local card height hints keep the Board's single scroll surface and
//! natural lane flow. Only viewport cards (plus overdraw) build controls/text.
//! Canonical cards, selection, source refresh and actions stay in Board.
use super::*;
use gpui::{
    AvailableSpace, ContentMask, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId,
    Style,
};

/// A rendered geometry leaf invalidates its Board ancestor without publishing
/// a source change. Its transparent viewport slot remains mounted when scrolling.
pub(super) struct Geometry;
impl Render for Geometry {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

pub(super) struct Height {
    value: Pixels,
    focused: Option<FocusHandle>,
}
// Hints never publish: Lane explicitly schedules the shared geometry leaf.
// Plain window-local cells avoid registering 240 reactive dependencies during
// every splitter step, including cards that are not rendered.
type HeightHint = std::rc::Rc<std::cell::RefCell<Height>>;

#[derive(Clone)]
enum Source {
    Issue(usize, Column, Arc<Binding>),
    Local(okilum_t3::board::local::Card),
}

#[derive(Clone)]
pub(super) struct Card {
    id: ElementId,
    board: WeakEntity<Board>,
    source: Source,
    selected: bool,
    height: Option<HeightHint>,
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
                Source::Issue(index, column, binding) => {
                    board.card(&binding.issues[*index], *column, binding, window, cx)
                }
                Source::Local(card) => board.local_card(card, window, cx),
            })
            .ok()
    }
    pub(super) fn issue(
        board: &Entity<Board>,
        scope: &okilum_t3::board::Scope,
        index: usize,
        column: Column,
        binding: Arc<Binding>,
        selected: bool,
    ) -> Self {
        Self {
            id: ("board-virtual-issue", scope.number as usize).into(),
            board: board.downgrade(),
            source: Source::Issue(index, column, binding),
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
        let focused = state
            .borrow()
            .focused
            .as_ref()
            .is_some_and(|focus| focus.contains_focused(window, cx));
        state.borrow_mut().value = bounds.size.height;
        // Overdraw refines layout without building invisible interaction trees.
        if !self.selected && !focused && !bounds.intersects(&window.content_mask().bounds) {
            return false;
        }
        let focused = child.prepaint(window, cx);
        state.borrow_mut().focused = focused;
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

/// Each lane has one natural-height layout node. Rich viewport cards are
/// measured once at the resolved lane width, rather than participating in
/// ancestor flexbox intrinsic-size negotiations. Canonical height/focus hints
/// survive unmounting; a refinement schedules one shared extent update.
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
    fn refine(&self, window: &mut Window, cx: &mut App) {
        let Some(card) = self.cards.first() else {
            return;
        };
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
                    board.geometry.update(cx, |_, cx| cx.notify());
                });
            });
        }
    }
}
impl IntoElement for Lane {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Lane {
    type RequestLayoutState = ();
    type PrepaintState = Vec<AnyElement>;
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
    ) -> (LayoutId, ()) {
        #[cfg(test)]
        if let Some(card) = self.cards.first() {
            card.count_layout(cx);
        }
        let states = window.use_keyed_state((self.id.clone(), "heights"), cx, |_, _| {
            std::collections::HashMap::<ElementId, HeightHint>::new()
        });
        let ids: std::collections::HashSet<_> =
            self.cards.iter().map(|card| card.id.clone()).collect();
        let mut height = states.update(cx, |states, _| {
            states.retain(|id, _| ids.contains(id));
            let mut height = px(0.);
            for card in &mut self.cards {
                let state = states
                    .entry(card.id.clone())
                    .or_insert_with(|| {
                        std::rc::Rc::new(std::cell::RefCell::new(Height {
                            value: px(192.),
                            focused: None,
                        }))
                    })
                    .clone();
                height += state.borrow().value;
                card.height = Some(state);
            }
            height
        });
        height += px(12. * self.cards.len().saturating_sub(1) as f32);
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = height.into();
        style.min_size.width = px(0.).into();
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
    ) -> Vec<AnyElement> {
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
            let (height, focused) = {
                let hint = state.borrow();
                (
                    hint.value,
                    hint.focused
                        .as_ref()
                        .is_some_and(|focus| focus.contains_focused(window, cx)),
                )
            };
            let estimated = Bounds::new(point(bounds.left(), y), size(bounds.size.width, height));
            if !card.selected && !focused && !estimated.intersects(&viewport) {
                y += estimated.size.height + px(12.);
                continue;
            }
            let mut element = card.clone().into_any_element();
            let measured = element.layout_as_root(
                size(
                    AvailableSpace::Definite(bounds.size.width),
                    AvailableSpace::MaxContent,
                ),
                window,
                cx,
            );
            // Place the next card using its actual predecessor, in this frame.
            // No placeholder can blank out a newly exposed portion of the lane.
            element.prepaint_at(estimated.origin, window, cx);
            y += measured.height + px(12.);
            painted.push(element);
        }
        let measured_height = y - bounds.top() - px(12.);
        if (measured_height - bounds.size.height).abs() > px(0.5) {
            self.refine(window, cx);
        }
        painted
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        elements: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        for element in elements {
            element.paint(window, cx);
        }
    }
}
