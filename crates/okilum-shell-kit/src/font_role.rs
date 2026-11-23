//! Which document font a choice applies to: notes or code.
use crate::brand;

#[derive(Clone, Copy)]
pub enum Role {
    Note,
    Code,
}
impl Role {
    pub fn index(self) -> usize {
        match self {
            Self::Note => 0,
            Self::Code => 1,
        }
    }
    pub fn default_family(self) -> &'static str {
        match self {
            Self::Note => brand::SANS_FONT,
            Self::Code => brand::MONO_FONT,
        }
    }
}
