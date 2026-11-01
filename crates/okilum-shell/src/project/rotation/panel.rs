//! Standalone GPUI surface; parent mounts the Entity and subscribes to Requested.
//! Disk work is performed by parent off the UI thread using PendingReview.
use super::{calendar_day, DraftState, PendingReview};
use crate::project::{design, *};
use okilum_t3::rotation::Checkpoint;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::project) enum Action {
    Resume,
    Defer,
    Cancel,
}
#[derive(Clone)]
pub(in crate::project) struct Requested {
    pub action: Action,
    /// Retain exact revision AND owner when scheduling asynchronous work.
    pub expected: Checkpoint,
}
impl EventEmitter<Requested> for ReconciliationPanel {}

pub(in crate::project) struct ReconciliationPanel {
    checkpoint: Checkpoint,
    draft_state: DraftState,
    resume_text: Option<String>,
    busy: bool,
    stale: bool,
    error: Option<String>,
}
impl ReconciliationPanel {
    pub fn new(review: &PendingReview) -> Self {
        Self {
            checkpoint: review.checkpoint().clone(),
            draft_state: review.draft_state(),
            resume_text: review.resume_text().map(str::to_owned),
            busy: false,
            stale: false,
            error: None,
        }
    }
    /// Invalidates outstanding actions on project/owner/composer change. Parent
    /// installs a freshly loaded review before enabling controls again.
    pub fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        cx.notify();
    }
    /// A failed writer leaves the durable claim pending. Reopen reconciliation
    /// to inspect the latest composer rather than automatically retrying it.
    pub fn failed(&mut self, expected: &Checkpoint, error: String, cx: &mut Context<Self>) {
        if &self.checkpoint != expected {
            return;
        }
        self.busy = false;
        self.stale = true;
        self.error = Some(error);
        cx.notify();
    }
    fn request(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.busy || self.stale {
            return;
        }
        if action == Action::Resume
            && (!self.checkpoint.policy.enabled()
                || !matches!(
                    self.draft_state,
                    DraftState::Empty | DraftState::AlreadyPrepared
                ))
        {
            return;
        }
        self.busy = true;
        cx.emit(Requested {
            action,
            expected: self.checkpoint.clone(),
        });
        cx.notify();
    }
}
impl Render for ReconciliationPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = &self.checkpoint.owner;
        let mut panel = v_flex()
            .id("rotation-reconciliation")
            .w_full()
            .p(px(design::INSET))
            .gap_2()
            .text_size(px(design::BODY))
            .child("Review pending manager rotation")
            .child(format!(
                "Target: {} · project {} · manager {}",
                owner.origin, owner.project, owner.manager
            ));
        if let Some(slot) = &self.checkpoint.pending {
            panel = panel.child(format!(
                "Window: {} · {:02}:{:02} {} · {}",
                calendar_day(slot.day),
                slot.minute / 60,
                slot.minute % 60,
                self.checkpoint.policy.timezone(),
                slot.utc.map_or_else(
                    || "unresolved".into(),
                    |t| format!(
                        "{} {:02}:{:02} UTC",
                        calendar_day(t.div_euclid(86400)),
                        t.rem_euclid(86400) / 3600,
                        t.rem_euclid(3600) / 60
                    )
                )
            ));
        }
        panel = panel.child(match self.draft_state {
            DraftState::SourceUnavailable => "Draft: original text was not retained with this claim. Restore and review the exact rotation source before resuming.",
            DraftState::Empty => "Draft: composer is empty. Resume prepares the reviewed text for you to send explicitly.",
            DraftState::AlreadyPrepared => "Draft: reviewed text is already in the composer. Resume keeps that draft and confirms its persistence.",
            DraftState::AuthoredDraftPreserved => "Draft: your text, attachments or message type are preserved. Resume cannot replace them.",
        });
        if let Some(text) = &self.resume_text {
            panel = panel.child("Reviewed resume text:").child(
                div()
                    .id("rotation-resume-text")
                    .max_h(px(160.))
                    .overflow_y_scroll()
                    .child(text.clone()),
            );
        }
        panel = panel.child("No rotation is sent automatically. Defer skips windows through now. Cancel clears only this claim. Both preserve your draft.");
        if self.stale {
            panel = panel
                .child("Target or draft changed. Reopen reconciliation before taking an action.");
        }
        if let Some(error) = &self.error {
            panel = panel.child(div().text_color(cx.theme().danger).child(error.clone()));
        }
        let locked = self.busy || self.stale;
        panel.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("rotation-resume")
                        .ghost()
                        .label("Resume draft")
                        .disabled(
                            locked
                                || !self.checkpoint.policy.enabled()
                                || !matches!(
                                    self.draft_state,
                                    DraftState::Empty | DraftState::AlreadyPrepared
                                ),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.request(Action::Resume, cx))),
                )
                .child(
                    Button::new("rotation-defer")
                        .ghost()
                        .label("Defer to next slot")
                        .disabled(locked)
                        .on_click(cx.listener(|this, _, _, cx| this.request(Action::Defer, cx))),
                )
                .child(
                    Button::new("rotation-cancel")
                        .ghost()
                        .label("Cancel this claim")
                        .disabled(locked)
                        .on_click(cx.listener(|this, _, _, cx| this.request(Action::Cancel, cx))),
                ),
        )
    }
}
