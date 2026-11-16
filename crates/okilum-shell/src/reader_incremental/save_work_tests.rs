//! The real incremental launch must not copy the whole Projects snapshot on UI.
use super::*;
use crate::reader_editor::work;
use ::core::prelude::v1::test;

#[gpui::test]
fn edit2803_incremental_launch_moves_project_copy_off_foreground(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    let project = root.join("Project");
    std::fs::create_dir_all(&project).unwrap();
    let title = format!("{}tail", "שלום🙂 project metadata ".repeat(1024));
    std::fs::write(
        project.join("@index.md"),
        format!("---\ntype: Project\nstatus: active\ntitle: {title}\n---\n# Project\n"),
    )
    .unwrap();
    const NOTES: usize = 128;
    for n in 0..NOTES {
        std::fs::write(
            project.join(format!("note{n:03}.md")),
            "- [ ] pendingword\n",
        )
        .unwrap();
    }
    // Project-shaped vaults also contain attachments; those must stay in the
    // inventory without becoming project/task source rows.
    for n in 0..256 {
        std::fs::write(project.join(format!("source{n:03}.rs")), "fn main() {}\n").unwrap();
    }
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Project/note000.md".into()),
                    index_dir: Some(temp.path().join("cache")),
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
    let reader = entity.unwrap();
    visual.run_until_parked();
    let published = reader.read_with(visual, |r, _| {
        assert!(r.incremental_state.is_some());
        assert_eq!(r.projects.rows().len(), 1);
        assert_eq!(r.projects.rows()[0].title, title);
        assert_eq!(r.projects.rows()[0].open_tasks, NOTES);
        r.projects.clone()
    });
    // Fired positive: measure the exact deep clone removed from this launch,
    // not a guessed byte/time threshold or a test-only annotated code path.
    work::reset();
    {
        let _scope = work::scope();
        drop((*published).clone());
    }
    let positive = work::take();
    assert!(
        positive.bytes >= title.len() * 2,
        "both cached title owners must be copied: {positive:?}"
    );
    assert!(
        positive.frees > 0,
        "the explicit clone must actually be destroyed"
    );
    std::fs::write(project.join("note000.md"), "- [x] completedword\n").unwrap();
    let (send, hold) = async_channel::bounded(1);
    let foreground = reader.update_in(visual, |r, window, cx| {
        r.incremental_hold = Some(hold);
        work::reset();
        {
            let _scope = work::scope();
            r.start_incremental(
                okilum_core::Changes {
                    changed: std::collections::BTreeSet::from(["Project/note000.md".into()]),
                    ..Default::default()
                },
                window,
                cx,
            );
        }
        let measured = work::take();
        assert!(
            r.incremental_active && r.incremental_state.is_none(),
            "actual worker path must fire"
        );
        measured
    });
    eprintln!(
        "edit2803 project clone positive={positive:?}; actual foreground launch={foreground:?}"
    );
    assert!(foreground.bytes < positive.bytes,
        "incremental launch copied the whole Projects snapshot on foreground: launch={foreground:?}, actual clone={positive:?}");
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert!(r.incremental_active, "worker gate must still be held");
        assert!(Arc::ptr_eq(&r.projects, &published));
    });
    send.try_send(()).unwrap();
    visual.run_until_parked();
    reader.read_with(visual, |r, _| {
        assert!(!r.incremental_active);
        assert!(r.incremental_state.is_some());
        assert!(!Arc::ptr_eq(&r.projects, &published));
        assert_eq!(
            published.rows()[0].open_tasks,
            NOTES,
            "published snapshot stays immutable"
        );
        assert_eq!(r.projects.rows()[0].open_tasks, NOTES - 1);
        assert_eq!(r.projects.rows()[0].title, title);
        assert_eq!(
            r.searcher
                .as_ref()
                .unwrap()
                .search("completedword", 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            std::fs::read_to_string(project.join("note000.md")).unwrap(),
            "- [x] completedword\n"
        );
    });
}
