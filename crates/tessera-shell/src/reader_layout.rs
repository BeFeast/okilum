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
}
impl Panels {
    /// Automatic tiling must not turn a docked panel into an obstruction.
    /// Keep the wide choices; only explicit opens activate a compact overlay.
    pub fn viewport_changed(&mut self, previous: f32, current: f32) {
        if !overlay(previous) && overlay(current) {
            self.active = Panel::Closed;
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
        match panel {
            Panel::Notes => self.notes = true,
            Panel::Backlinks => self.backlinks = true,
            Panel::Closed => return,
        }
        self.active = panel;
    }
    pub fn close(&mut self, panel: Panel) {
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
    settings_path_at(vault, config_base()?.join("tessera/reader-layout.json"))
}

/// Per-user application config directory; `None` when it cannot be absolute.
pub fn config_base() -> Option<std::path::PathBuf> {
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
pub(crate) fn settings_path_at(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiling_hides_panels_without_losing_wide_choices() {
        let mut panels = Panels::default();
        panels.open(Panel::Notes);
        panels.open(Panel::Backlinks);
        panels.viewport_changed(1366., 666.);
        assert!(!panels.visible(Panel::Notes, 666.));
        assert!(!panels.visible(Panel::Backlinks, 666.));
        assert!(panels.visible(Panel::Notes, 1366.));
        assert!(panels.visible(Panel::Backlinks, 1366.));
        panels.open(Panel::Notes);
        panels.viewport_changed(666., 640.);
        assert!(panels.visible(Panel::Notes, 640.));
        assert!(!panels.visible(Panel::Backlinks, 640.));
    }

    #[test]
    fn sides_are_independent_and_compact_visibility_restores_wide_choices() {
        let mut panels = Panels::default();
        panels.open(Panel::Notes);
        panels.open(Panel::Backlinks);
        assert!(panels.visible(Panel::Notes, 1366.));
        assert!(panels.visible(Panel::Backlinks, 1366.));
        assert!(!panels.visible(Panel::Notes, 640.));
        assert!(panels.visible(Panel::Backlinks, 640.));
        panels.open(Panel::Notes);
        assert!(panels.visible(Panel::Notes, 640.));
        assert!(!panels.visible(Panel::Backlinks, 640.));
        panels.close(Panel::Notes);
        assert_eq!(panels.dismiss_target(640.), Panel::Closed);
        assert!(panels.visible(Panel::Backlinks, 1366.));
    }

    #[test]
    fn simultaneous_panels_reserve_document_and_keep_preferences() {
        let mut panels = Panels::default();
        panels.open(Panel::Notes);
        panels.open(Panel::Backlinks);
        let preferred = Widths {
            notes: 650.,
            backlinks: 350.,
        };
        for available in [1000., 1100., 1366., 1600.] {
            let displayed = panels.widths(&preferred, available);
            assert!(displayed.notes >= PANEL_MIN_WIDTH);
            assert!(displayed.backlinks >= PANEL_MIN_WIDTH);
            assert!(available - displayed.notes - displayed.backlinks >= DOCUMENT_MIN_WIDTH - 0.01);
            assert!(
                panels.resize_width(Panel::Notes, 1200., &preferred, available)
                    + displayed.backlinks
                    + DOCUMENT_MIN_WIDTH
                    <= available + 0.01
            );
        }
        let narrow = panels.widths(&preferred, 640.);
        assert_eq!(narrow.notes, 0.);
        assert_eq!(narrow.backlinks, 350.);
        let restored = panels.widths(&preferred, 1600.);
        assert_eq!(restored.notes, 650.);
        assert_eq!(restored.backlinks, 350.);
    }

    #[test]
    fn preferred_widths_survive_clamps_and_persist_independently() {
        let root = std::env::temp_dir().join(format!("reader-widths-{}", uuid::Uuid::new_v4()));
        let path = root.join("reader-layout.json");
        let mut widths = Widths::default();
        widths.set(Panel::Notes, 650.);
        widths.set(Panel::Backlinks, 350.);
        assert_eq!(displayed_width(widths.notes, 640.), 592.);
        assert_eq!(displayed_width(widths.notes, 1000.), 520.);
        assert_eq!(displayed_width(widths.notes, 1366.), 650.);
        assert_eq!(widths.notes, 650.);
        widths.save(&path).unwrap();
        let loaded = Widths::load(&path);
        assert_eq!(loaded.notes, 650.);
        assert_eq!(loaded.backlinks, 350.);
        let mut stale_second_window = Widths::default();
        stale_second_window.set(Panel::Backlinks, 410.);
        stale_second_window
            .save_panel(Panel::Backlinks, &path)
            .unwrap();
        let merged = Widths::load(&path);
        assert_eq!(merged.notes, 650.);
        assert_eq!(merged.backlinks, 410.);
        std::fs::remove_dir_all(root).unwrap();
    }
}
