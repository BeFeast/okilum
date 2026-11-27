//! Re-exports shared find field + keeps Reader-hosted tests (#3238).
use super::*;
pub(crate) use okilum_shell_viewers::reader_find_field::*;

#[cfg(test)]
mod tests;
