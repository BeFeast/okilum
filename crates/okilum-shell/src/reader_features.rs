//! Optional modules are application preferences, never vault metadata.
use super::*;
use okilum_t3::NetworkAccess;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Feature {
    Agents,
    SecondBrain,
    Sync,
}
impl Feature {
    pub(crate) const ALL: [Self; 3] = [Self::Agents, Self::SecondBrain, Self::Sync];
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::SecondBrain => "second_brain",
            Self::Sync => "sync",
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Agents => "Agents",
            Self::SecondBrain => "Second brain",
            Self::Sync => "Sync",
        }
    }
    pub(crate) fn available(self) -> bool {
        match self {
            Self::Agents => true,
            Self::SecondBrain => cfg!(all(unix, feature = "brain")),
            Self::Sync => cfg!(any(target_os = "linux", target_os = "macos")),
        }
    }
    pub(crate) fn description(self) -> &'static str {
        if !self.available() {
            return "This module is not available on this platform yet.";
        }
        match self {
            Self::Agents => {
                "Projects, Board, Manager and T3. Set up a connection when you first use it."
            }
            Self::SecondBrain => {
                "Open a managed workspace. Your notes remain available in the editor."
            }
            Self::Sync => {
                "Keep folders in sync. Choose and connect a folder when you first use it."
            }
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Agents => 0,
            Self::SecondBrain => 1,
            Self::Sync => 2,
        }
    }
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    agents: bool,
    second_brain: bool,
    sync: bool,
}
impl Saved {
    fn get(self, feature: Feature) -> bool {
        match feature {
            Feature::Agents => self.agents,
            Feature::SecondBrain => self.second_brain,
            Feature::Sync => self.sync,
        }
    }
    fn set(&mut self, feature: Feature, value: bool) {
        match feature {
            Feature::Agents => self.agents = value,
            Feature::SecondBrain => self.second_brain = value,
            Feature::Sync => self.sync = value,
        }
    }
}
/// Observe this global to update UI/lifecycle immediately, including future Now.
#[derive(Clone)]
pub(crate) struct Features {
    saved: Saved,
    permits: [NetworkAccess; 3],
    path: Option<PathBuf>,
    error: Option<String>,
}
impl Global for Features {}
impl Features {
    fn new(saved: Saved, path: Option<PathBuf>) -> Self {
        Self {
            saved,
            permits: Feature::ALL.map(|feature| NetworkAccess::new(saved.get(feature))),
            path,
            error: None,
        }
    }
}
pub(crate) fn install_defaults(cx: &mut App) {
    let mut state = Features::new(Saved::default(), None);
    state.error =
        Some("Application preferences are unavailable. Optional modules remain off.".into());
    cx.set_global(state);
}

pub(crate) fn install(directory: &Path, cx: &mut App) {
    // Publish fail-closed defaults even when the profile is unreadable.
    let path = directory.join("features.json");
    let saved = if path.exists() {
        std::fs::symlink_metadata(&path)
            .ok()
            .filter(|m| m.is_file() && m.len() <= 16 * 1024)
            .and_then(|_| std::fs::read(&path).ok())
            .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
    } else {
        Some(Saved::default())
    };
    let mut state = Features::new(saved.unwrap_or_default(), Some(path));
    if saved.is_none() {
        state.error = Some(
            "Could not read module preferences. The original file is preserved; modules are off."
                .into(),
        );
    }
    cx.set_global(state);
}
/// Absent or unreadable application preferences never grant module access.
pub(crate) fn enabled(feature: Feature, cx: &App) -> bool {
    cx.try_global::<Features>()
        .is_some_and(|state| state.saved.get(feature))
}
pub(crate) fn access(feature: Feature, cx: &App) -> NetworkAccess {
    cx.try_global::<Features>()
        .map(|state| state.permits[feature.index()].clone())
        .unwrap_or_else(|| NetworkAccess::new(false))
}
pub(crate) fn set_enabled(feature: Feature, enabled: bool, cx: &mut App) {
    let mut state = cx
        .try_global::<Features>()
        .cloned()
        .unwrap_or_else(|| Features::new(Saved::default(), None));
    if state.saved.get(feature) == enabled {
        return;
    }
    if state.error.is_some() && enabled {
        return;
    }
    let mut saved = state.saved;
    saved.set(feature, enabled);
    if let Some(path) = &state.path {
        let result = (|| -> anyhow::Result<()> {
            use std::io::Write;
            let parent = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Missing profile directory"))?;
            std::fs::create_dir_all(parent)?;
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(parent.join("features.lock"))?;
            lock.try_lock()?;
            let mut latest: serde_json::Value = if path.exists() {
                anyhow::ensure!(
                    std::fs::symlink_metadata(path)?.is_file()
                        && std::fs::metadata(path)?.len() <= 16 * 1024,
                    "Invalid module preferences file"
                );
                serde_json::from_slice(&std::fs::read(path)?)?
            } else {
                serde_json::json!({})
            };
            let _: Saved = serde_json::from_value(latest.clone())?;
            latest[feature.key()] = serde_json::Value::Bool(enabled);
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(&serde_json::to_vec(&latest)?)?;
            file.as_file().sync_all()?;
            file.persist(path)?;
            Ok(())
        })();
        if let Err(error) = result {
            state.error = Some(format!("Could not save module preferences: {error}"));
            // Refusing persistence must never prevent an immediate local stop.
            if !enabled {
                state.permits[feature.index()].revoke();
                state.permits[feature.index()] = NetworkAccess::new(false);
                state.saved.set(feature, false);
            }
            cx.set_global(state);
            if !enabled {
                suspend(feature, cx);
            }
            cx.refresh_windows();
            return;
        }
    }
    state.permits[feature.index()].revoke();
    state.permits[feature.index()] = NetworkAccess::new(enabled);
    state.saved = saved;
    cx.set_global(state);
    if !enabled {
        suspend(feature, cx);
    }
    cx.refresh_windows();
}
fn suspend(feature: Feature, cx: &mut App) {
    if feature == Feature::Agents {
        reader_t3_settings::suspend(cx);
        cx.set_global(okilum_shell::project_avatar::ProjectIcons::default());
    }
    #[cfg(target_os = "linux")]
    if feature == Feature::Sync {
        reader_settings_sync::suspend(cx);
    }
}

