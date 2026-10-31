//! Shared Markdown configuration and project-scoped chat link intents.
//!
//! `confirmed_root` is a uniquely verified local `ProjectView.area`, never the
//! currently displayed Reader note's directory. The file callback must validate
//! existence/canonical containment before opening and display unavailable-link
//! reasons. Resolving an intent here performs no filesystem or network access.

use crate::{reader_code, reader_manager_links, reader_text_style};
use gpui::{App, ClickEvent, ElementId, MouseButton, SharedString, Window};
use gpui_component::{
    text::{SelectionFormat, TextView},
    ActiveTheme as _,
};
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkTarget {
    External(url::Url),
    File {
        path: PathBuf,
        /// Authored document heading/block/line intent, decoded once. The
        /// receiver must honor it or visibly report unsupported navigation.
        fragment: Option<String>,
    },
    Unavailable {
        target: String,
        reason: &'static str,
    },
}

/// Receives file intents and unavailable-link notices. HTTP(S) is handled by
/// `markdown` itself, so no parent hook needs to launch external URLs.
pub(crate) type LinkHandler = Arc<dyn Fn(LinkTarget, &mut Window, &mut App) + Send + Sync>;

/// The caller supplies a stable element ID for each message/activity body and
/// the current confirmed root. No Feed state or Reader-note context is needed.
pub(super) fn markdown(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    confirmed_root: Option<PathBuf>,
    on_link: LinkHandler,
    cx: &App,
) -> TextView {
    TextView::markdown(id, text)
        .selectable(true)
        .selection_format(SelectionFormat::Plain)
        .style(reader_text_style(cx.theme()))
        .code_block_actions(reader_code::actions)
        .on_link_click(move |url, event, window, cx| {
            if !activates_link(event) {
                return;
            }
            cx.stop_propagation();
            match resolve_link(url, confirmed_root.as_deref()) {
                LinkTarget::External(url) => cx.open_url(url.as_str()),
                target => on_link(target, window, cx),
            }
        })
}

fn activates_link(event: &ClickEvent) -> bool {
    match event {
        ClickEvent::Mouse(click) => {
            matches!(click.up.button, MouseButton::Left | MouseButton::Middle)
        }
        ClickEvent::Keyboard(_) => true,
        ClickEvent::Touch(click) => !click.long_press,
    }
}

/// Resolves lexical targets only. In particular, missing/ambiguous project
/// roots are explicit failures, never an invitation to use the Reader note.
/// Absolute file paths and file: URLs must remain inside the confirmed root.
pub(crate) fn resolve_link(raw: &str, confirmed_root: Option<&Path>) -> LinkTarget {
    let unavailable = |reason| LinkTarget::Unavailable {
        target: raw.to_owned(),
        reason,
    };
    if let Some(url) = reader_manager_links::external_url(raw) {
        return LinkTarget::External(url);
    }
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return unavailable("This chat link has no valid destination.");
    }
    if raw.starts_with("//") {
        return unavailable("This chat link does not specify a supported destination scheme.");
    }

    let (path, fragment) = match url::Url::parse(raw) {
        Ok(url) if url.scheme() == "file" => {
            if url.query().is_some() {
                return unavailable("Query parameters are not supported for chat file links.");
            }
            let Ok(path) = url.to_file_path() else {
                return unavailable("This chat file URL does not identify a local file.");
            };
            let fragment = match url.fragment().map(decode).transpose() {
                Ok(fragment) => fragment,
                Err(()) => return unavailable("This chat link has invalid text encoding."),
            };
            (path, fragment)
        }
        Ok(_) => return unavailable("This chat link uses an unsupported destination scheme."),
        Err(_) => {
            let (path, fragment) = raw
                .split_once('#')
                .map_or((raw, None), |(path, fragment)| (path, Some(fragment)));
            if path.is_empty() {
                return unavailable(
                    "A chat message has no source document for this fragment link.",
                );
            }
            if path.contains('?') {
                return unavailable("Query parameters are not supported for chat file links.");
            }
            let path = match decode(path) {
                Ok(path) => PathBuf::from(path),
                Err(()) => return unavailable("This chat link has invalid text encoding."),
            };
            let fragment = match fragment.map(decode).transpose() {
                Ok(fragment) => fragment,
                Err(()) => return unavailable("This chat link has invalid text encoding."),
            };
            (path, fragment)
        }
    };
    if path.as_os_str().is_empty() {
        return unavailable("This chat link has no file destination.");
    }
    let Some(root) = confirmed_root else {
        return unavailable("A unique local project root is not confirmed for this thread.");
    };
    let Some(root) = normalized_absolute(root) else {
        return unavailable("The confirmed local project root is not an absolute path.");
    };
    let target = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let Some(path) = normalized_absolute(&target) else {
        return unavailable("This chat file link escapes the confirmed project root.");
    };
    if !path.starts_with(&root) {
        return unavailable("This chat file link is outside the confirmed project root.");
    }
    LinkTarget::File { path, fragment }
}

