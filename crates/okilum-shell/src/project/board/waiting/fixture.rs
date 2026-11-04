//! Synthetic inputs shared by native regression tests and the screenshot harness.
use super::*;
use okilum_t3::board::{context, delivery as d};

pub(in crate::project::board) fn populate(board: &mut Board) {
    let at = now();
    std::fs::create_dir_all(board.area.join("ops")).unwrap();
    std::fs::write(
        board.area.join("ops/board-context.toml"),
        "version=1\nquestions_note='ops/questions.md'\nnotes=['try.md']\n",
    )
    .unwrap();
    std::fs::write(board.area.join("ops/questions.md"), "# Synthetic questions\n\n## Open\n- [Sync owner] Share the connection tutorial? — share — tester note\n").unwrap();
    let source = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let text = format!("---\nboard_context:\n  kind: try\n  title: Check editing and autosave\n  reason: Exercise this exact delivered build\n  consequence: Report the observed behavior\n  issue: 42\n  platform: linux\n  channel: beta\n  video: https://video.example.test/editing.mp4\ndelivery:\n  - issue: 42\n    pr: 7\n    platform: linux\n    channel: beta\n    build: '7552'\n    source: {source}\n    artifact: https://updates.example.test/7552.AppImage\n    scenario: Edit a note and reopen it\n---\nSynthetic exact-build scenario.\n");
    std::fs::write(board.area.join("try.md"), &text).unwrap();
    board.binding = Some(Binding::parse_toml("version=1\ntitle='Synthetic Board'\nrepository='https://git.example.test/team/project'\n[[issues]]\nnumber=42\nexpected='Editing and autosave'\nexecutor='Editor'\npull_requests=[7]\n").unwrap());
    let created = chrono::DateTime::from_timestamp((at - 2400) as i64, 0)
        .unwrap()
        .to_rfc3339();
    let issues = vec![
        serde_json::from_value(serde_json::json!({"number":42,"title":"Editing and autosave","state":"open"})).unwrap(),
        serde_json::from_value(serde_json::json!({"number":43,"title":"Pasted text lost its formatting","state":"open","labels":[{"name":"from-tester"}],"created_at":created,"comments":0,"user":{"id":4,"login":"filing-bot"}})).unwrap(),
    ];
    let ticket = board.snapshot.begin().unwrap();
    board.snapshot.live_issues(&ticket, Ok(issues), at);
    board.snapshot.threads(&ticket, Ok(Vec::new()), at);
    // Keep the PR identity in the same live projection used in production.
    board
        .snapshot
        .forgejo
        .value
        .get_mut(&42)
        .unwrap()
        .live_pulls
        .push(okilum_t3::board::live::Pull {
            number: 7,
            merged_at: None,
            checks: "success".into(),
            ..Default::default()
        });
    board
        .project_notes
        .accept(context::refresh(&board.area), at);
    let evidence = context::parse_note("try.md", &text).unwrap().evidence;
    let build = d::Build {
        platform: d::Platform::Linux,
        channel: d::Channel::Beta,
        build: "7552".into(),
        source: source.into(),
        artifact: "https://updates.example.test/7552.AppImage".into(),
        published_at: Some(
            chrono::DateTime::from_timestamp((at - 18000) as i64, 0)
                .unwrap()
                .to_rfc3339(),
        ),
        release_url: None,
    };
    board.delivery.accept(
        d::Update {
            config: Ok(Some(d::Config {
                version: 1,
                feeds: vec![d::Feed {
                    platform: d::Platform::Linux,
                    channel: d::Channel::Beta,
                    url: "https://updates.example.test/feed.json".into(),
                    format: d::Format::Receipt,
                    metadata: None,
                    artifact_name: None,
                    next: String::new(),
                    blocker: None,
                }],
                evidence: vec!["try.md".into()],
            })),
            feeds: vec![(
                (d::Platform::Linux, d::Channel::Beta),
                Ok(Some(build.clone())),
            )],
            artifacts: vec![(build.artifact.clone(), Ok(d::Artifact { available: true }))],
            ci: vec![(
                7,
                Ok(d::Ci {
                    pr: 7,
                    source: source.into(),
                    state: "success".into(),
                    links: vec![],
                    inclusion: std::collections::BTreeMap::from([(source.into(), Ok(true))]),
                }),
            )],
            evidence: vec![("try.md".into(), Ok(evidence))],
        },
        at,
    );
    board.filter_cache.invalidate();
    board.refresh_waiting();
}

#[cfg(feature = "project-ui-harness")]
pub(in crate::project) fn open(cx: &mut App) -> bool {
    let Ok(mode) = std::env::var("OKILUM_PROJECT_UI_FIXTURE") else {
        return false;
    };
    if !matches!(
        mode.as_str(),
        "board-waiting-dark" | "board-waiting-light" | "list-waiting-dark" | "list-waiting-light"
    ) {
        return false;
    }
    gpui_component::Theme::change(
        if mode.ends_with("light") {
            gpui_component::ThemeMode::Light
        } else {
            gpui_component::ThemeMode::Dark
        },
        None,
        cx,
    );
    brand::apply_theme(cx);
    let area = reader_history::state_directory()
        .expect("isolated fixture state")
        .join("board-waiting-fixture");
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1500.), px(700.)),
                cx,
            ))),
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title("Okilum — Synthetic waiting Board");
            let board = cx.new(|cx| {
                let mut board = Board::with_reader(area, |_| panic!("offline fixture"), window, cx);
                populate(&mut board);
                board.list.preferences.mode = if mode.starts_with("list-") {
                    list::Mode::List
                } else {
                    list::Mode::Board
                };
                board
            });
            cx.new(|cx| Root::new(board, window, cx))
        },
    )
    .expect("native waiting fixture window");
    true
}
