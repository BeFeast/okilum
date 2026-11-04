//! Bounded, source-lifetime Markdown state. Adjacent chat rows parse before they
//! enter the viewport; disclosure, assets and RPC remain render/user driven.
use super::*;
use gpui_component::text::TextViewState;

const CAPACITY: usize = 64;
struct Entry {
    text: SharedString,
    state: Entity<TextViewState>,
    used: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn adjacent_markdown_reuses_state_updates_source_and_has_a_bounded_lifetime(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let mut cache = Cache::default();
        let id: ElementId = ("message", 7usize).into();
        let first = cx.update(|cx| cache.prepare(id.clone(), "Original **source**".into(), cx));
        for _ in 0..10 {
            let again = cx.update(|cx| cache.prepare(id.clone(), "Original **source**".into(), cx));
            assert_eq!(first.entity_id(), again.entity_id());
        }
        assert_eq!(
            cache.created, 1,
            "revisiting a viewport must not reparse it"
        );
        let changed = cx.update(|cx| cache.prepare(id.clone(), "Revised **source**".into(), cx));
        assert_eq!(first.entity_id(), changed.entity_id());
        cx.run_until_parked();
        changed.update(cx, |state, cx| state.select_all(cx));
        assert_eq!(
            changed.read_with(cx, |state, _| state.selected_text()),
            "Revised source\n"
        );
        for i in 0..CAPACITY * 3 {
            cx.update(|cx| cache.prepare(("other", i).into(), format!("Message {i}").into(), cx));
            assert!(cache.len() <= CAPACITY);
        }
        assert!(
            cache.entity(&id).is_none(),
            "old viewport states are evicted"
        );
        cache.clear();
        assert_eq!(cache.len(), 0);
        let reset = cx.update(|cx| cache.prepare(id, "Original **source**".into(), cx));
        assert_ne!(
            first.entity_id(),
            reset.entity_id(),
            "another thread cannot inherit selection state"
        );
    }
}
#[derive(Default)]
pub(super) struct Cache {
    entries: HashMap<ElementId, Entry>,
    clock: u64,
    #[cfg(test)]
    pub(super) created: usize,
}
impl Cache {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
    pub(super) fn prepare(
        &mut self,
        id: ElementId,
        text: SharedString,
        cx: &mut App,
    ) -> Entity<TextViewState> {
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.used = self.clock;
            if entry.text != text {
                entry
                    .state
                    .update(cx, |state, cx| state.set_text(text.as_str(), cx));
                entry.text = text;
            }
            return entry.state.clone();
        }
        if self.entries.len() == CAPACITY {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(id, _)| id.clone())
                .unwrap();
            self.entries.remove(&oldest);
        }
        let state = cx.new(|cx| TextViewState::markdown(text.as_str(), cx));
        #[cfg(test)]
        {
            self.created += 1;
        }
        self.entries.insert(
            id,
            Entry {
                text,
                state: state.clone(),
                used: self.clock,
            },
        );
        state
    }
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
    #[cfg(test)]
    pub(super) fn entity(&self, id: &ElementId) -> Option<gpui::EntityId> {
        self.entries.get(id).map(|entry| entry.state.entity_id())
    }
}
impl Feed {
    pub(super) fn prewarm_markdown(&self, cx: &mut Context<Self>) {
        let state = self.scroller.read(cx);
        let anchor = if state.is_following_tail() {
            self.rows.len().saturating_sub(1)
        } else {
            state.logical_scroll_top().item_ix
        };
        let start = anchor.saturating_sub(16);
        let end = (anchor + 20).min(self.rows.len());
        let mut cache = self.rich.borrow_mut();
        for row in &self.rows[start.min(end)..end] {
            let Row::Message(position) = row else {
                continue;
            };
            let Some(message) = self
                .messages
                .get(position)
                .filter(|message| message.kind == MessageKind::Chat)
            else {
                continue;
            };
            cache.prepare(
                ("project-message", *position as usize).into(),
                message.text.clone(),
                cx,
            );
        }
    }
}
