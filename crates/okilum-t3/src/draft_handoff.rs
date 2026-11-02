//! Draft-only handoff protocol shared by native and webview composers.
//! No gateway send, journal intent or executor operation belongs in this port.

/// Identity of the *visible* composer. `generation` must change on project,
/// thread, webview navigation/replacement, or access changes, including A → B → A.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub project: String,
    pub thread: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    pub target: Target,
    pub text: String,
}

/// Emitted only after the exact composer has committed one undoable insertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acknowledgement {
    pub id: u64,
    pub target: Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Unavailable,
    WrongTarget,
    Empty,
    Busy,
    StaleAcknowledgement,
    Exhausted,
}

/// Implementations must compare the target and access immediately before the
/// insertion, atomically with it. Preserve draft bytes and use one native input
/// edit (or the supported web composer transaction), retaining ordinary Undo.
/// Never fall back to a hidden native composer, DOM selector, or send operation.
pub trait ComposerPort {
    fn append(&mut self, request: &Request) -> Result<Acknowledgement, Refusal>;
}

/// Separator shared by both adapters; the existing text is never trimmed.
pub fn appended_text(existing: &str, block: &str) -> String {
    if existing.is_empty() {
        block.to_owned()
    } else {
        format!("{existing}\n\n{block}")
    }
}

/// A receiver-side replay guard, scoped to one composer instance. Retain it for
/// that instance's lifetime; the sender allocates monotonically increasing IDs.
#[derive(Default)]
pub struct Receiver {
    last: Option<Request>,
}

impl Receiver {
    /// `insert` performs the single undoable append. On failure it must leave the
    /// composer unchanged. Replays return the original acknowledgement, no edit.
    pub fn apply(
        &mut self,
        visible: Option<&Target>,
        request: &Request,
        insert: impl FnOnce(&str) -> Result<(), Refusal>,
    ) -> Result<Acknowledgement, Refusal> {
        if visible != Some(&request.target) {
            return Err(Refusal::WrongTarget);
        }
        if request.text.trim().is_empty() {
            return Err(Refusal::Empty);
        }
        if let Some(last) = &self.last {
            if last == request {
                return Ok(Acknowledgement {
                    id: request.id,
                    target: request.target.clone(),
                });
            }
            if request.id <= last.id {
                return Err(Refusal::StaleAcknowledgement);
            }
        }
        insert(&request.text)?;
        self.last = Some(request.clone());
        Ok(Acknowledgement {
            id: request.id,
            target: request.target.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_target_atomic_append_and_replay() {
        let target = Target {
            project: "p".into(),
            thread: "t".into(),
            generation: 1,
        };
        let request = Request {
            id: 1,
            target: target.clone(),
            text: "Контекст 🧠".into(),
        };
        let mut receiver = Receiver::default();
        let mut draft = "existing\n".to_owned();
        let before = draft.clone();
        let ack = receiver
            .apply(Some(&target), &request, |text| {
                draft = appended_text(&draft, text);
                Ok(())
            })
            .unwrap();
        assert_eq!(draft, "existing\n\n\nКонтекст 🧠");
        assert_eq!(ack.target, target);
        receiver
            .apply(Some(&target), &request, |_| panic!("replay edited draft"))
            .unwrap();
        let replaced = Target {
            generation: 2,
            ..target.clone()
        };
        assert_eq!(
            receiver.apply(Some(&replaced), &request, |_| panic!("wrong composer")),
            Err(Refusal::WrongTarget)
        );
        assert_eq!(
            receiver.apply(None, &request, |_| panic!("revoked access")),
            Err(Refusal::WrongTarget)
        );
        // A real adapter owns Undo; here the edit's complete preimage is exact.
        draft = before;
        assert_eq!(draft, "existing\n");
    }
}
