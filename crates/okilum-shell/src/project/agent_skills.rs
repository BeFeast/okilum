//! Read-only Skills section. The owner supplies the existing Reader open callback.
use gpui::{div, prelude::*, App, IntoElement, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    Disableable as _,
};
use okilum_core::agent_skills::{self, Availability, Freshness, Inventory, Provider, Snapshot};
use std::path::PathBuf;

/// Render the session inventory. The owner runs RefreshRequest::discover off the
/// UI thread and calls finish_refresh on completion, then notifies the view.
/// Reload must publish the current scope and inventory revision through
/// Inventory::begin_reload(scope, inventory_revision), then complete with the same
/// request. Reload configuration first; configuration_failed blocks old-policy scans.
/// Open errors must call Inventory::invalidate and use the owner's visible notice.
pub(super) fn render<R: Clone + Eq>(
    inventory: &Inventory<R>,
    on_refresh: impl Fn(&mut Window, &mut App) + 'static,
    on_open: impl Fn(Result<PathBuf, String>, &mut Window, &mut App) + Clone + 'static,
) -> impl IntoElement {
    let root = inventory.scope().map(|scope| scope.root.clone());
    let empty = Snapshot::default();
    let snapshot = inventory.snapshot().unwrap_or(&empty);
    let historical = !matches!(inventory.freshness(), Freshness::Fresh);
    let message = match inventory.freshness() {
        Freshness::NotLoaded => "Project skills have not been loaded".to_owned(),
        Freshness::Refreshing if inventory.snapshot().is_some() => {
            "Refreshing skills; previous results are stale".to_owned()
        }
        Freshness::Refreshing => "Loading project skills".to_owned(),
        Freshness::Fresh if snapshot.skills.is_empty() => "No project skills found".to_owned(),
        Freshness::Fresh => "Project skills are up to date".to_owned(),
        Freshness::Incomplete => {
            "Skill reload is incomplete; last available listing shown; current availability is unconfirmed"
                .to_owned()
        }
        Freshness::Stale(reason) => {
            format!("Skills are stale: {reason}. Reload to update availability")
        }
        Freshness::Failed(error) => format!("Skill refresh failed: {error}. Reload to retry"),
    };
    let mut section = div()
        .flex()
        .flex_col()
        .gap_2()
        .child("Skills")
        .child(
            div()
                .debug_selector(|| "project-skills-status".into())
                .child(message),
        )
        .child("Local discovery only. Remote agent loading is unknown.")
        .child(".agents/skills: Codex and other agents. .claude/skills: Claude Code.")
        .child(
            Button::new("refresh-project-skills")
                .debug_selector(|| "refresh-project-skills".into())
                .ghost()
                .label("Reload skills")
                .disabled(root.is_none())
                .on_click(move |_, window, cx| on_refresh(window, cx)),
        );
    for (index, diagnostic) in inventory
        .reload_diagnostics()
        .iter()
        .chain(
            snapshot
                .diagnostics
                .iter()
                .filter(|diagnostic| !inventory.reload_diagnostics().contains(diagnostic)),
        )
        .enumerate()
    {
        section = section.child(
            div()
                .debug_selector(move || format!("project-skills-diagnostic-{index}"))
                .child(diagnostic.clone()),
        );
    }
    for (index, skill) in snapshot.skills.iter().enumerate() {
        let status = match &skill.availability {
            Availability::Available => "Available from project scope".to_owned(),
            Availability::Disabled => "Disabled".to_owned(),
            Availability::Duplicate => format!(
                "Duplicate name in {}; no skill selected",
                skill.provider.folder()
            ),
            Availability::Unavailable(reason) => format!("Unavailable: {reason}"),
        };
        let status = if historical {
            format!("Previous result: {status}; current availability is unconfirmed")
        } else {
            status
        };
        let mut row = div()
            .debug_selector(move || format!("project-skill-{index}"))
            .flex()
            .flex_col()
            .gap_1()
            .child(skill.name.clone())
            .child(skill.description.clone())
            .child(status);
        if skill.paired.as_ref().is_some_and(|paired| paired.differs) {
            row = row.child(
                div()
                    .debug_selector(move || format!("project-skill-{index}-differs"))
                    .child("The .agents and .claude copies of SKILL.md differ"),
            );
        }
        // One line and one Open action per exact source (#2938).
        for (source_index, (provider, source)) in skill.sources().enumerate() {
            let label = format!(
                "{} ({}: {})",
                source.display(),
                provider.folder(),
                provider.readers()
            );
            let root = root.clone();
            let source = source.to_path_buf();
            let on_open = on_open.clone();
            let selector = if source_index == 0 {
                format!("open-project-skill-{index}")
            } else {
                format!("open-project-skill-{index}-{source_index}")
            };
            row = row.child(
                div()
                    .debug_selector(move || format!("project-skill-{index}-source-{source_index}"))
                    .child(label),
            );
            row = row.child(
                Button::new((
                    "open-project-skill",
                    index * Provider::ALL.len() + source_index,
                ))
                .debug_selector(move || selector.clone())
                .ghost()
                .label(format!("Open {} SKILL.md", provider.folder()))
                .on_click(move |_, window, cx| {
                    on_open(
                        root.as_ref()
                            .ok_or_else(|| "No project selected".to_owned())
                            .and_then(|root| agent_skills::open_source(root, &source)),
                        window,
                        cx,
                    );
                }),
            );
        }
        section = section.child(row);
    }
    section
}

