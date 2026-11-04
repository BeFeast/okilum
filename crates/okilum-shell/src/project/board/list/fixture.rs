//! Explicitly selected synthetic native window; never connects to a real source.
use super::*;
impl Board {
    pub(crate) fn open_list_fixture(state: PathBuf, cx: &mut App) {
        let area = state.join("synthetic-board-project");
        std::fs::create_dir_all(&area).expect("synthetic project");
        cx.open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1400.), px(900.)), cx))),
            ..Default::default()
        }, move |window, cx| {
            window.set_window_title("Okilum — Synthetic Board List · 240 issues");
            let board = cx.new(|cx| {
                let mut board = Self::with_reader(area, |_| Ok(Vec::new()), window, cx);
                let titles = ["Manager stays responsive while scrolling the task list", "Keep the selected issue when returning from a note", "Show the current build in Settings", "Find highlights the correct paragraph", "Remember the project view on restart", "Sync keeps both computers up to date", "Improve keyboard navigation through long lists"];
                let at = now();
                let issues = (1..=240).map(|n| serde_json::json!({"number":n,"expected":titles[(n as usize - 1) % titles.len()],"executor":if n % 3 == 0 { "Tests" } else { "Editing" },"pull_requests":if n <= 8 { vec![1000 + n] } else { vec![] },"stage":if (9..=11).contains(&n) { Some("ready") } else { None },"agent_thread_id":if n <= 7 { Some("fixture-agent") } else { None }})).collect::<Vec<_>>();
                board.binding = Some(Binding::parse(&serde_json::json!({"version":1,"title":"Synthetic project","repository":"https://example.test/team/project","issues":issues}).to_string()).expect("synthetic Board"));
                let source = (1..=240).map(|n| {
                    let mut issue: okilum_t3::board::Issue = serde_json::from_value(serde_json::json!({"number":n,"title":titles[(n as usize - 1) % titles.len()],"state":"open","updated_at":chrono::DateTime::from_timestamp((at - n * 90) as i64, 0).unwrap().to_rfc3339(),"labels":[{"name":match n % 4 { 0=>"ci", 1=>"bug", 2=>"fr", _=>"task" }},{"name":if n % 2 == 0 { "from-oleg" } else { "from-tester" }},{"name":"epic: Board v2"}]})).unwrap();
                    if n <= 8 { issue.open_pull_requests.push(1000 + n); issue.live_pulls.push(okilum_t3::board::live::Pull { number: 1000 + n, merged_at: None, checks: match n % 4 { 0=>"failure", 1=>"success", 2=>"pending", _=>"unknown" }.into() }); }
                    (n, issue)
                }).collect();
                board.snapshot.forgejo.value = source;
                board.snapshot.forgejo.last_success = Some(at);
                board.snapshot.t3.value = vec![serde_json::from_value(serde_json::json!({"threadId":"fixture-agent","projectId":"fixture-project","title":"Synthetic agent","status":"running","providerInstanceId":"codex","model":"synthetic","runtimeMode":"full-access","activeRunId":"fixture-run","updatedAt":chrono::DateTime::from_timestamp((at-180) as i64,0).unwrap().to_rfc3339()})).unwrap()];
                board.local_cards.last_success = Some(at);
                board.list.preferences.mode = Mode::List;
                board.selected = Some(1);
                board
            });
            cx.new(|cx| Root::new(board, window, cx))
        }).expect("synthetic Board window");
    }
}
