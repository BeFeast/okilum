//! Reader setup state: unresolved ownership must keep the user's unsent draft.
use okilum_t3::{projects::local_binding::Selection, ManagerGateway, Project};

#[derive(Default)]
pub(crate) struct LocalProjectBinding {
    pub draft: String,
    pub address: String,
    pub checkout: String,
    selection: Option<Selection>,
    pub diagnostic: Option<String>,
}

impl LocalProjectBinding {
    /// Called only by the explicit project picker, never by a path/host heuristic.
    pub fn select(&mut self, selection: Selection) {
        self.selection = Some(selection);
        self.diagnostic = None;
    }

    /// Return a verified owner for the caller's revision-aware binding operation.
    /// A stale selection is invalidated, while all form inputs and drafts survive.
    pub fn verify(&mut self, gateway: &dyn ManagerGateway) -> Option<Project> {
        let result = self
            .selection
            .as_ref()
            .ok_or_else(|| "Owner unresolved: select a verified local T3 project".to_owned())
            .and_then(|selection| {
                selection
                    .verify(gateway)
                    .map_err(|error| format!("{error:#}"))
            });
        match result {
            Ok(project) => {
                self.diagnostic = None;
                Some(project)
            }
            Err(error) => {
                self.selection = None;
                self.diagnostic = Some(error);
                None
            }
        }
    }

    /// Changing connection inputs revokes the previous selection immediately.
    pub fn connection_changed(&mut self, address: String, checkout: String) {
        self.address = address;
        self.checkout = checkout;
        self.selection = None;
        self.diagnostic =
            Some("Owner unresolved: check the connection and select a project".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_connection_preserves_draft_and_revokes_owner() {
        let mut state = LocalProjectBinding {
            draft: "unsent context".into(),
            ..Default::default()
        };
        state.select(Selection {
            environment_id: "verified".into(),
            project_id: "explicit".into(),
            checkout: "/checkout".into(),
        });
        state.connection_changed("http://localhost:3773".into(), "/other-checkout".into());
        assert_eq!(state.draft, "unsent context");
        assert!(state.selection.is_none());
        assert!(state
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("Owner unresolved"));
    }
}
