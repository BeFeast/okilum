//! Non-shipping real event-loop checks for large/wide table operations.
use super::*;
use std::time::{Duration, Instant};

struct Surface(Entity<TablePreview>);
impl Render for Surface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex().size_full().min_h_0().child(self.0.clone())
    }
}

pub(crate) fn entry() -> bool {
    let Some(directory) = std::env::var_os("OKILUM_TABLE_NATIVE_FIXTURE") else {
        return false;
    };
    assert!(!build_info::gpui_test_support());
    let directory = PathBuf::from(directory);
    platform::text_platform::application().with_assets(Assets).run(move |cx| {
        reader_features::install_defaults(cx);
        gpui_component::init(cx);
        bind_keys(cx);
        brand::load_fonts(cx).unwrap();
        brand::apply_theme(cx);
        cx.activate(true);
        cx.spawn(async move |cx| {
            let mut windows = Vec::new();
            for name in ["large.csv", "large.json", "wide.csv"] {
                let started = Instant::now();
                let mut preview = None;
                let handle = cx.update(|cx| cx.open_window(WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| TablePreview::new(directory.join(name), cx));
                    preview = Some(view.clone());
                    cx.new(|cx| Root::new(cx.new(|_| Surface(view)), window, cx))
                })).unwrap();
                let preview = preview.unwrap();
                let mut previous = started;
                let mut maximum = Duration::ZERO;
                let mut ticks = 0;
                let mut phase = 0;
                loop {
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                    let now = Instant::now();
                    maximum = maximum.max(now.duration_since(previous));
                    previous = now;
                    ticks += 1;
                    assert!(maximum < Duration::from_secs(2), "{name}: event loop blocked {maximum:?}");
                    assert!(started.elapsed() < Duration::from_secs(90), "{name}: table never completed");
                    let ready = cx.update(|cx| {
                        let p = preview.read(cx);
                        p.table.is_some() && !p.loading && !p.updating
                    });
                    if !ready { continue; }
                    if phase == 0 {
                        cx.update(|cx| preview.update(cx, |p, cx| {
                            assert!(p.table.as_ref().unwrap().limited, "large file preview must be bounded");
                            p.load(true, cx);
                        }));
                        phase = 1;
                    } else if phase == 1 {
                        cx.update(|cx| preview.update(cx, |p, cx| {
                            assert!(p.table.as_ref().unwrap().rows.len() > 1000);
                            p.query = "needle".into();
                            p.sort = Some((1, false));
                            p.refilter(cx);
                        }));
                        phase = 2;
                    } else if ticks >= 6 { break; }
                }
                cx.update(|cx| preview.update(cx, |p, cx| {
                    assert!(!p.order.is_empty(), "filter must exercise actual rows");
                    assert!(p.table.as_ref().unwrap().cell(p.order[0], 0).contains("needle"));
                    p.anchor = Some(Cell { row: 1, column: 0 });
                    p.end = p.anchor;
                    p.copy(cx);
                    if name == "wide.csv" {
                        assert_eq!(p.table.as_ref().unwrap().columns, 200);
                        assert!(p.visible_columns.len() < 20, "columns are virtualized on the real platform");
                    }
                }));
                let receipt = serde_json::json!({"file":name,"ticks":ticks,"max_event_loop_gap_ms":maximum.as_millis(),"ready_ms":started.elapsed().as_millis(),"real_native_platform":true});
                std::fs::write(directory.join(format!("ready-{name}")), receipt.to_string()).unwrap();
                let wait_started = Instant::now();
                while !directory.join(format!("ack-{name}")).exists() {
                    assert!(wait_started.elapsed() < Duration::from_secs(30));
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                }
                windows.push(handle);
            }
            std::fs::write(directory.join("complete"), b"native tables observed").unwrap();
            cx.update(|cx| cx.quit());
        }).detach();
    });
    true
}
