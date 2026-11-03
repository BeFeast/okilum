//! Synthetic, opt-in reference workload. Never uses an installed profile or real thread.
use super::*;
use okilum_t3::projection::{Changes, RowKey, TurnRow};
use serde_json::json;

pub(super) fn populate(project: &Entity<ProjectView>, cx: &mut App) {
    project.update(cx, |view, cx| {
        view.generation += 1; // Discard the fixture gateway's initial asynchronous read.
        view.polling = true;
        view.chat.as_mut().unwrap().project_id = Some("fixture-managers".into());
        view.identity = Some(ServerIdentity { environment_id: "fixture".into(), label: "Fixture".into() });
        let target = view.manager_model_target().unwrap();
        view.models = Some(target.load(view.gateway.as_ref().unwrap().as_ref()).unwrap());
        view.model_target = Some(target);
        let mut rows = Vec::new();
        let mut runs = Vec::new();
        for turn in 0..200 {
            let run = format!("run-{turn}");
            runs.push(json!({"id":run,"threadId":"mcp:fixture-manager","ordinal":turn,"status":"completed"}));
            for (part, kind) in ["user_message", "command_execution", "file_search", "command_execution", "assistant_message", "assistant_message"].into_iter().enumerate() {
                let id = format!("item-{turn}-{part}");
                let text = format!("## Synthetic message {turn}/{part}\n\nRetain **source identity**, [documentation](https://example.test) and tool output.\n\n- Verify the local fixture.\n- Keep the next delivery available.\n\n```rust\nfn example() {{ println!(\"fixture\"); }}\n```\n\n{}", "A realistic paragraph wraps within the Manager reading column. ".repeat(8));
                rows.push(Arc::new(TurnRow {
                    key: RowKey { thread:"mcp:fixture-manager".into(), item:id.clone() },
                    local:true,
                    item:Arc::new(json!({"id":id,"threadId":"mcp:fixture-manager","ordinal":turn*6+part,"type":kind,"status":"completed","runId":run,"text":text,"input":"cargo check --locked","output":"Synthetic tool output\nAll checks passed."}))
                }));
            }
        }
        view.feed.update(cx, |feed, cx| {
            feed.set_timeline_runs("mcp:fixture-manager", &runs, None, cx);
            feed.apply_native(Changes { order:Some(rows.iter().map(|r| r.key.clone()).collect()), rows, ..Default::default() }, &[], cx);
        });
        cx.notify();
    });
}

#[cfg(feature = "project-ui-harness")]
pub(super) fn collect(cx: &mut App) {
    if std::env::var("OKILUM_PROJECT_UI_FIXTURE").as_deref() != Ok("performance") {
        return;
    }
    gpui::profiler::set_trace_enabled(true);
    let mut collector = gpui::profiler::FrameTimingCollector::new();
    cx.spawn(async move |_cx| {
        loop {
            gpui::Timer::after(Duration::from_millis(250)).await;
            let work = std::mem::replace(&mut *WORK.lock().unwrap(), [(0, 0); 3]);
            eprintln!("WORK {}", json!({"unix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64(),"board":work[0],"card":work[1],"row":work[2]}));
            for event in collector.collect_unseen() {
                if let gpui::profiler::FrameEvent::Present(frame) = event {
                    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64() - frame.present_end.elapsed().as_secs_f64();
                    eprintln!("PRESENT {}", json!({"unix":unix,"present_ms":frame.present_duration().as_secs_f64()*1000.}));
                }
                if let gpui::profiler::FrameEvent::Draw(frame) = event {
                    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64() - frame.draw_end.elapsed().as_secs_f64();
                    eprintln!("PERF {}", json!({"unix":unix,"draw_ms":frame.draw_duration().as_secs_f64()*1000.,"invalidations":frame.invalidations}));
                }
            }
        }
    }).detach();
}

#[cfg(feature = "project-ui-harness")]
static WORK: std::sync::Mutex<[(u64, u64); 3]> = std::sync::Mutex::new([(0, 0); 3]);
#[cfg(feature = "project-ui-harness")]
pub(super) struct Work(usize, std::time::Instant);
#[cfg(feature = "project-ui-harness")]
impl Work {
    pub(super) fn start(kind: usize) -> Self {
        Self(kind, std::time::Instant::now())
    }
}
#[cfg(feature = "project-ui-harness")]
impl Drop for Work {
    fn drop(&mut self) {
        let mut work = WORK.lock().unwrap();
        work[self.0].0 += 1;
        work[self.0].1 += self.1.elapsed().as_nanos() as u64;
    }
}
