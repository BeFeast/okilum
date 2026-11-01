//! Exact, draft-only rotation handoff. Parent wires this into the selected
//! project's composer; no send, launch, registry edit or scheduled task occurs.
use okilum_t3::chat_drafts::{Attachment, Draft};
use okilum_t3::rotation::{Blocker, Checkpoint, Owner, PolicyStore, TzifResolver};

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

/// Prepare a recovery draft only after its exact owner/window claim is durable.
/// Parent applies the handoff, flushes the existing draft writer, and ONLY then
/// calls PolicyStore::acknowledge(checkpoint.revision, owner). A crash or a stale
/// composer leaves the pending claim visible for reconciliation after restart.
pub(super) fn prepare_recovery(
    store: &PolicyStore,
    expected_revision: u64,
    owner: &Owner,
    current: Draft<Attachment>,
    exact_text: String,
    now: i64,
    resolver: &TzifResolver,
    blocker: Option<Blocker>,
) -> Result<(Handoff, Checkpoint), String> {
    let handoff = Handoff::capture(owner.clone(), current, exact_text)?;
    let claim = store.claim(expected_revision, owner, now, resolver, blocker)?;
    Ok((handoff, claim))
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
    fn recovery_claim_is_durable_and_busy_composer_does_not_claim() {
        use okilum_t3::rotation::{Policy, Recovery};
        let path = std::env::temp_dir().join(format!("rotation-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let store = PolicyStore::new(&path);
        let resolver = TzifResolver::new("/usr/share/zoneinfo");
        let owner = owner();
        let start = 20758 * 86400;
        store
            .configure(
                0,
                owner.clone(),
                Policy::new(true, "Asia/Jerusalem", ["08:30", "20:30"]).unwrap(),
                start,
                &resolver,
            )
            .unwrap();
        let busy = Draft {
            text: "user draft".into(),
            attachments: vec![],
        };
        let now = start + 22 * 3600;
        assert!(prepare_recovery(
            &store,
            1,
            &owner,
            busy,
            "exact request".into(),
            now,
            &resolver,
            None
        )
        .is_err());
        assert_eq!(store.load().unwrap().unwrap().revision, 1);
        let (handoff, checkpoint) = prepare_recovery(
            &store,
            1,
            &owner,
            Draft::default(),
            "exact request\n".into(),
            now,
            &resolver,
            None,
        )
        .unwrap();
        assert_eq!(
            handoff.apply(&owner, &Draft::default()).unwrap().text,
            "exact request\n"
        );
        assert!(matches!(
            store
                .load()
                .unwrap()
                .unwrap()
                .recover(&owner, now, &resolver, None)
                .unwrap(),
            Recovery::NeedsReconciliation(_)
        ));
        store.acknowledge(checkpoint.revision, &owner).unwrap();
        std::fs::remove_dir_all(path).unwrap();
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