fn decode(raw: &str) -> Result<String, ()> {
    let text = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .map_err(|_| ())?;
    if text.chars().any(char::is_control) {
        return Err(());
    }
    Ok(text.into_owned())
}

fn normalized_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn external_links_do_not_depend_on_a_local_project_root() {
        let raw = "https://example.org/issues/42#discussion";
        assert_eq!(
            resolve_link(raw, None),
            LinkTarget::External(url::Url::parse(raw).unwrap())
        );
        assert!(matches!(
            resolve_link("http://example.org", Some(Path::new("/project"))),
            LinkTarget::External(_)
        ));
    }

    #[test]
    fn file_targets_use_only_the_confirmed_project_root_and_keep_fragments() {
        let root = Path::new("/projects/okilum");
        for raw in [
            "docs/../notes/hello%20world.md#My%20heading",
            "/projects/okilum/notes/hello%20world.md#My%20heading",
            "file:///projects/okilum/notes/hello%20world.md#My%20heading",
        ] {
            assert_eq!(
                resolve_link(raw, Some(root)),
                LinkTarget::File {
                    path: root.join("notes/hello world.md"),
                    fragment: Some("My heading".into()),
                },
                "{raw}"
            );
        }
        assert_eq!(
            resolve_link("notes/hash%23name.md", Some(root)),
            LinkTarget::File {
                path: root.join("notes/hash#name.md"),
                fragment: None,
            }
        );
    }

    #[test]
    fn missing_or_ambiguous_local_root_stays_explicit() {
        for raw in ["notes/file.md", "/projects/okilum/file.md"] {
            let LinkTarget::Unavailable { reason, .. } = resolve_link(raw, None) else {
                panic!("unconfirmed root unexpectedly resolved {raw}")
            };
            assert!(reason.contains("unique local project root"));
        }
        assert!(matches!(
            resolve_link("file.md", Some(Path::new("relative/project"))),
            LinkTarget::Unavailable { .. }
        ));
    }

    #[test]
    fn escaping_paths_fragments_and_other_schemes_are_explicit() {
        let root = Some(Path::new("/projects/okilum"));
        for raw in [
            "../outside.md",
            "%2E%2E/outside.md",
            "/projects/okilum-neighbor/file.md",
            "file:///etc/passwd",
            "#Heading",
            "javascript:alert(1)",
            "vscode://file/projects/okilum/main.rs",
            "//example.org/path",
            "file.md?revision=previous",
            "file%00name.md",
            "file%FFname.md",
        ] {
            assert!(
                matches!(resolve_link(raw, root), LinkTarget::Unavailable { .. }),
                "unexpectedly resolved {raw}"
            );
        }
        // Positive control: a missing file remains an intent for the receiver
        // to validate; the lexical resolver never silently searches elsewhere.
        assert!(matches!(
            resolve_link("missing.md", root),
            LinkTarget::File { .. }
        ));
    }
}

impl super::Feed {
    pub(super) fn markdown(
        &self,
        id: impl Into<ElementId>,
        text: impl Into<SharedString>,
        cx: &App,
    ) -> TextView {
        let root = self
            .parent
            .upgrade()
            .and_then(|parent| parent.read(cx).area.clone());
        markdown(
            id,
            text,
            root,
            crate::project::ProjectView::chat_link_handler(self.parent.clone()),
            cx,
        )
    }
}
