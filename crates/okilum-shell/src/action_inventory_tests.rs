//! #3232 (epic #3241): the registered action names, captured before the
//! okilum-shell crate split. Actions register through `inventory` at link
//! time; moving `actions!` into another crate must keep every name, and a
//! linker that drops an object file would drop its actions silently.
//!
//! `dump_registered_action_names` writes the sorted list to
//! `$OKILUM_ACTION_NAMES_OUT` (stdout when unset). It is ignored in ordinary
//! runs; `scripts/ci/shell-split-baseline.sh` runs it to refresh
//! `crates/okilum-shell/tests/fixtures/actions.txt`.
use gpui::TestAppContext;

fn registered_action_names(cx: &mut TestAppContext) -> Vec<&'static str> {
    let mut names = cx.update(|cx| cx.all_action_names().to_vec());
    names.sort_unstable();
    names.dedup();
    names
}

/// Control: the list the dump writes comes from the linked registry, so an
/// app action, a namespaced feature action and a gpui-component action are in it.
#[gpui::test]
fn the_action_registry_holds_app_feature_and_component_actions(cx: &mut TestAppContext) {
    let names = registered_action_names(cx);
    for expected in ["okilum::NewNote", "ui_zoom::ZoomIn", "input::Enter"] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }
}

#[gpui::test]
#[ignore = "baseline dump for scripts/ci/shell-split-baseline.sh"]
fn dump_registered_action_names(cx: &mut TestAppContext) {
    let mut text = registered_action_names(cx).join("\n");
    text.push('\n');
    match std::env::var_os("OKILUM_ACTION_NAMES_OUT") {
        Some(path) => std::fs::write(&path, text).expect("write action names"),
        None => print!("{text}"),
    }
}
