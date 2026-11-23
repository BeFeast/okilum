//! Paste settings kept in the UI state store; the paste engine stays in the app.
use serde::{Deserialize, Serialize};

/// Default for Settings → Editor → "Ask before copying files larger than".
pub const DEFAULT_ASK_ABOVE_MB: u32 = 100;

/// Global image import policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub enabled: bool,
    pub quality: u32,
    pub keep_original: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            enabled: true,
            quality: 85,
            keep_original: false,
        }
    }
}

impl Options {
    pub fn normalized(self) -> Self {
        Self {
            quality: self.quality.clamp(1, 100),
            ..self
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}
