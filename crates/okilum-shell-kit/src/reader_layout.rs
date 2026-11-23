//! Presentation state only: opening a panel never owns document/navigation state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Panel {
    #[default]
    Closed,
    Notes,
    Backlinks,
}

/// Wide visibility is independent; compact presentation exposes only the last
/// requested side. Viewport changes never mutate the user's wide choices.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Panels {
    pub notes: bool,
    pub backlinks: bool,
    pub active: Panel,
    /// A temporary window-local view; saved preferences retain the original sides.
    #[serde(skip)]
    pub maximized_restore: Option<(bool, bool, Panel)>,
}
impl Panels {
    pub fn maximized(&self) -> bool {
        self.maximized_restore.is_some()
    }

    pub fn toggle_maximized(&mut self) {
        if self.restore_maximized() {
            return;
        }
        self.maximized_restore = Some((self.notes, self.backlinks, self.active));
        self.notes = false;
        self.backlinks = false;
        self.active = Panel::Closed;
    }

    fn restore_maximized(&mut self) -> bool {
        let Some((notes, backlinks, active)) = self.maximized_restore.take() else {
            return false;
        };
        self.notes = notes;
        self.backlinks = backlinks;
        self.active = active;
        true
    }

    pub fn preferences(&self) -> Self {
        let mut saved = self.clone();
        saved.restore_maximized();
        saved
    }

    /// Automatic tiling must not turn a docked panel into an obstruction.
    /// Keep the wide choices; only explicit opens activate a compact overlay.
    pub fn viewport_changed(&mut self, previous: f32, current: f32) {
        if !overlay(previous) && overlay(current) {
            self.active = Panel::Closed;
            if let Some((_, _, active)) = &mut self.maximized_restore {
                *active = Panel::Closed;
            }
        }
    }

    pub fn visible(&self, panel: Panel, available: f32) -> bool {
        let enabled = match panel {
            Panel::Notes => self.notes,
            Panel::Backlinks => self.backlinks,
            Panel::Closed => false,
        };
        enabled && (!overlay(available) || self.active == panel)
    }
    pub fn open(&mut self, panel: Panel) {
        if panel == Panel::Closed {
            return;
        }
        self.restore_maximized();
        match panel {
            Panel::Notes => self.notes = true,
            Panel::Backlinks => self.backlinks = true,
            Panel::Closed => return,
        }
        self.active = panel;
    }
    pub fn close(&mut self, panel: Panel) {
        if panel == Panel::Closed {
            return;
        }
        self.restore_maximized();
        match panel {
            Panel::Notes => self.notes = false,
            Panel::Backlinks => self.backlinks = false,
            Panel::Closed => return,
        }
        if self.active == panel {
            self.active = Panel::Closed;
        }
    }
    pub fn dismiss_target(&self, available: f32) -> Panel {
        if self.visible(self.active, available) {
            self.active
        } else if self.visible(Panel::Notes, available) {
            Panel::Notes
        } else if self.visible(Panel::Backlinks, available) {
            Panel::Backlinks
        } else {
            Panel::Closed
        }
    }
    pub fn widths(&self, preferred: &Widths, available: f32) -> Widths {
        let mut result = Widths {
            notes: 0.,
            backlinks: 0.,
        };
        if self.visible(Panel::Notes, available) {
            result.notes = displayed_width(preferred.notes, available);
        }
        if self.visible(Panel::Backlinks, available) {
            result.backlinks = displayed_width(preferred.backlinks, available);
        }
        let budget = (available - DOCUMENT_MIN_WIDTH).max(0.);
        if !overlay(available)
            && result.notes > 0.
            && result.backlinks > 0.
            && result.notes + result.backlinks > budget
        {
            let extra = result.notes + result.backlinks - 2. * PANEL_MIN_WIDTH;
            let room = (budget - 2. * PANEL_MIN_WIDTH).max(0.);
            let left_share = if extra > 0. {
                (result.notes - PANEL_MIN_WIDTH) / extra
            } else {
                0.5
            };
            result.notes = PANEL_MIN_WIDTH + room * left_share;
            result.backlinks = PANEL_MIN_WIDTH + room * (1. - left_share);
        }
        result
    }
    pub fn resize_width(
        &self,
        panel: Panel,
        requested: f32,
        preferred: &Widths,
        available: f32,
    ) -> f32 {
        let displayed = self.widths(preferred, available);
        let sibling = if panel == Panel::Notes {
            displayed.backlinks
        } else {
            displayed.notes
        };
        let maximum = if overlay(available) {
            available - 48.
        } else {
            available - DOCUMENT_MIN_WIDTH - sibling
        };
        requested.max(PANEL_MIN_WIDTH).min(maximum.max(0.))
    }
}

