//! Exact, draft-only rotation handoff. Parent wires this into the selected
//! project's composer; no send, launch, registry edit or scheduled task occurs.
use okilum_t3::chat_drafts::{Attachment, Draft};
use okilum_t3::rotation::Owner;

#[derive(Clone)]
pub(super) struct Handoff {
    owner: Owner,
    captured: Draft<Attachment>,
    replacement: Draft<Attachment>,
}
impl Handoff {
    pub(super) fn capture(
        owner: Owner,
        current: Draft<Attachment>,
        exact_text: String,
    ) -> Result<Self, &'static str> {
        if exact_text.is_empty() {
            return Err("Rotation draft is empty");
        }
        // Never overwrite an existing composer, including attachment-only work.
        if !current.text.is_empty() || !current.attachments.is_empty() {
            return Err("Keep or clear the current draft before preparing rotation");
        }
        Ok(Self {
            owner,
            captured: current,
            replacement: Draft {
                text: exact_text,
                attachments: Vec::new(),
            },
        })
    }
    /// Compare again at application time, after any asynchronous preview work.
    pub(super) fn apply(
        self,
        owner: &Owner,
        current: &Draft<Attachment>,
    ) -> Result<Draft<Attachment>, &'static str> {
        if &self.owner != owner {
            return Err("The selected project or manager changed");
        }
        if current != &self.captured {
            return Err("The composer changed; draft preserved");
        }
        Ok(self.replacement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner() -> Owner {
        Owner {
            origin: "fixture".into(),
            project: "p".into(),
            manager: "m".into(),
            revision: 1,
        }
    }
    #[test]
    fn exact_handoff_preserves_edits_and_rejects_stale_selection() {
        let text = "Rotation preview\n🧠  keep spaces  \n";
        let o = owner();
        let handoff = || Handoff::capture(o.clone(), Draft::default(), text.into()).unwrap();
        let changed = Draft {
            text: "new user edit".into(),
            attachments: vec![],
        };
        assert!(handoff().apply(&o, &changed).is_err());
        let mut stale = o.clone();
        stale.revision += 1;
        assert!(handoff().apply(&stale, &Draft::default()).is_err());
        assert_eq!(handoff().apply(&o, &Draft::default()).unwrap().text, text);
        assert!(Handoff::capture(o, changed, text.into()).is_err());
    }
}
