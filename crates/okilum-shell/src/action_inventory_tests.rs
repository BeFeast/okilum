//! Every GPUI action registered before the okilum-shell crate split stays
//! registered (#3232, epic #3241).
//!
//! Action names come from the literal namespace in `actions!` / `#[action]`,
//! not from the crate, so moving them into an `okilum-shell-*` crate keeps the
//! names users bind in keymaps. Registration goes through `inventory`: if the
//! linker drops a crate's object file, its actions vanish without a compile
//! error. This test catches that. New actions are allowed; a deliberate rename
//! or removal edits `tests/fixtures/actions.txt` in the same PR.
//!
//! Regenerate: `OKILUM_WRITE_ACTION_GOLDEN=<path> cargo test -p okilum-shell
//! --bin okilum action_inventory_tests` writes the current list to <path>.

use std::collections::BTreeSet;

const GOLDEN: &str = include_str!("../tests/fixtures/actions.txt");

fn golden() -> Vec<&'static str> {
    GOLDEN
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

#[test]
fn registered_actions_include_every_pre_split_action() {
    let registered: BTreeSet<&'static str> = gpui::generate_list_of_all_registered_actions()
        .map(|action| action.name)
        .collect();
    if let Some(path) = std::env::var_os("OKILUM_WRITE_ACTION_GOLDEN") {
        let list: String = registered.iter().map(|name| format!("{name}\n")).collect();
        std::fs::write(&path, list).expect("write the action list");
    }
    let golden = golden();
    assert!(
        golden.len() > 100,
        "tests/fixtures/actions.txt holds {} actions; regenerate it (see module docs)",
        golden.len()
    );
    let brain_absent = cfg!(not(all(unix, feature = "brain")));
    let missing: Vec<_> = golden
        .iter()
        // `mod brain` only exists with `all(unix, feature = "brain")`; the golden
        // is generated on Linux with it, so other builds skip those names.
        .filter(|name| !(brain_absent && name.starts_with("brain::")))
        .filter(|name| !registered.contains(*name))
        .collect();
    assert!(
        missing.is_empty(),
        "{} registered action(s) disappeared: {missing:?}. A moved crate must stay linked \
         into the binary; a deliberate rename updates tests/fixtures/actions.txt",
        missing.len()
    );
}
