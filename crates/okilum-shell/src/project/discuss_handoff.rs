//! Board Discuss coordinator. The owner supplies the currently visible port;
//! this module never selects a hidden composer or resolves a thread by title.
use okilum_t3::draft_handoff::{Acknowledgement, ComposerPort, Refusal, Request, Target};

#[derive(Default)]
pub struct DiscussHandoff {
    visible: Option<Target>,
    pending: Option<Request>,
    next_id: u64,
}

impl DiscussHandoff {
    /// Call on project/thread/panel/access lifecycle changes. Invalidate before
    /// scheduling asynchronous work. A replaced instance needs a new generation.
    pub fn set_visible(&mut self, target: Option<Target>) {
        if self.visible != target {
            self.pending = None;
            self.visible = target;
        }
    }

    /// Capture the originating Board project before opening the Manager panel.
    /// An in-flight request is never silently replaced or retargeted.
    pub fn begin(&mut self, project: &str, text: String) -> Result<Request, Refusal> {
        let target = self.visible.as_ref().ok_or(Refusal::Unavailable)?;
        if target.project != project {
            return Err(Refusal::WrongTarget);
        }
        if text.trim().is_empty() {
            return Err(Refusal::Empty);
        }
        if self.pending.is_some() {
            return Err(Refusal::Busy);
        }
        self.next_id = self.next_id.checked_add(1).ok_or(Refusal::Exhausted)?;
        let request = Request {
            id: self.next_id,
            target: target.clone(),
            text,
        };
        self.pending = Some(request.clone());
        Ok(request)
    }

    /// Synchronous native/supported bridge adapter. An asynchronous webview uses
    /// begin + acknowledge instead, retaining the exact request through its IPC.
    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
    pub fn deliver(
        &mut self,
        project: &str,
        text: String,
        port: &mut impl ComposerPort,
    ) -> Result<(), Refusal> {
        let request = self.begin(project, text)?;
        match port.append(&request) {
            Ok(ack) => self.acknowledge(ack),
            Err(error) => {
                self.pending = None;
                Err(error)
            }
        }
    }

    /// Only this success may be presented as "added to draft". Late, foreign or
    /// replaced-thread acknowledgements cannot confirm the current action.
    pub fn acknowledge(&mut self, ack: Acknowledgement) -> Result<(), Refusal> {
        let request = self.pending.as_ref().ok_or(Refusal::StaleAcknowledgement)?;
        if self.visible.as_ref() != Some(&ack.target)
            || request.target != ack.target
            || request.id != ack.id
        {
            return Err(Refusal::StaleAcknowledgement);
        }
        self.pending = None;
        Ok(())
    }

    /// Cancel/refuse an exact outstanding bridge request without cancelling a
    /// newer action. Timeout is not success and must not trigger blind resend.
    pub fn cancel(&mut self, request: &Request) {
        if self.pending.as_ref() == Some(request) {
            self.pending = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discuss_1784_visible_port_receives_exact_draft_without_send() {
        use okilum_t3::draft_handoff::{appended_text, Receiver};
        struct Port {
            target: Target,
            receiver: Receiver,
            draft: String,
            edits: usize,
        }
        impl ComposerPort for Port {
            fn append(&mut self, request: &Request) -> Result<Acknowledgement, Refusal> {
                self.receiver.apply(Some(&self.target), request, |text| {
                    self.draft = appended_text(&self.draft, text);
                    self.edits += 1;
                    Ok(())
                })
            }
        }
        let target = Target {
            project: "p".into(),
            thread: "visible".into(),
            generation: 1,
        };
        let mut port = Port {
            target: target.clone(),
            receiver: Receiver::default(),
            draft: "draft".into(),
            edits: 0,
        };
        let mut handoff = DiscussHandoff::default();
        handoff.set_visible(Some(target));
        handoff
            .deliver("p", "context 🧠".into(), &mut port)
            .unwrap();
        assert_eq!(port.draft, "draft\n\ncontext 🧠");
        assert_eq!(port.edits, 1);
        // The port exposes no send operation; a subsequent action is unblocked
        // only after the matching acknowledgement returned from the insertion.
        assert!(handoff.begin("p", "next".into()).is_ok());
    }

    #[test]
    fn discuss_1784_replaced_thread_and_revocation_reject_late_ack() {
        let mut handoff = DiscussHandoff::default();
        let target = Target {
            project: "p".into(),
            thread: "t".into(),
            generation: 1,
        };
        handoff.set_visible(Some(target.clone()));
        assert_eq!(
            handoff.begin("other", "context".into()),
            Err(Refusal::WrongTarget)
        );
        let old = handoff.begin("p", "context".into()).unwrap();
        handoff.set_visible(None);
        assert_eq!(
            handoff.acknowledge(Acknowledgement {
                id: old.id,
                target: target.clone()
            }),
            Err(Refusal::StaleAcknowledgement)
        );
        handoff.set_visible(Some(Target {
            generation: 2,
            ..target
        }));
        let current = handoff.begin("p", "new".into()).unwrap();
        assert_eq!(
            handoff.acknowledge(Acknowledgement {
                id: old.id,
                target: old.target
            }),
            Err(Refusal::StaleAcknowledgement)
        );
        handoff
            .acknowledge(Acknowledgement {
                id: current.id,
                target: current.target,
            })
            .unwrap();
    }
}

#[cfg(test)]
pub(super) mod native_tests;
