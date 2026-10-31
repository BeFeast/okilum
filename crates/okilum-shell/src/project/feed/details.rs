//! On-demand detail belongs to a source row/revision, never its list position.
//! A fresh row lease prevents late reads crossing reset, reconnect or thread.
use super::*;
use okilum_t3::projection::TurnRow;

enum Phase {
    Idle,
    Loading,
    Ready,
    Unavailable(&'static str),
}
pub(super) struct Detail {
    key: RowKey,
    revision: Option<String>,
    lease: Arc<()>,
    markdown: Option<SharedString>,
    phase: Phase,
}

impl Feed {
    pub(super) fn retain_detail_source(
        &mut self,
        position: u64,
        row: &TurnRow,
        cx: &mut Context<Self>,
    ) {
        let needed = presentation::content::needs_remote_detail(&row.item);
        if !needed {
            self.details.remove(&position);
            return;
        }
        let revision = okilum_t3::stream::turn_item::revision(&row.item)
            .ok()
            .map(str::to_owned);
        let same = self
            .details
            .get(&position)
            .is_some_and(|detail| detail.key == row.key && detail.revision == revision);
        if !same {
            self.details.insert(
                position,
                Detail {
                    key: row.key.clone(),
                    revision,
                    lease: Arc::new(()),
                    markdown: None,
                    phase: Phase::Idle,
                },
            );
        }
        if self.expanded.contains(&position) {
            self.load_detail(position, cx);
        }
    }

    pub(super) fn refresh_detail(&mut self, position: u64, cx: &mut Context<Self>) {
        if let Some(detail) = self.details.get_mut(&position) {
            if matches!(detail.phase, Phase::Loading) {
                return;
            }
            detail.phase = Phase::Idle;
        }
        self.load_detail(position, cx);
    }

    pub(super) fn load_detail(&mut self, position: u64, cx: &mut Context<Self>) {
        // Projection application and disclosure may run inside the parent's
        // update. Read its client only after that borrow has been released.
        cx.defer(move |this, cx| this.load_detail_now(position, cx));
    }

    fn load_detail_now(&mut self, position: u64, cx: &mut Context<Self>) {
        let Some(detail) = self.details.get_mut(&position) else {
            return;
        };
        if !matches!(detail.phase, Phase::Idle) {
            return;
        }
        let client = self
            .parent
            .upgrade()
            .and_then(|parent| parent.read(cx).native.model_client())
            .filter(|client| client.access_allowed());
        let Some(client) = client else {
            detail.phase = Phase::Unavailable("Reconnect to load full output.");
            self.detail_changed(position, cx);
            return;
        };
        let (key, revision, lease) = (
            detail.key.clone(),
            detail.revision.clone(),
            detail.lease.clone(),
        );
        detail.phase = Phase::Loading;
        self.detail_changed(position, cx);
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { client.turn_item(&key, revision.as_deref()) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(detail) = this.details.get_mut(&position).filter(|detail| Arc::ptr_eq(&detail.lease, &lease)) else { return };
                match result {
                    Ok(Some(item)) => match presentation::content::detail(&item) {
                        Some(content) => {
                            detail.markdown = Some(content.markdown.into());
                            detail.phase = if content.needs_remote_detail {
                                Phase::Unavailable("T3 still supplied abbreviated output. Full detail is unavailable.")
                            } else { Phase::Ready };
                        }
                        None => detail.phase = Phase::Unavailable("Full detail is unavailable for this activity."),
                    },
                    Ok(None) => detail.phase = Phase::Unavailable("This activity is no longer available on the server."),
                    Err(_) => detail.phase = Phase::Unavailable("Could not load full output from T3. Try again."),
                }
                this.detail_changed(position, cx);
            });
        }).detach();
    }

    fn detail_changed(&mut self, position: u64, cx: &mut Context<Self>) {
        if self.expanded.contains(&position) {
            if let Some(index) = self.indices.get(&position).copied() {
                self.scroller
                    .update(cx, |state, cx| state.remeasure_items(index..index + 1, cx));
            }
            cx.notify();
        }
    }

    pub(super) fn detail_text(&self, position: u64, fallback: &SharedString) -> SharedString {
        self.details
            .get(&position)
            .and_then(|detail| detail.markdown.clone())
            .unwrap_or_else(|| fallback.clone())
    }

    pub(super) fn detail_status(&self, position: u64, cx: &mut App) -> AnyElement {
        let Some(detail) = self.details.get(&position) else {
            return div().into_any_element();
        };
        let notice = match detail.phase {
            Phase::Idle => Some("Full output has not been loaded."),
            Phase::Loading => Some("Loading full output…"),
            Phase::Unavailable(message) => Some(message),
            Phase::Ready if detail.revision.as_deref() == Some("live") => {
                Some("Activity is still running. Refresh for the latest output.")
            }
            Phase::Ready => None,
        };
        let Some(notice) = notice else {
            return div().into_any_element();
        };
        let parent = self.parent.clone();
        v_flex()
            .gap_1()
            .child(
                div()
                    .whitespace_normal()
                    .text_color(brand::palette(cx).text_muted)
                    .child(notice),
            )
            .child(
                Button::new(("project-detail-refresh", position as usize))
                    .ghost()
                    .small()
                    .label("Refresh details")
                    .disabled(matches!(detail.phase, Phase::Loading))
                    .on_click(move |_, _, cx| {
                        let _ = parent.update(cx, |view, cx| {
                            view.feed
                                .update(cx, |feed, cx| feed.refresh_detail(position, cx))
                        });
                    }),
            )
            .into_any_element()
    }
}
