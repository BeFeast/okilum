//! Read-only Skills section. The owner supplies the existing Reader open callback.
use gpui::{div, prelude::*, App, IntoElement, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    Disableable as _,
};
use okilum_core::agent_skills::{self, Availability, Freshness, Inventory, Snapshot};
use std::path::PathBuf;

/// Render the session inventory. The owner runs RefreshRequest::discover off the
/// UI thread and calls finish_refresh on completion, then notifies the view.
/// Refresh reloads configuration first; configuration_failed blocks old-policy scans.
/// Open errors must call Inventory::invalidate and use the owner's visible notice.
pub(super) fn render(
    inventory: &Inventory,
    on_refresh: impl Fn(&mut Window, &mut App) + 'static,
    on_open: impl Fn(Result<PathBuf, String>, &mut Window, &mut App) + Clone + 'static,
) -> impl IntoElement {
    let root = inventory.scope().map(|scope| scope.root.clone());
    let empty = Snapshot::default();
    let snapshot = inventory.snapshot().unwrap_or(&empty);
    let historical = !matches!(
        inventory.freshness(),
        Freshness::Fresh | Freshness::Incomplete
    );
    let message = match inventory.freshness() {
        Freshness::NotLoaded => "Project skills have not been loaded".to_owned(),
        Freshness::Refreshing if inventory.snapshot().is_some() => {
            "Refreshing skills; previous results are stale".to_owned()
        }
        Freshness::Refreshing => "Loading project skills".to_owned(),
        Freshness::Fresh if snapshot.skills.is_empty() => "No project skills found".to_owned(),
        Freshness::Fresh => "Project skills are up to date".to_owned(),
        Freshness::Incomplete => {
            "Skill discovery is incomplete; availability is unconfirmed".to_owned()
        }
        Freshness::Stale(reason) => {
            format!("Skills are stale: {reason}. Refresh to update availability")
        }
        Freshness::Failed(error) => format!("Skill refresh failed: {error}. Refresh to retry"),
    };
    let mut section = div()
        .flex()
        .flex_col()
        .gap_2()
        .child("Skills")
        .child(message)
        .child(
            Button::new("refresh-project-skills")
                .ghost()
                .label("Refresh skills")
                .disabled(root.is_none())
                .on_click(move |_, window, cx| on_refresh(window, cx)),
        );
    for diagnostic in &snapshot.diagnostics {
        section = section.child(div().child(diagnostic.clone()));
    }
    for (index, skill) in snapshot.skills.iter().enumerate() {
        let status = match &skill.availability {
            Availability::Available => "Available from project scope".to_owned(),
            Availability::Disabled => "Disabled".to_owned(),
            Availability::Duplicate => "Duplicate name; no skill selected".to_owned(),
            Availability::Unavailable(reason) => format!("Unavailable: {reason}"),
        };
        let status = if historical {
            format!("Previous result: {status}; current availability is unconfirmed")
        } else if matches!(inventory.freshness(), Freshness::Incomplete) {
            format!("{status}; discovery is incomplete")
        } else {
            status
        };
        let source = skill.source.display().to_string();
        let root = root.clone();
        let skill_for_open = skill.clone();
        let on_open = on_open.clone();
        section = section.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(skill.name.clone())
                .child(skill.description.clone())
                .child(status)
                .child(source)
                .child(
                    Button::new(("open-project-skill", index))
                        .ghost()
                        .label("Open SKILL.md")
                        .on_click(move |_, window, cx| {
                            on_open(
                                root.as_ref()
                                    .ok_or_else(|| "No project selected".to_owned())
                                    .and_then(|root| {
                                        agent_skills::open_source(root, &skill_for_open)
                                    }),
                                window,
                                cx,
                            );
                        }),
                ),
        );
    }
    section
}
