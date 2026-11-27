//! Native harness and Reader-integration tests for delimited tables (#3238).
use super::*;
pub(crate) use okilum_shell_viewers::reader_delimited::bind_keys;
pub(crate) use okilum_shell_viewers::reader_delimited::*;

#[cfg(feature = "data-view-native-harness")]
pub(crate) mod native_harness;
#[cfg(test)]
mod toolbar_tests;
