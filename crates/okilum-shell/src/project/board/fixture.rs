//! Isolated native screenshot fixture; never reads real project files or sources.
use super::*;

pub(in super::super) fn open(cx: &mut App) -> bool {
    let Ok(mode) = std::env::var("OKILUM_BOARD_UI_FIXTURE") else {
        return false;
    };
    Theme::change(
        if mode == "light" {
            gpui_component::ThemeMode::Light
        } else {
            gpui_component::ThemeMode::Dark
        },
        None,
        cx,
    );
    brand::apply_theme(cx);
    let state = reader_history::state_directory().expect("isolated fixture state");
    cx.open_window(WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1400.), px(850.)), cx))),
        ..Default::default()
    }, move |window, cx| {
        window.set_window_title("Okilum — Board filters synthetic fixture");
        let board = cx.new(|cx| {
            let mut board = Board::with_reader(state.join("synthetic-project"), |_| panic!("fixture never reads sources"), window, cx);
            board.binding = Some(Binding::parse(r#"
version: 1
title: Synthetic Board
repository: https://example.invalid/synthetic/board
issues:
  - {number: 123, expected: 'Поиск по задачам на русском', executor: Editing}
  - {number: 456, expected: 'Tester report: keep the Board responsive', executor: Tests}
  - {number: 789, expected: 'A compact shared filter bar', executor: Editing}
  - {number: 790, expected: 'Remember the search when returning from a note', executor: Tests}
"#).unwrap());
            board.snapshot.forgejo.value = [(123, "Поиск по задачам на русском", vec!["from-oleg", "bug", "epic: board"]),
                (456, "Tester report: keep the Board responsive", vec!["from-tester", "bug"]),
                (789, "A compact shared filter bar", vec!["from-oleg", "fr"]),
                (790, "Remember the search when returning from a note", vec!["task"])]
                .into_iter().map(|(number, title, labels)| {
                    (number, serde_json::from_value(serde_json::json!({"number":number,"title":title,"state":"open",
                        "labels":labels.into_iter().map(|name| serde_json::json!({"name":name})).collect::<Vec<_>>() })).unwrap())
                }).collect();
            board.binding.as_mut().unwrap().issues[1].current_task = true;
            board.binding.as_mut().unwrap().issues[1].agent_thread_id = Some("synthetic-agent".into());
            board.snapshot.t3.value = vec![serde_json::from_value(serde_json::json!({
                "threadId":"synthetic-agent","title":"Synthetic test agent","status":"running","activeRunId":"fixture-run",
                "providerInstanceId":"codex","model":"model","runtimeMode":"full-access",
                "updatedAt":chrono::Utc::now().to_rfc3339()
            })).unwrap()];
            board.list.preferences.mode = list::Mode::List;
            board.list.preferences.collapsed[Column::Queue as usize] = false;
            board.filter.chip = filters::Chip::Oleg;
            board
        });
        cx.new(|cx| Root::new(board, window, cx))
    }).expect("native Board filter fixture window");
    cx.activate(true);
    true
}
