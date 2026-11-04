//! Dispatch adapter for the Reader's explicit rules offer. Saving performs no
//! gateway calls; click workers reuse the existing durable Manager journals.
use okilum_t3::{
    actions::{DispatchMode, Intent},
    rules_refresh::{RulesOffer, RulesRefresh, RulesTarget},
};

/// On the offer button click, pass the captured offer and the currently verified
/// Manager identity. Feed this intent through the existing durable action path,
/// targeting `clicked.target` (including origin/project), never a later selection.
/// Do not replace or clear the user's independent composer draft.
///
/// After journal preparation call `RulesRefresh::prepared` with its request ID
/// before transport dispatch. Keep the returned receipt with the worker; pass
/// it to `acknowledge` only on matching queue acceptance. Retry uses that same
/// retained journal record. Neither queue acceptance nor a send failure asserts
/// that the Manager actually reloaded rules.
#[allow(dead_code, reason = "offer click adapter, not wired yet")]
pub fn rules_reread_intent(
    refresh: &RulesRefresh,
    clicked: &RulesOffer,
    selected: &RulesTarget,
) -> Result<Intent, &'static str> {
    let text = refresh.message(clicked, selected)?;
    Ok(Intent::message(text, DispatchMode::QueueAfterActive))
}

use super::*;
use okilum_t3::rules_refresh::{validate_rules_retry, RulesDispatch, RulesRevision};

pub(crate) struct RulesDelivery {
    pub receipt: Option<RulesDispatch>,
    pub acknowledged: bool,
    pub error: Option<String>,
}

pub(crate) type RulesJob = Box<dyn FnOnce() -> RulesDelivery + Send>;

impl ProjectView {
    /// Capture only the configured Manager whose area contains this saved file.
    /// No lookup, connection, discovery or send is triggered by a source save.
    pub(crate) fn saved_rules_offer(
        &self,
        vault: &Path,
        source: &Path,
        revision: &str,
    ) -> Option<RulesOffer> {
        let area = self.area.as_ref()?;
        if !source.starts_with(area) || !source.starts_with(vault) {
            return None;
        }
        let binding = self.binding.as_ref()?;
        let target = self.rules_target()?;
        if binding.manager.thread_id.as_deref() != Some(target.thread_id.as_str()) {
            return None;
        }
        let path = source
            .strip_prefix(vault)
            .ok()?
            .to_str()?
            .replace('\\', "/");
        if path
            .split('/')
            .any(|part| part == ".." || part == "." || part.is_empty())
        {
            return None;
        }
        let offer = RulesOffer {
            target,
            source: RulesRevision {
                path,
                revision: revision.into(),
            },
        };
        let mut check = RulesRefresh::default();
        check.saved(offer.clone()).ok()?;
        Some(offer)
    }

    fn rules_target(&self) -> Option<RulesTarget> {
        let binding = self.binding.as_ref()?;
        let chat = self.chat.as_ref()?;
        let project = binding.manager.project_id.as_ref()?;
        if binding.manager.thread_id.as_ref()? != &chat.thread_id
            || chat.project_id.as_ref() != Some(project)
        {
            return None;
        }
        Some(RulesTarget {
            origin: self.environment.as_ref()?.origin(),
            project_id: project.clone(),
            thread_id: chat.thread_id.clone(),
        })
    }