// Parent adapter: consume the Reader's actual immutable publication, never an
// invented sequence, a truncated metadata hash, or a worker checkpoint.
use super::{ProjectSection, ProjectView};
use crate::{reader_features, Reader, Vault};
use gpui::{Context, WeakEntity};
use okilum_core::agent_skills::{DiscoveryScope, RefreshRequest};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct Publication {
    vault: Arc<Vault>,
    owner_lease: Arc<()>,
    view_generation: u64,
    source_publication_epoch: u64,
}
impl PartialEq for Publication {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.vault, &other.vault)
            && Arc::ptr_eq(&self.owner_lease, &other.owner_lease)
            && self.view_generation == other.view_generation
            && self.source_publication_epoch == other.source_publication_epoch
    }
}
impl Eq for Publication {}

#[derive(Clone)]
pub(super) struct Owner {
    reader: WeakEntity<Reader>,
    root: PathBuf,
    lease: Arc<()>,
}
impl Owner {
    fn current(&self, generation: u64, cx: &App) -> Result<Publication, String> {
        let reader = self.reader.upgrade().ok_or("Project Reader is closed")?;
        let reader = reader.read(cx);
        if reader.vault_root != self.root || reader.vault.root != self.root {
            return Err("Project inventory belongs to a different vault".into());
        }
        Ok(Publication {
            vault: reader.vault.clone(),
            owner_lease: self.lease.clone(),
            view_generation: generation,
            source_publication_epoch: reader.source_publication_epoch,
        })
    }
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lease, &other.lease)
    }
}

pub(super) fn bind(view: &mut ProjectView, reader: WeakEntity<Reader>, root: PathBuf) {
    // Explicit binding only. This is not called by render, scroll or layout.
    if let Some(project) = view.area.clone() {
        view.skills.configure(DiscoveryScope {
            root: root.clone(),
            project,
            disabled: Vec::new(),
            config_revision: 0,
        });
    } else {
        view.skills.clear();
    }
    view.skills.invalidate("Project inventory owner changed");
    view.skills_owner = Some(Owner {
        reader,
        root,
        lease: Arc::new(()),
    });
}

