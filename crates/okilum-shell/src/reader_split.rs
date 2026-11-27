//! Re-exports shared split pane + keeps Reader pointer gesture tests (#3238).
use super::*;
pub(crate) use okilum_shell_viewers::reader_split::*;

#[cfg(test)]
#[path = "reader_pointer_gesture_tests.rs"]
mod pointer_gesture_tests;
