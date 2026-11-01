//! Read-only Skills section. The owner supplies the existing Reader open callback.
use gpui::{div, prelude::*, App, IntoElement, Window};
use gpui_component::button::{Button, ButtonVariants};
use okilum_core::agent_skills::{self, Availability, Snapshot};
use std::path::PathBuf;

/// Render the bounded discovery result, retaining coverage diagnostics beside rows.
/// Run discovery off the UI thread; Open revalidates the source before navigation.
/// Callback errors belong in the owner's ordinary visible notice surface.
pub(super) fn render(
    root: PathBuf,
    snapshot: &Snapshot,
    on_open: impl Fn(Result<PathBuf, String>, &mut Window, &mut App) + Clone + 'static,
) -> impl IntoElement {
    let mut section = div().flex().flex_col().gap_2().child("Skills");
    if snapshot.skills.is_empty() {
        section = section.child(if snapshot.diagnostics.is_empty() {
            "No project skills found"
        } else {
            "Skill discovery is incomplete"
        });
    }
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
                                agent_skills::open_source(&root, &skill_for_open),
                                window,
                                cx,
                            );
                        }),
                ),
        );
    }
    section
}
