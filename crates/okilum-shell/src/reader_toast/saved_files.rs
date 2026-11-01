//! An import receipt names the file that was actually saved, including collision
//! suffixes and image conversion. It never guesses a clipboard filename (#1622).
use super::*;

pub(super) fn show(root: &Path, paths: &[PathBuf], window: &mut Window, cx: &mut App) {
    let relatives = paths
        .iter()
        .filter_map(|path| path.strip_prefix(root).ok())
        .filter(|path| {
            !path.as_os_str().is_empty()
                && path
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
                && !okilum_core::vault::service_path(path)
        })
        .map(okilum_core::vault::note_path)
        .collect::<Vec<_>>();
    // Image import puts the converted/linked image after an optional original.
    let Some(relative) = relatives.last().cloned() else {
        return;
    };
    let message = if relatives.len() == 1 {
        format!("Saved {relative}")
    } else {
        format!("Saved {} files · {relative}", relatives.len())
    };
    let root = root.to_path_buf();
    let path_label = relative.clone();
    let reveal_label = match platform::labels::Os::CURRENT {
        platform::labels::Os::Mac => "Reveal in Finder",
        platform::labels::Os::Windows => "Reveal in Explorer",
        platform::labels::Os::Linux => "Show in folder",
    };
    push(
        Notification::new()
            .content(move |_, _, _| {
                let path = path_label.clone();
                div()
                    .debug_selector(move || format!("paste-saved-path-{path}"))
                    .text_sm()
                    .whitespace_normal()
                    .child(message.clone())
                    .into_any_element()
            })
            .action(move |_, _, _| {
                let root = root.clone();
                let relative = relative.clone();
                Button::new("reveal-saved-attachment")
                    .debug_selector(|| "reveal-saved-attachment".into())
                    .small()
                    .icon(IconName::FolderOpen)
                    .label(reveal_label)
                    .on_click(move |_, window, cx| {
                        // Re-check current containment/existence when the user
                        // acts; a stale receipt never authorizes a new target.
                        reader_files::run(
                            reader_files::FileAction::Reveal,
                            &root,
                            &relative,
                            window,
                            cx,
                        )
                    })
            }),
        Some(Duration::from_secs(8)),
        window,
        cx,
    );
}
