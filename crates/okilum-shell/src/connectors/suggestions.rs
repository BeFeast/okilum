//! Explicit generation controls use saved Chat settings and retain each change.
use super::*;
use crate::brain::suggestions_outbox::{Pending, SuggestionsJournal};

#[derive(Default)]
pub(super) struct Suggestions {
    current: Value,
    journal: Option<SuggestionsJournal>,
    pending: Vec<Pending>,
    busy: bool,
    fresh: bool,
    message: String,
    journal_error: Option<String>,
}
impl ConnectorsView {
    fn load_suggestions_journal(&mut self) -> Result<(), String> {
        if self.suggestions.journal.is_none() {
            self.suggestions.journal = Some(SuggestionsJournal::open(&self.identity)?);
        }
        self.suggestions.pending = self.suggestions.journal.as_ref().unwrap().pending()?;
        self.suggestions.journal_error = None;
        Ok(())
    }
    pub(super) fn refresh_suggestions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.suggestions.busy {
            return;
        }
        if let Err(error) = self.load_suggestions_journal() {
            self.suggestions.journal_error = Some(error);
        }
        self.suggestions.busy = true;
        self.suggestions.fresh = false;
        let endpoint = self.endpoint;
        let identity = self.identity.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move {
                    rpc_guarded(endpoint, json!({"op":"suggestions_get"}), Some(&identity))
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.suggestions.busy = false;
                match reply {
                    Ok(data)
                        if data["schema"] == "tessera-suggestions/v1"
                            && data["revision"].as_u64().is_some()
                            && matches!(
                                data["mode"].as_str(),
                                Some("disabled" | "enabled" | "paused")
                            ) =>
                    {
                        this.suggestions.current = data;
                        this.suggestions.fresh = true;
                    }
                    Ok(_) => {
                        this.suggestions.message =
                            "Suggestions settings are not supported by this backend.".into()
                    }
                    Err(error) => this.suggestions.message = error,
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn change_suggestions(
        &mut self,
        enabled: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.suggestions.busy {
            return;
        }
        if let Err(error) = self.load_suggestions_journal() {
            self.suggestions.journal_error = Some(error);
            cx.notify();
            return;
        }
        if let Some(enabled) = enabled {
            if !self.suggestions.fresh
                || self.suggestions.current["can_change"] != true
                || !self.suggestions.pending.is_empty()
                || enabled && self.suggestions.current["provider"]["available"] != true
            {
                return;
            }
            let journal = self.suggestions.journal.as_ref().unwrap();
            let pending = journal.prepare(
                self.suggestions.current["revision"].as_u64().unwrap(),
                enabled,
            );
            if let Err(error) = journal.retain(&pending) {
                // Publication may have succeeded before fsync failed. Reread the
                // journal so the next click cannot invent a replacement request.
                self.suggestions.message = error;
                if let Err(error) = self.load_suggestions_journal() {
                    self.suggestions.journal_error = Some(error);
                }
                cx.notify();
                return;
            }
            self.suggestions.pending.push(pending);
        }
        let Some(pending) = self.suggestions.pending.first().cloned() else {
            return;
        };
        self.suggestions.busy = true;
        self.suggestions.fresh = false;
        self.suggestions.message = "Saving Suggestions setting…".into();
        let endpoint = self.endpoint;
        let identity = pending.workspace.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let request = pending.wire();
            let reply = cx.background_executor().spawn(async move { rpc_guarded(endpoint, request, Some(&identity)) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.suggestions.busy = false;
                let result = reply.and_then(|outcome| {
                    this.suggestions.journal.as_ref().ok_or("Suggestions journal is unavailable")?.acknowledge(&pending, &outcome)?;
                    Ok(outcome)
                });
                match result {
                    Ok(outcome) => {
                        this.suggestions.message = if outcome["status"] == "not_applied" {
                            match outcome["reason"].as_str() {
                                Some("provider_unavailable") => "Saved Chat is not configured. Save Chat settings before enabling Suggestions.",
                                Some("revision_changed") => "Settings changed elsewhere. Review the current mode before trying again.",
                                _ => "This change was not applied. Review the current setting before trying again.",
                            }.into()
                        } else {
                            if outcome["receipt"]["replayed"] == true {
                                "Earlier change acknowledged. Current status includes any later changes."
                            } else {
                                "Setting saved. Current status is refreshed below."
                            }.into()
                        };
                        if let Err(error) = this.load_suggestions_journal() { this.suggestions.journal_error = Some(error); }
                        cx.emit(ConnectionsChanged);
                        // A replayed Enable receipt may predate a later Pause.
                        // Never derive current mode/revision from that receipt.
                        this.refresh_suggestions(window, cx);
                    }
                    Err(error) => this.suggestions.message = format!("{error} The original settings change remains saved. Connect to its original brain using Workspace, then recover the delivery."),
                }
                cx.notify();
            });
        }).detach();
    }
    pub(super) fn suggestions_card(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let colors = super::super::brand::palette(cx);
        let current = &self.suggestions.current;
        let mode = if self.suggestions.fresh {
            current["mode"].as_str().unwrap_or("unavailable")
        } else {
            "refresh required"
        };
        let mut card = v_flex().id("suggestions-settings").max_w(px(880.)).p_5().gap_3()
            .rounded(px(10.)).border_1().border_color(colors.border_subtle).bg(colors.surface_raised)
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Suggestions"))
            .child("Prepare ideas from new inbox thoughts, decisions and saved results using your saved Chat model. Suggestions do not start tasks or engines.")
            .child(format!("Mode: {mode}"));
        if self.suggestions.fresh && current["can_change"] == false {
            card = card.child("This backend is not accepting Suggestions settings changes. Existing suggestions remain available.");
        }
        if let Some(model) = current["provider"]["model"].as_str() {
            card = card.child(format!("Saved Chat model: {model}"));
        }
        if let Some(message) = current["provider"]["message"].as_str() {
            card = card.child(message.to_string());
        }
        if current["provider"]["available"] == false {
            card = card.child("Configure and save Chat below before enabling Suggestions.");
        }
        if let Some(queued) = current["queued"].as_u64() {
            card = card.child(format!(
                "Queued: {queued} · Running: {}",
                current["running"].as_u64().unwrap_or(0)
            ));
        }
        if let Some(backlog) = current["backlog"].as_str() {
            card = card.child(backlog.to_string());
        }
        card = card.child("Pause stops new model requests. A running request may finish; ready suggestions remain available. Thoughts captured while paused wait until you resume.");
        if let Some(error) = &self.suggestions.journal_error {
            card = card.child(error.clone());
        }
        if !self.suggestions.message.is_empty() {
            card = card.child(self.suggestions.message.clone());
        }
        let blocked = self.busy || self.suggestions.busy;
        let pending = !self.suggestions.pending.is_empty();
        let change_blocked = blocked
            || pending
            || !self.suggestions.fresh
            || self.suggestions.journal_error.is_some()
            || current["can_change"] != true;
        let enabled = current["mode"] == "enabled";
        let mut actions = h_flex().gap_2().flex_wrap().child(
            super::super::brand::control("suggestions-refresh", cx)
                .label("Refresh Suggestions")
                .disabled(blocked)
                .on_click(cx.listener(|this, _, window, cx| this.refresh_suggestions(window, cx))),
        );
        if pending {
            card = card.child(format!(
                "Unconfirmed setting: {}. Recover the saved change before changing the mode again.",
                if self.suggestions.pending[0].request.enabled {
                    "Enable"
                } else {
                    "Pause"
                }
            ));
            actions =
                actions.child(
                    super::super::brand::control("suggestions-recover", cx)
                        .label("Recover settings change")
                        .disabled(blocked || self.suggestions.journal_error.is_some())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.change_suggestions(None, window, cx)
                        })),
                );
        }
        actions = actions.child(
            super::super::brand::control("suggestions-toggle", cx)
                .label(if enabled {
                    "Pause Suggestions"
                } else if current["mode"] == "paused" {
                    "Resume Suggestions"
                } else {
                    "Enable Suggestions"
                })
                .disabled(change_blocked || !enabled && current["provider"]["available"] != true)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.change_suggestions(Some(!enabled), window, cx)
                })),
        );
        card.child(actions).into_any_element()
    }
}
