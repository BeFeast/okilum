//! Rebinding derived views must not require rewriting equal canonical sources.
use super::*;
use ::core::prelude::v1::test;
use gpui::{TestAppContext, VisualTestContext};

#[derive(Clone, Copy, Debug)]
enum Consumer {
    Tasks,
    Projects,
    Now,
}

fn mounted<'a>(
    cx: &'a mut TestAppContext,
    temp: &tempfile::TempDir,
) -> (Entity<Reader>, &'a mut VisualTestContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let root = temp.path().join("vault");
    std::fs::create_dir_all(root.join("Demo")).unwrap();
    std::fs::write(
        root.join("Work.md"),
        "# Work\n\n- [ ] First task\n- [ ] Second task\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Demo/_index.md"),
        "---\ntype: Project\nstatus: active\n---\n# Demo\n",
    )
    .unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Work.md".into()),
                    index_dir: Some(temp.path().join("cache")),
                    session_directory: Some(temp.path().join("state")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(960.)));
    (reader.unwrap(), visual)
}

fn present(reader: &Reader, consumer: Consumer) -> bool {
    match consumer {
        Consumer::Tasks => reader
            .tasks_index
            .as_ref()
            .is_some_and(|index| index.note_tasks("Work.md").len() == 2),
        Consumer::Projects => reader
            .projects
            .rows()
            .iter()
            .any(|p| p.path == "Demo/_index.md"),
        Consumer::Now => reader.workbench.rows().any(|row| row.path == "Work.md"),
    }
}

async fn exercise(cx: &mut TestAppContext, consumer: Consumer) {
    let temp = tempfile::tempdir().unwrap();
    let (reader, visual) = mounted(cx, &temp);
    visual
        .condition(&reader, |reader, _| {
            reader.incremental_state.is_some()
                && !reader.incremental_initializing
                && !reader.incremental_active
                && [Consumer::Tasks, Consumer::Projects, Consumer::Now]
                    .into_iter()
                    .all(|consumer| present(reader, consumer))
        })
        .await;
    let path = match consumer {
        Consumer::Projects => "Demo/_index.md",
        _ => "Work.md",
    };
    let source = std::fs::read(temp.path().join("vault").join(path)).unwrap();
    let generation = reader.update_in(visual, |reader, window, cx| {
        reader.watcher.take();
        let generation = reader
            .incremental_state
            .as_ref()
            .unwrap()
            .snapshot
            .search_generation
            .clone();
        assert!(
            generation.is_some(),
            "positive control: a committed search baseline exists"
        );
        match consumer {
            Consumer::Tasks => reader.tasks_index = Some(Arc::default()),
            Consumer::Projects => reader.projects = Arc::default(),
            Consumer::Now => {
                reader.workbench = Arc::default();
                reader.workbench_ready = None;
            }
        }
        assert!(
            !present(reader, consumer),
            "positive control: derived view was retired"
        );
        reader.start_incremental(
            okilum_core::Changes {
                changed: [path.to_owned()].into_iter().collect(),
                ..Default::default()
            },
            window,
            cx,
        );
        cx.notify();
        generation
    });
    visual
        .condition(&reader, move |reader, _| {
            !reader.incremental_active
                && reader.incremental_state.is_some()
                && present(reader, consumer)
        })
        .await;
    reader.read_with(visual, |reader, _| {
        assert_eq!(
            reader
                .incremental_state
                .as_ref()
                .unwrap()
                .snapshot
                .search_generation,
            generation,
            "rehydrating a derived view must not rewrite the search generation"
        );
    });
    assert_eq!(
        std::fs::read(temp.path().join("vault").join(path)).unwrap(),
        source
    );
    visual.update(|window, _| window.remove_window());
    visual.run_until_parked();
}

#[gpui::test]
async fn edit2803_identical_hint_restores_retired_tasks_without_search_rewrite(
    cx: &mut TestAppContext,
) {
    exercise(cx, Consumer::Tasks).await;
}

#[gpui::test]
async fn edit2803_identical_hint_restores_retired_projects_without_search_rewrite(
    cx: &mut TestAppContext,
) {
    exercise(cx, Consumer::Projects).await;
}

#[gpui::test]
async fn edit2803_identical_hint_restores_retired_now_without_search_rewrite(
    cx: &mut TestAppContext,
) {
    exercise(cx, Consumer::Now).await;
}
