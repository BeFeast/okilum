//! Pure pinned-file data and labels shared by the pin strip (#3238, epic #3241).

/// Height of the single horizontal pin row above the note/file pane.
pub const PIN_ROW_HEIGHT: f32 = 34.;

/// One persisted pin in the current vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedFile {
    pub path: String,
    pub label: String,
    /// Moved or deleted outside Okilum: not in the complete inventory.
    pub missing: bool,
}

/// The menu label for `rel`'s pin.
pub fn pin_label(pinned: bool) -> &'static str {
    if pinned {
        "Unpin"
    } else {
        "Pin"
    }
}
