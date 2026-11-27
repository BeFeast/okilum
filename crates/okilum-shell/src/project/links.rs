use super::*;
pub(crate) use okilum_shell_project::project::links::*;

impl Reader {
    pub(super) fn connect_manager_links(
        &mut self,
        manager: &Entity<ProjectView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(
            manager,
            window,
            |this, manager, request: &OpenFile, window, cx| {
                if this.project.as_ref() != Some(manager)
                    || this.project_navigation.active.as_ref().map(|p| &p.folder)
                        != Some(&request.area)
                {
                    return;
                }
                let target = this.vault_root.canonicalize().ok().and_then(|vault| {
                    request
                        .path
                        .strip_prefix(vault)
                        .ok()
                        .map(okilum_core::vault::note_path)
                });
                let Some(relative) = target else {
                    this.link_notice = Some("This chat file is outside the current vault.".into());
                    cx.notify();
                    return;
                };
                let line = request
                    .fragment
                    .as_deref()
                    .and_then(|s| s.strip_prefix('L'))
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|n| *n > 0);
                this.open_note_landing(
                    &relative,
                    None,
                    if line.is_some() {
                        None
                    } else {
                        request.fragment.as_deref()
                    },
                    line.map(prepared_links::Landing::Line),
                    window,
                    cx,
                );
            },
        )
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn chat_file_links_open_the_confirmed_project_note_in_the_real_reader(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        let area = vault.join("Project");
        std::fs::create_dir_all(&area).unwrap();
        std::fs::write(vault.join("start.md"), "# Outside the project").unwrap();
        std::fs::write(
            area.join("Target.md"),
            "# Target\n\n## Destination\nFrom chat.",
        )
        .unwrap();
        let project = okilum_t3::projects::Project {
            folder: area.clone(),
            name: "Project".into(),
            kind: okilum_t3::projects::Kind::Code,
            environment: None,
            manager_project: None,
            manager: okilum_t3::projects::Manager::NotStarted,
            appearance: Default::default(),
        };
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        open_path: Some(vault.join("start.md")),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        visual.run_until_parked();
        let reader = entity.unwrap();
        let manager = reader.update_in(visual, |reader, window, cx| {
            let manager = cx.new(|cx| {
                let mut view = ProjectView::empty(Some(temp.path().join("state")), window, cx);
                view.area = Some(area.clone());
                view.active = true;
                view
            });
            reader.project_navigation.active = Some(project);
            reader.project = Some(manager.clone());
            reader.connect_manager_links(&manager, window, cx);
            manager
        });
        manager.update_in(visual, |manager, window, cx| {
            manager.open_chat_link(
                super::super::feed::content::resolve_link("Target.md#Destination", Some(&area)),
                window,
                cx,
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(
                reader.pane.current_rel, "Project/Target.md",
                "file action must use the confirmed area, not start.md's directory"
            );
            assert!(reader.pane.note_source.contains("From chat."));
        });
        manager.update_in(visual, |manager, window, cx| {
            manager.open_chat_link(
                super::super::feed::content::resolve_link("Missing.md", Some(&area)),
                window,
                cx,
            );
        });
        visual.run_until_parked();
        manager.read_with(visual, |manager, _| {
            assert!(manager
                .threads
                .link_notice
                .as_deref()
                .unwrap()
                .contains("missing"))
        });
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.pane.current_rel, "Project/Target.md")
        });
    }

    #[test]
    fn file_validation_requires_a_real_file_under_the_confirmed_project() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Note.md"), "# Note").unwrap();
        std::fs::write(other.path().join("Other.md"), "# Other").unwrap();
        assert_eq!(
            local_file(root.path(), &root.path().join("Note.md")).unwrap(),
            root.path().join("Note.md").canonicalize().unwrap()
        );
        assert!(local_file(root.path(), root.path()).is_err());
        assert!(local_file(root.path(), &root.path().join("missing.md")).is_err());
        assert!(local_file(root.path(), &other.path().join("Other.md")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(other.path().join("Other.md"), root.path().join("Alias.md"))
                .unwrap();
            assert!(local_file(root.path(), &root.path().join("Alias.md")).is_err());
        }
    }
}
