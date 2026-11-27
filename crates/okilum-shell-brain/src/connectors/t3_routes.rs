//! Ordinary Connections review/adopt flow; provider work is a separate Start.
mod controller;
use super::*;
use crate::brain::t3_route_outbox::{Pending, RouteJournal};
use controller::{generic_save_config, hydrated_form, unknown_history_count, Selection};
use gpui_component::input::{Textarea, TextareaState};

#[derive(Default)]
pub(super) struct TargetRoutes {
    current: Value,
    baseline: Value,
    manifest_input: Option<Entity<TextareaState>>,
    fresh: bool,
    selection: Selection,
    journal: Option<RouteJournal>,
    pending: Vec<Pending>,
    journal_error: Option<String>,
    message: String,
}
fn target_label(target: &Value) -> String {
    if !target.is_object() {
        return "No selected T3 target".into();
    }
    format!(
        "{} · environment {} · project {}",
        target["base_url"].as_str().unwrap_or("unknown origin"),
        target["environment_id"].as_str().unwrap_or("unknown"),
        target["project_id"].as_str().unwrap_or("unknown")
    )
}
fn blocker_message(code: &str) -> &'static str {
    match code {
        "active_or_uncertain_work" | "active_work" | "unsettled_work" | "provider_work_unsettled" | "running_conversation" | "correlation_uncertain" => "Finish or resolve existing provider work before reviewing a future target.",
        "environment_mismatch" | "project_mismatch" | "project_missing" | "candidate_environment_mismatch" | "candidate_project_missing" => "Candidate identity did not match discovery. Check its environment and project, then review again.",
        "revision_changed" | "inventory_changed" | "candidate_changed" | "prepared_selection_stale" | "snapshot_changed" => "Saved work or target changed. Refresh and prepare a new review.",
        "receipt_mismatch" | "receipt_identity_conflict" | "legacy_identity_unproven" | "ambiguous_history" | "terminal_receipt_missing" | "t3_receipt_corrupt" | "t3_receipt_identity_conflict" | "terminal_evidence_unproven" | "terminal_result_core_mismatch" | "terminal_result_identity_mismatch" | "historical_proof_changed" | "legacy_identity_mismatch" => "Historical operation evidence cannot be verified. Preserve its records and resolve the reported mismatch.",
        "endpoint_unreachable" | "candidate_unavailable" | "discovery_unavailable" | "candidate_discovery_unavailable" => "Candidate is unavailable. Check its endpoint and credential reference, then review again.",
        "source_projection_pending" | "terminal_projection_missing" | "terminal_sequence_unacknowledged" | "terminal_cursor_unacknowledged" => "A provider result or its source projection is not durably acknowledged. Resolve that existing operation before adopting a target.",
        _ => "Resolve the reported condition in the saved Brain, then prepare a new review. No target has been adopted.",
    }
}
impl ConnectorsView {
    pub(super) fn remember_target_baseline(&mut self, config: &Value) {
        self.target_routes.baseline = config["t3"].clone();
    }
    pub(super) fn generic_connector_config(&self, form: Value) -> Result<Value, String> {
        generic_save_config(
            form,
            &self.target_routes.baseline,
            &self.target_routes.current,
        )
    }
    fn target_manifest(&self, cx: &App) -> Result<Option<Value>, String> {
        let text = self
            .target_routes
            .manifest_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Ok(None);
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|_| "Compatibility manifest must be valid JSON; no request was sent.")?;
        if !value.is_object() || !value["entries"].is_array() {
            return Err("Compatibility manifest requires an entries array.".into());
        }
        Ok(Some(value))
    }
    fn target_journal(&mut self) -> Result<(), String> {
        if self.target_routes.journal.is_none() {
            self.target_routes.journal = Some(RouteJournal::open(&self.identity)?);
        }
        self.target_routes.pending = self.target_routes.journal.as_ref().unwrap().pending()?;
        self.target_routes.journal_error = None;
        Ok(())
    }
    pub(super) fn refresh_target_routes(
        &mut self,
        hydrate_form: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        if let Err(error) = self.target_journal() {
            self.target_routes.journal_error = Some(error);
        }
        self.target_routes.fresh = false;
        self.busy = true;
        let endpoint = self.endpoint;
        let identity = self.identity.clone();
        let before = self.config(cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move {
                    rpc_guarded(endpoint, json!({"op":"t3_target_get"}), Some(&identity))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match reply {
                    Ok(value)
                        if value["schema"] == "okilum-t3-target/v1"
                            && value["generations"].is_array() =>
                    {
                        // Authoritative current view, never inferred from a replayed receipt.
                        if let Some(effective) =
                            hydrated_form(&before, &this.config(cx), &value, hydrate_form)
                        {
                            this.load(&effective, window, cx);
                        }
                        this.target_routes.current = value;
                        this.target_routes.fresh = true;
                    }
                    Ok(_) => {
                        this.target_routes.message =
                            "Future-target selection is not supported by this backend.".into()
                    }
                    Err(error) => this.target_routes.message = error,
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn review_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.target_routes.pending.is_empty() {
            return;
        }
        let candidate = self.config(cx)["t3"].clone();
        if !candidate.is_object() {
            self.target_routes.message =
                "Enable T3 and select its candidate settings first.".into();
            cx.notify();
            return;
        }
        let manifest = match self.target_manifest(cx) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.target_routes.message = error;
                cx.notify();
                return;
            }
        };
        let request = self
            .target_routes
            .selection
            .prepare_with_manifest(candidate.clone(), manifest.clone());
        self.target_routes.message = "Checking the candidate and retained history…".into();
        self.busy = true;
        let requested = candidate.clone();
        let endpoint = self.endpoint;
        let identity = self.identity.clone();
        cx.notify();
        cx.spawn_in(window,async move|this,cx|{
            let reply=cx.background_executor().spawn(async move{rpc_guarded(endpoint,request,Some(&identity))}).await;
            let _=this.update_in(cx,|this,_,cx|{
                this.busy=false;let current=this.config(cx)["t3"].clone();
                let current_manifest=this.target_manifest(cx);
                this.target_routes.message=match reply {
                    Ok(review)=>match current_manifest.and_then(|now|this.target_routes.selection.accept_with_manifest(requested,&current,manifest,&now,review)){
                        Ok(())=>"Review the distinct target and retained-history verdict below. Adoption requires a separate click.".into(),
                        Err(error)=>error,
                    },
                    Err(error)=>error,
                };cx.notify();
            });
        }).detach();
    }
    fn adopt_target(&mut self, recover: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Err(error) = self.target_journal() {
            self.target_routes.journal_error = Some(error);
            cx.notify();
            return;
        }
        if !recover {
            let manifest = match self.target_manifest(cx) {
                Ok(value) => value,
                Err(error) => {
                    self.target_routes.message = error;
                    cx.notify();
                    return;
                }
            };
            self.target_routes.selection.observe_manifest(&manifest);
            let candidate = self.config(cx)["t3"].clone();
            if !self.target_routes.pending.is_empty()
                || !self.target_routes.selection.can_adopt(&candidate)
            {
                return;
            }
            let Some(pending) = self
                .target_routes
                .selection
                .adoption(self.identity.clone(), &candidate)
            else {
                return;
            };
            if let Err(error) = self
                .target_routes
                .journal
                .as_ref()
                .unwrap()
                .retain(&pending)
            {
                self.target_routes.message = error;
                if let Err(error) = self.target_journal() {
                    self.target_routes.journal_error = Some(error);
                }
                cx.notify();
                return;
            }
            self.target_routes.pending.push(pending);
        }
        let Some(pending) = self.target_routes.pending.first().cloned() else {
            return;
        };
        self.target_routes.selection.clear();
        self.target_routes.fresh = false;
        self.busy = true;
        self.target_routes.message="Recording the future target selection. Existing operations keep their historical routes…".into();
        let endpoint = self.endpoint;
        let request = pending.wire();
        let identity = pending.workspace.clone();
        cx.notify();
        cx.spawn_in(window,async move|this,cx|{
            let reply=cx.background_executor().spawn(async move{rpc_guarded(endpoint,request,Some(&identity))}).await;
            let _=this.update_in(cx,|this,window,cx|{
                this.busy=false;
                let result=reply.and_then(|outcome|{
                    this.target_routes.journal.as_ref().ok_or("Target adoption history is unavailable")?.acknowledge(&pending,&outcome)?;Ok(outcome)
                });
                match result {
                    Ok(outcome)=>{
                        this.target_routes.message=if outcome["status"]=="committed" {
                            "Future-target selection acknowledged. Existing operations were not restarted. Current selection is refreshed below.".into()
                        } else {format!("Not adopted: {}. {}",outcome["reason"].as_str().unwrap_or("rejected"),blocker_message(outcome["reason"].as_str().unwrap_or("")))};
                        if let Err(error)=this.target_journal(){this.target_routes.journal_error=Some(error);}
                        cx.emit(ConnectionsChanged);this.refresh_target_routes(false,window,cx);
                    },
                    Err(error)=>this.target_routes.message=format!("{error} The exact adoption request remains saved. Recover this delivery before preparing another target."),
                }
                cx.notify();
            });
        }).detach();
    }
    pub(super) fn target_routes_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.target_routes.manifest_input.is_none() {
            self.target_routes.manifest_input = Some(cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(4)
                    .placeholder("Optional compatibility manifest JSON")
            }));
        }
        match self.target_manifest(cx) {
            Ok(manifest) => self.target_routes.selection.observe_manifest(&manifest),
            Err(_) => self.target_routes.selection.clear(),
        };
        let candidate = self.config(cx)["t3"].clone();
        self.target_routes.selection.observe(&candidate);
        let colors = super::super::brand::palette(cx);
        let mut card=v_flex().id("t3-future-target").gap_3().p_4().border_1().border_color(colors.border_subtle)
            .child(div().font_weight(FontWeight::SEMIBOLD).child("T3 target for future stages"))
            .child("A different environment is a distinct target. Existing operations retain their original identities and history; adoption starts no provider work.")
            .child(format!("Current selection{}: {}",if self.target_routes.fresh{""}else{" (refresh required)"},target_label(&self.target_routes.current["active"])))
            .child(h_flex().gap_2().child(super::super::brand::control("t3-target-refresh",cx).label("Refresh selected target").disabled(self.busy).on_click(cx.listener(|this,_,window,cx|this.refresh_target_routes(false,window,cx))))
                .child(super::super::brand::control("t3-target-review",cx).label("Review future target").disabled(self.busy || !candidate.is_object() || !self.target_routes.pending.is_empty()).on_click(cx.listener(|this,_,window,cx|this.review_target(window,cx)))));
        card=card.child(div().text_sm().child("Optional preserved-envelope evidence. Paste an explicitly reviewed manifest with backend artifact paths and pinned hashes; leave empty when no compatibility proof is needed."))
            .child(Textarea::new(self.target_routes.manifest_input.as_ref().unwrap()).disabled(self.busy));
        let historical_count = self.target_routes.current["historical_terminal_unroutable_count"]
            .as_u64()
            .unwrap_or(0);
        if historical_count > 0 {
            card = card.child(format!("{historical_count} settled historical operation(s) remain local-only because their original route is unproved. Saved outcomes are preserved; historical links and provider replay are permanently unavailable."));
        }
        if let Some(review) = &self.target_routes.selection.review {
            card = card
                .child(format!("Previous: {}", target_label(&review["previous"])))
                .child(format!("Candidate: {}", target_label(&review["candidate"])));
            for proof in review["compatibility_proofs"]
                .as_array()
                .into_iter()
                .flatten()
            {
                card = card.child(format!(
                    "Preserved envelope proof: operation {} · artifact {} · envelope {} · {}",
                    proof["operation_id"].as_str().unwrap_or("unknown"),
                    proof["artifact_sha256"].as_str().unwrap_or("missing hash"),
                    proof["envelope_sha256"].as_str().unwrap_or("missing hash"),
                    proof["provenance"].as_str().unwrap_or("missing provenance")
                ));
            }
            let unknown = unknown_history_count(review);
            if unknown > 0 {
                card=card.child(format!("{unknown} settled historical operation(s) have no proved origin. Their saved outcomes remain local; links and provider replay are permanently unavailable. Adopting accepts this limitation without inventing a route."));
            }
            for blocker in review["blockers"].as_array().into_iter().flatten() {
                let code = blocker["code"].as_str().unwrap_or("unrecognized_blocker");
                card = card.child(format!("Blocked: {code}. {}", blocker_message(code)));
            }
            card = card.child(
                super::super::brand::control("t3-target-adopt", cx)
                    .primary()
                    .label(if unknown > 0 {
                        "Accept local-only history and adopt for future stages"
                    } else {
                        "Adopt for future stages"
                    })
                    .disabled(
                        self.busy
                            || self.target_routes.journal_error.is_some()
                            || !self.target_routes.pending.is_empty()
                            || !self.target_routes.selection.can_adopt(&candidate),
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.adopt_target(false, window, cx)),
                    ),
            );
        }
        if !self.target_routes.pending.is_empty() {
            card=card.child("An adoption delivery is unresolved. Its original workspace, candidate and operation ID are retained; newer form edits will not change its retry.")
                .child(super::super::brand::control("t3-target-recover",cx).label("Recover saved adoption delivery").disabled(self.busy || self.target_routes.journal_error.is_some()).on_click(cx.listener(|this,_,window,cx|this.adopt_target(true,window,cx))));
        }
        if let Some(error) = &self.target_routes.journal_error {
            card = card.child(format!("Adoption history unavailable: {error}"));
        }
        card.child(self.target_routes.message.clone())
            .into_any_element()
    }
}