pub const DEFAULT_PANEL_WIDTH: f32 = 280.;

/// A view's preferred presentation, independent of viewport clamps or maximize.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ViewLayout {
    pub panels: Panels,
    pub widths: Widths,
}
impl ViewLayout {
    pub fn capture(panels: &Panels, widths: &Widths) -> Self {
        Self {
            panels: panels.preferences(),
            widths: widths.clone(),
        }
    }

    pub fn restore(&self) -> (Panels, Widths) {
        let mut widths = Widths::default();
        widths.set(Panel::Notes, self.widths.notes);
        widths.set(Panel::Backlinks, self.widths.backlinks);
        (self.panels.preferences(), widths)
    }
}
pub const PANEL_MIN_WIDTH: f32 = 200.;
pub const DOCUMENT_MIN_WIDTH: f32 = 480.;
pub const DOCK_MIN_WIDTH: f32 = 1000.;

pub fn overlay(width: f32) -> bool {
    width < DOCK_MIN_WIDTH
}

/// Preferred sizes are independent of transient viewport clamps.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Widths {
    pub notes: f32,
    pub backlinks: f32,
}
impl Default for Widths {
    fn default() -> Self {
        Self {
            notes: DEFAULT_PANEL_WIDTH,
            backlinks: DEFAULT_PANEL_WIDTH,
        }
    }
}
impl Widths {
    pub fn get(&self, panel: Panel) -> f32 {
        match panel {
            Panel::Backlinks => self.backlinks,
            _ => self.notes,
        }
    }
    pub fn set(&mut self, panel: Panel, width: f32) {
        if !width.is_finite() {
            return;
        }
        let width = width.clamp(PANEL_MIN_WIDTH, 1200.);
        match panel {
            Panel::Notes => self.notes = width,
            Panel::Backlinks => self.backlinks = width,
            Panel::Closed => {}
        }
    }
    pub fn load(path: &std::path::Path) -> Self {
        let mut result: Self = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        result.set(Panel::Notes, result.notes);
        result.set(Panel::Backlinks, result.backlinks);
        result
    }
    pub fn save_panel(&self, panel: Panel, path: &std::path::Path) -> std::io::Result<()> {
        // Multiple Reader windows share settings; never write a stale sibling width.
        let mut latest = Self::load(path);
        latest.set(panel, self.get(panel));
        latest.save(path)
    }
    fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("missing settings directory"))?;
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".reader-layout-{}.json", uuid::Uuid::new_v4()));
        let result = (|| {
            std::fs::write(&temporary, serde_json::to_vec(self)?)?;
            std::fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
}

pub fn displayed_width(preferred: f32, available: f32) -> f32 {
    let reserve = if overlay(available) {
        48.
    } else {
        DOCUMENT_MIN_WIDTH
    };
    preferred
        .max(PANEL_MIN_WIDTH)
        .min((available - reserve).max(0.))
}

/// Never put presentation preferences into the selected canonical tree, even
/// when a custom XDG location points there or through an existing symlink.
pub fn settings_path(vault: &std::path::Path) -> Option<std::path::PathBuf> {
    settings_path_at(vault, config_base()?.join("okilum/reader-layout.json"))
}

/// Per-user application config directory; `None` when it cannot be absolute.
pub fn config_base() -> Option<std::path::PathBuf> {
    if let Some(state) = crate::reader_history::isolated_state_directory() {
        return Some(state.join("config"));
    }
    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            home.join(if cfg!(target_os = "macos") {
                "Library/Application Support"
            } else {
                ".config"
            })
        });
    #[cfg(windows)]
    let base = dirs::config_dir()?;
    base.is_absolute().then_some(base)
}

/// Validate an exact preference destination off the UI executor.
pub fn settings_path_at(
    vault: &std::path::Path,
    path: std::path::PathBuf,
) -> Option<std::path::PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return None;
    }
    let ancestor = path.ancestors().find(|p| p.exists())?.canonicalize().ok()?;
    let vault = vault.canonicalize().ok()?;
    if ancestor.starts_with(&vault) || path.starts_with(&vault) {
        return None;
    }
    Some(path)
}

/// Reader measure (docs/design/reader.md): a 740px column with 40px side
/// padding keeps lines near 75 characters. It is a maximum: narrower panes
/// still use their full width.
pub const READER_MAX_WIDTH: f32 = 740.;
