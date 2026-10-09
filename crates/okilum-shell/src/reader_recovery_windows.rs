//! No editor recovery markers are needed in the read-only Windows diagnostic.
use gpui::{App, Global};
use std::path::Path;

pub(crate) struct RecoveryStartup(pub bool);
impl Global for RecoveryStartup {}

pub(crate) fn is_recovering(_: &App) -> bool {
    false
}
pub(crate) fn install(_: &Path, _: Option<&Path>, _: &mut App) {}