    /// Build a worker only on explicit click. The job owns the original client,
    /// origin, project, thread and journal; it never reads a later UI selection.
    pub(crate) fn rules_send_job(
        &self,
        refresh: &RulesRefresh,
        clicked: &RulesOffer,
        cx: &App,
    ) -> Result<RulesJob, &'static str> {
        if !self.active
            || self.busy
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
        {
            return Err("Open the connected Manager before sending the rules request");
        }
        let selected = self
            .rules_target()
            .ok_or("Manager identity is not confirmed")?;
        // A retained ambiguous attempt retries its exact journal record below.
        let mut check = RulesRefresh::default();
        let text = if refresh.pending().is_some_and(|r| r.offer == *clicked) {
            check.saved(clicked.clone())?;
            check.message(clicked, &selected)?
        } else {
            refresh.message(clicked, &selected)?
        };
        let area = self.area.clone().ok_or("Missing project area")?;
        let binding = self.binding.clone().ok_or("Missing Manager binding")?;
        let state = self
            .state
            .clone()
            .ok_or("Durable application state is unavailable")?;
        let environment = self
            .environment
            .clone()
            .ok_or("Missing Manager environment")?;
        let offer = clicked.clone();
        let retry_id = refresh.pending().map(|receipt| receipt.request_id.clone());
        let validate_binding = move || -> anyhow::Result<()> {
            let current = okilum_t3::projects::Project::read(&area)?.native_binding()?;
            anyhow::ensure!(
                current.as_ref() == Some(&binding),
                "Project Manager binding changed; nothing was sent"
            );
            Ok(())
        };
        #[cfg(native_manager)]
        if self.native.active() {
            let intent = Intent::message(text.clone(), DispatchMode::QueueAfterActive);
            if self.native.deleted()
                || !self.native.stream_error_is_clear()
                || !self.native.actions.read(cx).state.allows(&intent)
            {
                return Err("Manager is not ready for a queued rules request");
            }
            let (client, _) = self.native.connection().ok_or("Manager is offline")?;
            if !client.can_operate() {
                return Err("Manager connection cannot send");
            }
            return Ok(Box::new(move || {
                let mut delivery = RulesDelivery {
                    receipt: None,
                    acknowledged: false,
                    error: None,
                };
                let result = (|| -> anyhow::Result<()> {
                    validate_binding()?;
                    let journal = okilum_t3::actions::Journal::new(
                        &state,
                        &environment,
                        &offer.target.thread_id,
                    );
                    // Restore the exact original operation after uncertain delivery
                    // or restart. Never mint another identity for this revision.
                    let existing = journal.records()?.into_iter().find(|r| {
                        matches!(&r.command.intent, Intent::Message { text: retained, .. } if retained == &text)
                    });
                    let record = match existing {
                        Some(record) => {
                            validate_rules_retry(
                                retry_id.as_deref(),
                                Some(&record.command.command_id),
                            )
                            .map_err(anyhow::Error::msg)?;
                            record
                        }
                        None => {
                            // Reconnection can change the available protocol. A
                            // retained MCP request must never become a new native
                            // command, including after the Reader was reopened.
                            let legacy =
                                MessageJournal::new(&state, &environment, &offer.target.thread_id);
                            if let Some(record) = legacy
                                .records()?
                                .into_iter()
                                .find(|r| r.message.text == text)
                            {
                                validate_rules_retry(
                                    retry_id.as_deref(),
                                    Some(&record.message.client_request_id),
                                )
                                .map_err(anyhow::Error::msg)?;
                                delivery.receipt = Some(RulesDispatch {
                                    offer: offer.clone(),
                                    request_id: record.message.client_request_id,
                                });
                                delivery.acknowledged = record.receipt.is_some();
                                anyhow::ensure!(delivery.acknowledged,
                                    "This rules request is retained in the MCP transport; reconnect that transport to retry the original message");
                                return Ok(());
                            }
                            validate_rules_retry(retry_id.as_deref(), None)
                                .map_err(anyhow::Error::msg)?;
                            journal.prepare(intent)?
                        }
                    };
                    delivery.receipt = Some(RulesDispatch {
                        offer,
                        request_id: record.command.command_id.clone(),
                    });
                    let sent = journal.send(&record, |command| client.dispatch(command))?;
                    delivery.acknowledged = sent.sequence.is_some();
                    Ok(())
                })();
                delivery.error = result.err().map(|e| format!("{e:#}"));
                delivery
            }));
        }
        #[cfg(native_manager)]
        if self.threads.enabled {
            return Err("Pair the Manager connection before sending");
        }
        let gateway = self.gateway.clone().ok_or("Manager is offline")?;
        if !matches!(
            gateway.status(),
            GatewayStatus::Connected { .. } | GatewayStatus::ExpiresSoon { .. }
        ) {
            return Err("Manager is offline");
        }
        Ok(Box::new(move || {
            let mut delivery = RulesDelivery {
                receipt: None,
                acknowledged: false,
                error: None,
            };
            let result = (|| -> anyhow::Result<()> {
                validate_binding()?;
                let page = gateway
                    .read_thread(&okilum_t3::ThreadQuery::messages(&offer.target.thread_id))?;
                anyhow::ensure!(
                    page.thread.project_id == offer.target.project_id
                        && page.thread.thread_id == offer.target.thread_id,
                    "Manager belongs to another project"
                );
                let journal = MessageJournal::new(&state, &environment, &offer.target.thread_id);
                let records = journal.records()?;
                let existing = records.iter().find(|r| r.message.text == text).cloned();
                let record = match existing {
                    Some(record) => {
                        validate_rules_retry(
                            retry_id.as_deref(),
                            Some(&record.message.client_request_id),
                        )
                        .map_err(anyhow::Error::msg)?;
                        record
                    }
                    None => {
                        let native = okilum_t3::actions::Journal::new(
                            &state,
                            &environment,
                            &offer.target.thread_id,
                        );
                        if let Some(record) = native.records()?.into_iter().find(|r| {
                            matches!(&r.command.intent, Intent::Message { text: retained, .. } if retained == &text)
                        }) {
                            validate_rules_retry(retry_id.as_deref(), Some(&record.command.command_id)).map_err(anyhow::Error::msg)?;
                            delivery.receipt = Some(RulesDispatch { offer: offer.clone(), request_id: record.command.command_id });
                            delivery.acknowledged = record.sequence.is_some();
                            anyhow::ensure!(delivery.acknowledged,
                                "This rules request is retained in the native transport; reconnect that transport to retry the original command");
                            return Ok(());
                        }
                        validate_rules_retry(retry_id.as_deref(), None)
                            .map_err(anyhow::Error::msg)?;
                        anyhow::ensure!(
                            !records.iter().any(|r| r.receipt.is_none()),
                            "Resolve the retained Manager message before another request"
                        );
                        journal.prepare(&text)?
                    }
                };
                delivery.receipt = Some(RulesDispatch {
                    offer,
                    request_id: record.message.client_request_id.clone(),
                });
                delivery.acknowledged = journal.send(gateway.as_ref(), &record)?.receipt.is_some();
                Ok(())
            })();
            delivery.error = result.err().map(|e| format!("{e:#}"));
            delivery
        }))
    }
}
