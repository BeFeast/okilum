//! Native Windows links the UI thread with a 1 MiB stack reserve. The Reader
//! read/edit draws of a debug build must fit there (#2825). Each draw runs in
//! a child test process whose libtest thread gets exactly that stack, so an
//! overflow fails this test instead of aborting the whole suite.
use super::*;
use ::core::prelude::v1::test;
use std::{process::Command, time::Duration};

const CHILD: &str = "OKILUM_STACK2825_CHILD";
/// The unchanged PE stack reserve recorded by the native Windows probes.
const WINDOWS_MAIN_STACK: usize = 1024 * 1024;

fn draw(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
}

fn read_then_edit(cx: &mut TestAppContext, name: &str) {
    cx.executor().allow_parking();
    let (reader, visual, _temp) =
        crate::reader_viewer_chrome::tests::fixture_with_files(cx, name, &[]);
    visual.simulate_resize(size(px(1100.), px(800.)));
    draw(visual);
    assert_eq!(reader.read_with(visual, |r, _| r.selected_file().to_owned()), name);
    assert!(reader.read_with(visual, |r, _| r.editing.is_none()));
    println!("STACK2825_CHILD file={name} mode=read");
    reader.update_in(visual, |r, window, cx| {
        window.activate_window();
        r.toggle_source(window, cx);
    });
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(130));
    draw(visual);
    assert!(
        reader.read_with(visual, |r, _| r.editing.is_some()),
        "{name} entered Edit"
    );
    draw(visual);
    println!("STACK2825_CHILD file={name} mode=edit");
}

#[gpui::test]
fn stack2825_child_file_read_edit(cx: &mut TestAppContext) {
    if std::env::var_os(CHILD).is_some() {
        read_then_edit(cx, "data.json");
    }
}

#[gpui::test]
fn stack2825_child_note_read_edit(cx: &mut TestAppContext) {
    if std::env::var_os(CHILD).is_some() {
        read_then_edit(cx, "note.md");
    }
}

fn run_child(test: &str, stack: usize) -> (bool, String) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("RUST_MIN_STACK", stack.to_string())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

#[test]
fn stack2825_reader_read_edit_draws_fit_the_windows_main_stack() {
    for (test, name) in [
        (
            "issue2825_stack_tests::stack2825_child_file_read_edit",
            "data.json",
        ),
        (
            "issue2825_stack_tests::stack2825_child_note_read_edit",
            "note.md",
        ),
    ] {
        let (passed, text) = run_child(test, WINDOWS_MAIN_STACK);
        assert!(
            passed
                && text.contains(&format!("STACK2825_CHILD file={name} mode=read"))
                && text.contains(&format!("STACK2825_CHILD file={name} mode=edit"))
                && text.contains("1 passed"),
            "{name} Read -> Edit must draw within the 1 MiB native stack:\n{text}"
        );
    }
    // Control: the child's stack limit actually applies to this draw path.
    let (passed, text) = run_child(
        "issue2825_stack_tests::stack2825_child_file_read_edit",
        64 * 1024,
    );
    assert!(
        !passed && text.contains("has overflowed its stack"),
        "a 64 KiB child stack must overflow:\n{text}"
    );
}