pub(crate) fn error(cx: &App) -> Option<String> {
    cx.try_global::<Features>().and_then(|s| s.error.clone())
}

impl Reader {
    pub(super) fn features_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !enabled(Feature::Agents, cx) {
            self.leave_vault_project(window, cx);
            self.title_nav.close();
            self.release_picker_focus(window, cx);
            self.project_navigation.generation = self.project_navigation.generation.wrapping_add(1);
            self.project_navigation.loading = false;
            self.project_navigation.board_visible = false;
            for board in self.project_navigation.boards.values() {
                board.update(cx, |board, cx| board.set_visible(false, window, cx));
            }
            self.project_navigation.discovered = false;
            self.project_navigation.discovery = Default::default();
            self.project_navigation.t3_gateways.clear();
            self.project_navigation.statuses.clear();
            self.project_navigation.status_managers.clear();

            for project in self.project_views.values() {
                project.update(cx, |view, cx| view.suspend_module(window, cx));
            }
            if self.project_navigation.section == reader_project_navigation::Section::Projects {
                self.project_navigation.section = reader_project_navigation::Section::Reader;
            }
        }
        self.refresh_palette_sections(cx);
        cx.notify();
    }
}

#[cfg(all(unix, feature = "brain"))]
pub(crate) fn open_second_brain(cx: &mut App) {
    if !enabled(Feature::SecondBrain, cx) {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(window_state::default_bounds(
            size(px(1200.), px(800.)),
            cx,
        ))),
        ..TitleBar::window_options()
    };
    let _ = cx.open_window(options, |window, cx| {
        window.set_window_title("Okilum — Second brain");
        let view = cx.new(|cx| {
            workspace::Workspace::new(
                Opts {
                    managed_workspace: true,
                    ..Opts::default()
                },
                window,
                cx,
            )
        });
        cx.new(|cx| Root::new(view, window, cx))
    });
}

/// Legacy component fixtures exercise modules explicitly. Profile acceptance
/// tests install the production preferences and are never overridden here.
#[cfg(test)]
pub(crate) fn test_init(cx: &mut App) {
    gpui_component::init(cx);
    enable_test_modules(cx);
}

#[cfg(test)]
pub(crate) fn enable_test_modules(cx: &mut App) {
    if !cx.has_global::<Features>() {
        cx.set_global(Features::new(
            Saved {
                agents: true,
                second_brain: true,
                sync: true,
            },
            None,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn missing_application_preferences_never_grant_a_module(cx: &mut TestAppContext) {
        cx.update(|cx| {
            for feature in Feature::ALL {
                assert!(!enabled(feature, cx));
                assert!(!access(feature, cx).allowed());
            }
        });
    }

    #[gpui::test]
    fn new_profile_is_core_only_and_preferences_survive_reopen_without_touching_data(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let note = temp.path().join("note.md");
        let credential = temp.path().join("t3-connections.json");
        std::fs::write(&note, b"# Original\r\nExact bytes\0\xff").unwrap();
        std::fs::write(&credential, br#"{"project":"https://example.test"}"#).unwrap();
        let original = std::fs::read(&note).unwrap();
        let original_connection = std::fs::read(&credential).unwrap();
        cx.update(|cx| {
            install(temp.path(), cx);
            for feature in Feature::ALL {
                assert!(!enabled(feature, cx));
            }
            set_enabled(Feature::Agents, true, cx);
            set_enabled(Feature::SecondBrain, true, cx);
            let worker = access(Feature::Agents, cx);
            assert!(worker.allowed());
            set_enabled(Feature::Agents, false, cx);
            assert!(!worker.allowed());
            set_enabled(Feature::Agents, true, cx);
            assert!(access(Feature::Agents, cx).allowed());
            assert!(!worker.allowed(), "old workers never regain access");
            install(temp.path(), cx);
            assert!(enabled(Feature::Agents, cx));
            assert!(enabled(Feature::SecondBrain, cx));
            assert!(!enabled(Feature::Sync, cx));
        });
        assert_eq!(std::fs::read(&note).unwrap(), original);
        assert_eq!(std::fs::read(&credential).unwrap(), original_connection);
    }

    #[gpui::test]
    fn invalid_profile_is_preserved_and_disabling_still_revokes_when_storage_fails(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("features.json");
        std::fs::write(&path, b"{broken original").unwrap();
        cx.update(|cx| {
            install(temp.path(), cx);
            assert!(error(cx).is_some());
            for feature in Feature::ALL {
                assert!(!enabled(feature, cx));
            }
            set_enabled(Feature::Agents, true, cx);
            assert!(!enabled(Feature::Agents, cx));
        });
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken original");
        std::fs::remove_file(&path).unwrap();
        cx.update(|cx| {
            install(temp.path(), cx);
            set_enabled(Feature::Agents, true, cx);
            let worker = access(Feature::Agents, cx);
            std::fs::write(&path, b"{concurrent corruption").unwrap();
            set_enabled(Feature::Agents, false, cx);
            assert!(!enabled(Feature::Agents, cx));
            assert!(!worker.allowed());
            assert!(error(cx).is_some());
        });
        assert_eq!(std::fs::read(&path).unwrap(), b"{concurrent corruption");
    }
}
