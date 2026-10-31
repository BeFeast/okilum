//! A project's picture (#1627): one avatar wherever a project is named — the
//! Projects list, the title-bar switcher, the sidebar header in a project and
//! the Manager panel's header. The icon is the one T3 Code shows: a project
//! with a T3 counterpart takes T3's `projectIcon` and `faviconPath`; any
//! other project takes `icon:` from its project.yaml. Without either, a logo
//! in the project's folder, then an icon for its project kind.
use gpui::{
    div, img, prelude::*, px, rgb, AnyElement, App, FontWeight, Global, ObjectFit, SharedString,
};
use gpui_component::{ActiveTheme as _, Icon};
use okilum_t3::projects::{
    icon::{self, Avatar, Counterpart, ProjectIcon},
    Kind, Project,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

/// The T3 counterparts seen in the last listing, by vault project folder.
/// Projects absent here use their project.yaml.
#[derive(Default)]
pub struct ProjectIcons {
    pub links: BTreeMap<PathBuf, Counterpart>,
}
impl Global for ProjectIcons {}

/// The T3 counterpart of a project, if one was seen.
pub fn link<'a>(project: &Project, cx: &'a App) -> Option<&'a Counterpart> {
    cx.try_global::<ProjectIcons>()?.links.get(&project.folder)
}

/// The manual icon that applies: T3's for a linked project, project.yaml's
/// otherwise.
pub fn manual_icon(project: &Project, cx: &App) -> Option<ProjectIcon> {
    match link(project, cx) {
        Some(link) => link.icon.clone(),
        None => project.appearance.icon.clone(),
    }
}

/// Manual icon > logo (T3's favicon, then one in the folder) > project kind.
pub fn avatar(project: &Project, cx: &App) -> Avatar {
    let link = link(project, cx);
    let manual = match link {
        Some(link) => link.icon.as_ref(),
        None => project.appearance.icon.as_ref(),
    };
    let logo = link
        .and_then(|link| link.logo.as_deref())
        .or(project.appearance.logo.as_deref());
    match icon::resolve(manual, logo, &project.name) {
        // A deliberately chosen monogram remains a manual icon. Only the
        // automatic name-derived fallback is replaced by the project's kind.
        Avatar::Monogram { .. } => Avatar::Icon(ProjectIcon::Lucide {
            name: match project.kind {
                Kind::Code => "code-xml",
                Kind::Knowledge => "book-open",
                Kind::Hybrid => "layers",
            }
            .into(),
            color: icon::IconColor::Gray,
        }),
        avatar => avatar,
    }
}

struct LucideNames(Arc<BTreeSet<String>>);
impl Global for LucideNames {}

fn bundled() -> BTreeSet<String> {
    crate::lucide::names().iter().cloned().collect()
}

/// Reads the icon set once per app; the Projects picker and the icon
/// dialog call it as they open.
pub fn init(cx: &mut App) {
    if !cx.has_global::<LucideNames>() {
        cx.set_global(LucideNames(Arc::new(bundled())));
    }
}

/// Replaces the known icon set: tests, whose app carries no assets.
#[doc(hidden)]
pub fn set_lucide_names(names: impl IntoIterator<Item = String>, cx: &mut App) {
    cx.set_global(LucideNames(Arc::new(names.into_iter().collect())));
}

/// Every Lucide icon T3 Code offers (the whole bundled set, aliases
/// included), by Lucide name.
pub fn lucide_names(cx: &App) -> Arc<BTreeSet<String>> {
    match cx.try_global::<LucideNames>() {
        Some(names) => names.0.clone(),
        None => Arc::new(bundled()),
    }
}

/// A Lucide icon by name, or the folder for a name Lucide does not have.
pub fn lucide(name: &str, cx: &App) -> Icon {
    let known = match cx.try_global::<LucideNames>() {
        Some(names) => names.0.contains(name),
        None => crate::lucide::contains(name),
    };
    let name = if known { name } else { icon::DEFAULT_LUCIDE };
    Icon::default().path(SharedString::from(crate::lucide::asset_path(name)))
}

/// One avatar, `size` px square.
pub fn render(avatar: &Avatar, size: f32, cx: &App) -> AnyElement {
    let dark = cx.theme().is_dark();
    let frame = || {
        div()
            .size(px(size))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
    };
    match avatar {
        Avatar::Icon(ProjectIcon::Lucide { name, color }) => frame()
            .child(
                lucide(name, cx)
                    .size(px(size))
                    .text_color(rgb(color.glyph(dark))),
            )
            .into_any_element(),
        Avatar::Icon(ProjectIcon::Emoji { emoji }) => frame()
            .text_size(px(size * 0.85))
            .line_height(px(size))
            .child(emoji.clone())
            .into_any_element(),
        Avatar::Icon(ProjectIcon::Monogram { text, color }) | Avatar::Monogram { text, color } => {
            let letters = text.chars().count().max(1) as f32;
            frame()
                .rounded(px((size * 0.25).max(3.)))
                .bg(rgb(color.tile()))
                .text_color(gpui::white())
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(size * if letters > 1. { 0.5 } else { 0.62 }))
                .line_height(px(size))
                .child(text.clone())
                .into_any_element()
        }
        Avatar::Logo(path) => frame()
            .rounded(px((size * 0.2).max(2.)))
            .child(
                img(path.clone())
                    .size(px(size))
                    .object_fit(ObjectFit::Contain),
            )
            .into_any_element(),
    }
}

/// The project's avatar, `size` px square.
pub fn project_avatar(project: &Project, size: f32, cx: &App) -> AnyElement {
    render(&avatar(project, cx), size, cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn project_kind_fallback_keeps_local_and_explicit_t3_icons(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            for (kind, expected) in [
                (Kind::Code, "code-xml"),
                (Kind::Knowledge, "book-open"),
                (Kind::Hybrid, "layers"),
            ] {
                let mut project = Project {
                    folder: PathBuf::from("/vault/Dev/Areas/fixture"),
                    name: "Same name".into(),
                    kind,
                    environment: None,
                    manager_project: None,
                    manager: okilum_t3::projects::Manager::NotStarted,
                    appearance: Default::default(),
                };
                assert_eq!(
                    avatar(&project, cx),
                    Avatar::Icon(ProjectIcon::Lucide {
                        name: expected.into(),
                        color: icon::IconColor::Gray
                    })
                );
                project.appearance.logo = Some(project.folder.join(".okilum/icon.png"));
                assert_eq!(
                    avatar(&project, cx),
                    Avatar::Logo(project.appearance.logo.clone().unwrap())
                );
                let chosen = ProjectIcon::Monogram {
                    text: "AB".into(),
                    color: icon::IconColor::Blue,
                };
                project.appearance.icon = Some(chosen.clone());
                assert_eq!(
                    avatar(&project, cx),
                    Avatar::Icon(chosen),
                    "an explicit monogram is not replaced"
                );
                let t3 = ProjectIcon::Emoji {
                    emoji: "🚀".into()
                };
                cx.set_global(ProjectIcons {
                    links: [(
                        project.folder.clone(),
                        Counterpart {
                            environment: "fixture".into(),
                            project_id: "fixture".into(),
                            icon: Some(t3.clone()),
                            logo: None,
                        },
                    )]
                    .into_iter()
                    .collect(),
                });
                assert_eq!(avatar(&project, cx), Avatar::Icon(t3));
                cx.set_global(ProjectIcons::default());
            }
        });
    }
}
