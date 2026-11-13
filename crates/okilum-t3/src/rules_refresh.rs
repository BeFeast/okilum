//! Explicit rules refresh offers. Saving only creates an offer; dispatch is a
//! separate user action through the existing Manager message pipeline.

/// Retried delivery must find the exact original durable identity. Absence or
/// a different transport's identity cannot authorize preparation of a new send.
pub fn validate_rules_retry(
    expected: Option<&str>,
    retained: Option<&str>,
) -> Result<(), &'static str> {
    match (expected, retained) {
        (Some(expected), Some(actual)) if expected == actual => Ok(()),
        (None, _) => Ok(()),
        (Some(_), None) => Err("Original rules request is missing; nothing was sent"),
        _ => Err("Retained rules request identity changed; nothing was sent"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RulesTarget {
    pub origin: String,
    pub project_id: String,
    pub thread_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RulesRevision {
    /// Canonical vault-relative source identity, not a guessed server path.
    pub path: String,
    /// Revision returned by the successful source save, never an editor buffer.
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RulesOffer {
    pub target: RulesTarget,
    pub source: RulesRevision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RulesDispatch {
    pub offer: RulesOffer,
    /// Receipt from existing durable message preparation (command/request ID).
    pub request_id: String,
}

#[derive(Clone, Debug, Default)]
pub struct RulesRefresh {
    offer: Option<RulesOffer>,
    preparing: Option<RulesOffer>,
    pending: Option<RulesDispatch>,
    acknowledged: Option<RulesDispatch>,
}

impl RulesRefresh {
    /// Call only after successful source save and resolved project ownership.
    /// Unchanged saves do not generate a new offer. Unsupported/ambiguous
    /// ownership must be surfaced by the caller rather than choosing a Manager.
    pub fn saved(&mut self, offer: RulesOffer) -> Result<bool, &'static str> {
        if [
            &offer.target.origin,
            &offer.target.project_id,
            &offer.target.thread_id,
            &offer.source.path,
            &offer.source.revision,
        ]
        .iter()
        .any(|s| s.trim().is_empty())
        {
            return Err("Rules refresh requires an exact target and saved revision");
        }
        // The caller decides project scope from the Rules stack; any of its
        // Markdown layers (AGENTS.md, CLAUDE.md, SKILL.md, imports) qualifies.
        if !offer.source.path.to_ascii_lowercase().ends_with(".md") {
            return Err("Rules refresh source must be a Markdown agent file");
        }
        if self.offer.as_ref() == Some(&offer) {
            return Ok(false);
        }
        self.offer = Some(offer);
        Ok(true)
    }

    pub fn offer(&self) -> Option<&RulesOffer> {
        self.offer.as_ref()
    }
    pub fn pending(&self) -> Option<&RulesDispatch> {
        self.pending.as_ref()
    }
    pub fn acknowledged(&self) -> Option<&RulesDispatch> {
        self.acknowledged.as_ref()
    }

    /// Validate the actual clicked offer against current selection before
    /// preparing any durable send. A stale click must not target a new Manager.
    pub fn message(
        &self,
        clicked: &RulesOffer,
        selected: &RulesTarget,
    ) -> Result<String, &'static str> {
        if self.offer.as_ref() != Some(clicked) || &clicked.target != selected {
            return Err("Rules refresh offer or Manager selection changed");
        }
        if self.pending.is_some() {
            return Err("Resolve the retained rules refresh send before another request");
        }
        if self
            .acknowledged
            .as_ref()
            .is_some_and(|receipt| &receipt.offer == clicked)
        {
            return Err("Rules refresh message was already acknowledged");
        }
        // Quote identities as data so newlines cannot impersonate instructions.
        Ok(format!("Please re-read your rules: AGENTS.md and the files it imports. Okilum saved source {:?} at revision {:?} in project {:?}. Re-read the current source and report the revision you actually read; this message receipt alone does not confirm rules were reloaded.",
            clicked.source.path, clicked.source.revision, clicked.target.project_id))
    }

    /// Install only after the existing journal successfully prepares the exact
    /// message. Keep this receipt on ambiguous transport failure; Retry must use
    /// the journal's original request rather than preparing a new message.
    pub fn prepared(
        &mut self,
        clicked: &RulesOffer,
        selected: &RulesTarget,
        request_id: String,
    ) -> Result<RulesDispatch, &'static str> {
        self.message(clicked, selected)?;
        if request_id.trim().is_empty() {
            return Err("Missing durable send identity");
        }
        let dispatch = RulesDispatch {
            offer: clicked.clone(),
            request_id,
        };
        self.pending = Some(dispatch.clone());
        Ok(dispatch)
    }

    /// Capture the clicked offer before its worker starts. A subsequent save
    /// may replace the visible offer without erasing this operation's recovery.
    pub fn begin_delivery(&mut self, clicked: &RulesOffer) -> Result<(), &'static str> {
        if self.preparing.is_some() {
            return Err("Rules delivery is already in progress");
        }
        if let Some(pending) = &self.pending {
            if &pending.offer != clicked {
                return Err("Retry the original retained rules request first");
            }
        } else {
            self.message(clicked, &clicked.target)?;
        }
        self.preparing = Some(clicked.clone());
        Ok(())
    }

    pub fn end_delivery(&mut self, clicked: &RulesOffer) {
        if self.preparing.as_ref() == Some(clicked) {
            self.preparing = None;
        }
    }

    /// Apply a worker result using its captured durable receipt. A save may
    /// replace the visible offer while delivery is in flight: complete a known
    /// old pending receipt, but never install it as the new offer's receipt.
    pub fn record_delivery(&mut self, receipt: &RulesDispatch, acknowledged: bool) -> bool {
        if self.pending.as_ref() == Some(receipt) {
            if acknowledged {
                self.acknowledge(receipt);
            }
            return true;
        }
        if self.pending.is_some() {
            return false;
        }
        if self.preparing.as_ref() == Some(&receipt.offer) {
            if receipt.request_id.trim().is_empty() {
                return false;
            }
            self.pending = Some(receipt.clone());
            if acknowledged {
                self.acknowledge(receipt);
            }
            return true;
        }
        if self.offer.as_ref() != Some(&receipt.offer) {
            return false;
        }
        if self
            .prepared(
                &receipt.offer,
                &receipt.offer.target,
                receipt.request_id.clone(),
            )
            .is_err()
        {
            return false;
        }
        if acknowledged {
            self.acknowledge(receipt);
        }
        true
    }

    /// Queue acceptance only, not proof that the Manager re-read its rules.
    /// Late acknowledgements must carry the complete original dispatch receipt.
    pub fn acknowledge(&mut self, receipt: &RulesDispatch) -> bool {
        if self.pending.as_ref() != Some(receipt) {
            return false;
        }
        self.pending = None;
        self.acknowledged = Some(receipt.clone());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn offer(revision: &str) -> RulesOffer {
        RulesOffer {
            target: RulesTarget {
                origin: "https://t3.example".into(),
                project_id: "project".into(),
                thread_id: "manager".into(),
            },
            source: RulesRevision {
                path: "area/AGENTS.md".into(),
                revision: revision.into(),
            },
        }
    }
    #[test]
    fn save_offers_without_sending_and_click_keeps_exact_revision_and_target() {
        let mut state = RulesRefresh::default();
        let saved = offer("rev-2");
        assert!(state.saved(saved.clone()).unwrap());
        assert!(!state.saved(saved.clone()).unwrap());
        assert!(state.pending().is_none());
        let mut elsewhere = saved.target.clone();
        elsewhere.thread_id = "other-manager".into();
        assert!(state.message(&saved, &elsewhere).is_err());
        assert!(state.message(&offer("rev-1"), &saved.target).is_err());
        let text = state.message(&saved, &saved.target).unwrap();
        assert!(text.contains("area/AGENTS.md") && text.contains("rev-2"));
        let receipt = state
            .prepared(&saved, &saved.target, "request-1".into())
            .unwrap();
        assert!(state.message(&saved, &saved.target).is_err());
        assert!(state.acknowledge(&receipt));
        assert!(state.message(&saved, &saved.target).is_err());
        assert!(!state.acknowledge(&receipt));
    }
    #[test]
    fn any_markdown_agent_file_in_scope_can_be_offered() {
        for path in [
            "area/CLAUDE.md",
            "_AgentContract.md",
            "area/skills/x/SKILL.md",
            "docs/style.md",
        ] {
            let mut saved = offer("rev");
            saved.source.path = path.into();
            let mut state = RulesRefresh::default();
            assert!(state.saved(saved.clone()).unwrap(), "{path}");
            assert!(state.message(&saved, &saved.target).unwrap().contains(path));
        }
        let mut binary = offer("rev");
        binary.source.path = "area/rules.bin".into();
        assert!(RulesRefresh::default().saved(binary).is_err());
    }
    #[test]
    fn old_acknowledgement_cannot_consume_new_revision_or_new_request() {
        let mut state = RulesRefresh::default();
        let first = offer("first");
        state.saved(first.clone()).unwrap();
        let old = state
            .prepared(&first, &first.target, "request-1".into())
            .unwrap();
        let second = offer("second");
        state.saved(second.clone()).unwrap();
        assert!(state.acknowledge(&old));
        assert_eq!(state.offer(), Some(&second));
        let new = state
            .prepared(&second, &second.target, "request-2".into())
            .unwrap();
        assert!(!state.acknowledge(&old));
        assert_eq!(state.pending(), Some(&new));
        assert!(state.acknowledge(&new));
    }
    #[test]
    fn worker_receipts_keep_failed_retry_identity_and_do_not_replace_new_save() {
        let mut state = RulesRefresh::default();
        let first = offer("first");
        state.saved(first.clone()).unwrap();
        let receipt = RulesDispatch {
            offer: first.clone(),
            request_id: "retained-request".into(),
        };
        assert!(state.record_delivery(&receipt, false));
        assert_eq!(state.pending(), Some(&receipt));
        let mut wrong = receipt.clone();
        wrong.request_id = "unrelated-request".into();
        assert!(!state.record_delivery(&wrong, true));
        assert_eq!(state.pending(), Some(&receipt));
        let second = offer("second");
        state.saved(second.clone()).unwrap();
        assert!(state.record_delivery(&receipt, true));
        assert!(state.pending().is_none());
        assert_eq!(state.offer(), Some(&second));
        assert!(state.message(&second, &second.target).is_ok());
        assert!(!state.record_delivery(&receipt, true));
        assert_eq!(state.offer(), Some(&second));
    }
    #[test]
    fn retry_cannot_be_recreated_or_switched_to_another_transport_identity() {
        assert!(validate_rules_retry(Some("native-command-1"), Some("native-command-1")).is_ok());
        assert!(validate_rules_retry(Some("native-command-1"), Some("mcp-request-2")).is_err());
        assert!(validate_rules_retry(Some("mcp-request-1"), None).is_err());
        assert!(validate_rules_retry(None, Some("retained-before-restart")).is_ok());
        assert!(
            validate_rules_retry(None, None).is_ok(),
            "new explicit send positive control"
        );
    }
    #[test]
    fn failed_inflight_old_revision_remains_retryable_after_a_new_save() {
        let mut state = RulesRefresh::default();
        let old = offer("old");
        state.saved(old.clone()).unwrap();
        state.begin_delivery(&old).unwrap();
        let latest = offer("latest");
        state.saved(latest.clone()).unwrap();
        let receipt = RulesDispatch {
            offer: old.clone(),
            request_id: "original-command".into(),
        };
        assert!(state.record_delivery(&receipt, false));
        state.end_delivery(&old);
        assert_eq!(state.pending(), Some(&receipt));
        assert_eq!(state.offer(), Some(&latest));
        assert!(state.begin_delivery(&latest).is_err());
        state.begin_delivery(&old).unwrap();
        assert!(state.record_delivery(&receipt, true));
        state.end_delivery(&old);
        assert!(state.pending().is_none());
        assert!(state.message(&latest, &latest.target).is_ok());
    }
}