fn enabled(view: &ProjectView, cx: &App) -> bool {
    view.active
        && view.section == ProjectSection::Skills
        && reader_features::enabled(reader_features::Feature::Agents, cx)
}
fn scope(view: &ProjectView) -> Option<DiscoveryScope> {
    Some(DiscoveryScope {
        root: view.resource_vault.clone()?,
        project: view.area.clone()?,
        // Existing local-only discovery policy. No provider/global configuration
        // or user-selected disabled paths are inferred by this adapter.
        disabled: Vec::new(),
        config_revision: 0,
    })
}
fn capture(view: &ProjectView, cx: &App) -> Result<(DiscoveryScope, Owner, Publication), String> {
    let scope = scope(view).ok_or("No local project inventory is bound")?;
    let owner = view
        .skills_owner
        .clone()
        .ok_or("No published project inventory owner")?;
    let publication = owner.current(view.generation, cx)?;
    if publication.vault.root != scope.root {
        return Err("Published project inventory root changed".into());
    }
    Ok((scope, owner, publication))
}

/// Called by the existing Reader publication observer, including an empty
/// inventory. Read from its CURRENT published Arc before invalidating workers.
pub(super) fn published(view: &mut ProjectView, cx: &App) {
    if !enabled(view, cx) {
        return;
    }
    match capture(view, cx) {
        Ok((scope, _, revision)) => {
            view.skills.configure_revision(scope, revision);
        }
        Err(error) => view.skills.configuration_failed(error),
    }
}

pub(super) fn display_current(
    view: &ProjectView,
    displayed_scope: Option<&DiscoveryScope>,
    displayed_revision: Option<&Publication>,
    cx: &App,
) -> bool {
    enabled(view, cx)
        && capture(view, cx).is_ok_and(|(scope, _, revision)| {
            displayed_scope == Some(&scope)
                && displayed_revision == Some(&revision)
                && view.skills.scope() == Some(&scope)
                && view.skills.inventory_revision() == Some(&revision)
        })
}

struct Ticket {
    request: RefreshRequest<Publication>,
    owner: Owner,
    revision: Publication,
    generation: u64,
}
fn begin(view: &mut ProjectView, cx: &App) -> Option<Ticket> {
    if !enabled(view, cx) {
        return None;
    }
    let (scope, owner, revision) = match capture(view, cx) {
        Ok(captured) => captured,
        Err(error) => {
            view.skills.configuration_failed(error.clone());
            view.skills_notice = Some(error);
            return None;
        }
    };
    let request = view.skills.begin_reload(scope, revision.clone())?;
    view.skills_notice = None;
    Some(Ticket {
        request,
        owner,
        revision,
        generation: view.generation,
    })
}
fn finish(
    view: &mut ProjectView,
    ticket: &Ticket,
    result: Result<Snapshot, String>,
    cx: &App,
) -> bool {
    let current = enabled(view, cx)
        && view.generation == ticket.generation
        && view
            .skills_owner
            .as_ref()
            .is_some_and(|owner| owner.same(&ticket.owner))
        && capture(view, cx).is_ok_and(|(scope, _, revision)| {
            view.skills.scope() == Some(&scope) && revision == ticket.revision
        });
    if !current {
        if view.skills.matches_request(&ticket.request) {
            view.skills
                .invalidate("Project scope or published inventory changed during reload");
        }
        return false;
    }
    view.skills.finish_refresh(&ticket.request, result)
}

pub(super) fn reload(view: &mut ProjectView, cx: &mut Context<ProjectView>) {
    let Some(ticket) = begin(view, cx) else {
        cx.notify();
        return;
    };
    cx.spawn(async move |view, cx| {
        let request = ticket.request.clone();
        let snapshot = cx
            .background_executor()
            .spawn(async move { request.discover() })
            .await;
        let _ = view.update(cx, |view, cx| {
            finish(view, &ticket, Ok(snapshot), cx);
            cx.notify();
        });
    })
    .detach();
    cx.notify();
}

#[cfg(test)]
#[path = "agent_skills_tests.rs"]
mod tests;
