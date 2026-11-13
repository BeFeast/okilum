//! External terminal integration. No embedded terminal or automatic command execution.
use super::*;
use gpui_component::{
    input::{Input, InputState},
    WindowExt as _,
};
mod command;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows_console;

#[cfg(test)]
#[derive(Default)]
pub(crate) struct Launches(pub(crate) Vec<PathBuf>);
#[cfg(test)]
impl gpui::Global for Launches {}
gpui::actions!(project_terminal, [OpenTerminal, OpenInTerminal]);

pub(crate) fn bind_keys(cx: &mut App) {
    // Input/editor-specific bindings retain their meaning, including Ctrl+J.
    let context = Some("Reader && !Input && !Editor && !SourceEditor && !Textarea");
    cx.bind_keys([
        KeyBinding::new("secondary-j", OpenTerminal, context),
        KeyBinding::new("ctrl-`", OpenTerminal, context),
        // Next to Reveal (alt-cmd-r) and Copy vault path (alt-cmd-c).
        KeyBinding::new("alt-cmd-t", OpenInTerminal, Some("Reader")),
    ]);
}
fn setting_path(project: &Path) -> anyhow::Result<PathBuf> {
    Ok(setting_file(project, &reader_history::state_directory()?))
}
fn setting_file(project: &Path, state: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let id = Sha256::digest(project.as_os_str().as_encoded_bytes());
    state.join("project-terminal").join(format!("{id:x}.json"))
}
fn saved(project: &Path) -> anyhow::Result<Option<String>> {
    read_saved(&setting_path(project)?)
}
fn read_saved(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
/// A launcher can run: a regular executable file. On Windows the Windows
/// Terminal App Execution Alias is a reparse point that cannot be followed,
/// so its own entry counts.
fn executable(path: &Path) -> bool {
    #[cfg(windows)]
    {
        path.symlink_metadata().is_ok_and(|m| !m.is_dir())
    }
    #[cfg(not(windows))]
    {
        let Ok(metadata) = path.metadata() else {
            return false;
        };
        use std::os::unix::fs::PermissionsExt as _;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
}

/// The Windows system directory from the OS, not from the environment.
#[cfg(windows)]
fn system_directory() -> Option<PathBuf> {
    windows_console::system_directory()
}
#[cfg(not(windows))]
fn system_directory() -> Option<PathBuf> {
    None
}

/// Trusted absolute programs only (see `command::resolve`): never the
/// current directory, a relative PATH entry or anything inside `excluded`.
fn trusted(excluded: Vec<PathBuf>) -> impl Fn(&str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH");
    let system = system_directory();
    move |name| {
        command::resolve(
            name,
            path.as_deref(),
            &excluded,
            system.as_deref(),
            executable,
        )
    }
}

fn initial_for_project(
    project: &okilum_t3::projects::Project,
    state: &Path,
) -> anyhow::Result<command::Initial> {
    use anyhow::Context as _;
    // Re-read settings: the selected project may predate a connection/checkout edit.
    let project = okilum_t3::projects::Project::read(&project.folder)?;
    let document = okilum_t3::area_files::read(&project.folder, "project")?;
    let manifest = document.as_ref().map(|d| &d.value);
    for field in ["terminal_command", "terminal_host"] {
        if let Some(value) = manifest
            .and_then(|d| d.get("execution"))
            .and_then(|d| d.get(field))
        {
            anyhow::ensure!(value.is_string(), "execution.{field} must be a string");
        }
    }
    let custom = read_saved(&setting_file(&project.folder, state))?
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            manifest.and_then(|d| {
                d["execution"]["terminal_command"]
                    .as_str()
                    .map(str::to_owned)
            })
        });
    let host = manifest.and_then(|d| d["execution"]["terminal_host"].as_str());
    // An explicit override does not depend on a working T3 registry.
    let has_t3 = project.environment.is_some()
        || manifest.is_some_and(|d| d["t3"]["origin"].as_str().is_some());
    let origin =
        if !has_t3 || custom.as_ref().is_some_and(|v| !v.trim().is_empty()) || host.is_some() {
            None
        } else {
            project::prefill_browser_origin(
                &project.folder,
                project.environment.as_deref().unwrap_or("project"),
                Some(state),
            )?
        };
    let checkout = project
        .appearance
        .source_checkout
        .as_deref()
        .or_else(|| project.folder.to_str())
        .context("Project checkout is not UTF-8")?;
    command::initial(
        origin.as_deref(),
        project.environment.is_some(),
        host,
        checkout,
        custom.as_deref(),
    )
    .map_err(anyhow::Error::msg)
}
fn launch(project: &okilum_t3::projects::Project) -> anyhow::Result<()> {
    let initial = initial_for_project(project, &reader_history::state_directory()?)?;
    let os = std::env::consts::OS;
    let terminal = std::env::var("TERMINAL").ok();
    let mut excluded = vec![project.folder.clone()];
    excluded.extend(project.folder.canonicalize());
    let launcher = command::launcher(os, terminal.as_deref(), trusted(excluded))
        .map_err(anyhow::Error::msg)?;
    run(command::plan(&initial, &launcher))
}

/// The process for a plan. It inherits Okilum's own working directory, never
/// the selected folder: the folder travels as an argument. An AppImage's own
/// variables are not inherited.
fn process(plan: &command::Plan, get: impl Fn(&str) -> Option<String>) -> std::process::Command {
    let mut process = std::process::Command::new(&plan.program);
    process
        .args(&plan.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    for (name, value) in command::appimage_env(get) {
        match value {
            Some(value) => process.env(name, value),
            None => process.env_remove(name),
        };
    }
    process
}

/// Starts the planned terminal and waits for its launcher, keeping at most
/// 8 KiB of its diagnostics.
fn run(plan: command::Plan) -> anyhow::Result<()> {
    use anyhow::Context as _;
    // PowerShell without Windows Terminal: its own console, real console
    // handles, nothing inherited; the console outlives this launch.
    #[cfg(windows)]
    if plan.new_console {
        windows_console::spawn(&plan.program, &plan.args)
            .with_context(|| format!("Cannot start system terminal {}", plan.program.display()))?;
        return Ok(());
    }
    let mut child = process(&plan, |name| std::env::var(name).ok())
        .spawn()
        .with_context(|| format!("Cannot start system terminal {}", plan.program.display()))?;
    // Drain stderr without retaining unbounded terminal diagnostics.
    use std::io::Read as _;
    let mut diagnostic = Vec::new();
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut buffer = [0u8; 4096];
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => diagnostic.extend_from_slice(&buffer[..n.min(8192 - diagnostic.len())]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let status = child.wait()?;
    anyhow::ensure!(
        status.success(),
        "System terminal {} failed ({}): {}",
        plan.program.display(),
        status,
        String::from_utf8_lossy(&diagnostic).trim()
    );
    Ok(())
}

/// macOS application «Open in terminal» uses. Okilum has no terminal-app
/// preference yet; Terminal.app is the system's own.
const MAC_TERMINAL_APP: &str = "Terminal";

/// The folder a terminal opens in for a vault item: a folder itself, or the
/// folder that contains a file.
pub(crate) fn folder_for(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.to_owned()
    } else {
        path.parent()
            .map(Path::to_owned)
            .unwrap_or_else(|| path.to_owned())
    }
}

/// The launch of «Open in terminal»: launchers come from trusted locations
/// outside the vault and the folder is an argument (#2837 review).
fn folder_launch(
    directory: &Path,
    resolve: impl Fn(&str) -> Option<PathBuf>,
) -> anyhow::Result<command::Plan> {
    let os = std::env::consts::OS;
    let terminal = std::env::var("TERMINAL").ok();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let launcher = command::folder_launcher(os, terminal.as_deref(), desktop.as_deref(), resolve)
        .map_err(anyhow::Error::msg)?;
    command::folder_plan(directory, &launcher, MAC_TERMINAL_APP).map_err(anyhow::Error::msg)
}

fn excluded(directory: &Path, vault: &Path) -> Vec<PathBuf> {
    let mut excluded = vec![vault.to_owned(), directory.to_owned()];
    excluded.extend(vault.canonicalize());
    excluded.extend(directory.canonicalize());
    excluded
}

/// «Open in terminal» (#2837): the system terminal in `path`'s folder, on a
/// dedicated thread; a launch failure is reported in the originating window.
pub(crate) fn open_folder(path: &Path, vault: &Path, window: &mut Window, cx: &mut App) {
    let directory = okilum_core::vault::plain_path(&folder_for(path));
    #[cfg(test)]
    if cx.has_global::<Launches>() {
        cx.global_mut::<Launches>().0.push(directory);
        return;
    }
    let vault = vault.to_owned();
    report(
        move || {
            let resolve = trusted(excluded(&directory, &vault));
            run(folder_launch(&directory, resolve)?)
        },
        window,
        cx,
    );
}

fn report(
    launch: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    // A terminal can remain alive for hours. Neither the UI thread nor GPUI's
    // shared background worker pool waits for that process. Reap on a dedicated
    // thread and deliver any launch/exit failure back to the originating window.
    let (send, receive) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = send.send_blocking(launch());
    });
    let window = window.window_handle();
    cx.spawn(async move |cx| {
        if let Ok(Err(error)) = receive.recv().await {
            let _ = window.update(cx, |_, window, cx| {
                window.push_notification(format!("Terminal could not open: {error:#}"), cx)
            });
        }
    })
    .detach();
}
pub(crate) fn open_project(area: PathBuf, window: &mut Window, cx: &mut App) {
    match okilum_t3::projects::Project::read(&area) {
        Ok(project) => open(&project, window, cx),
        Err(e) => window.push_notification(format!("Terminal project could not be read: {e}"), cx),
    }
}
pub(crate) fn open(project: &okilum_t3::projects::Project, window: &mut Window, cx: &mut App) {
    #[cfg(test)]
    if cx.has_global::<Launches>() {
        cx.global_mut::<Launches>().0.push(project.folder.clone());
        return;
    }
    let project = project.clone();
    report(move || launch(&project), window, cx);
}
impl Reader {
    pub(super) fn open_project_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(project) = self.project_navigation.active.clone() {
            open(&project, window, cx);
        } else {
            window.push_notification("Choose a project before opening its terminal", cx);
        }
    }
}

struct TerminalSettings {
    project: PathBuf,
    input: Entity<InputState>,
    error: Option<String>,
}
impl Render for TerminalSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().p_4().gap_3().child("Project terminal command")
            .child("Empty uses the T3 host or local checkout. Enter an executable and arguments, for example ssh maestro. Quotes group arguments; shell operators are not evaluated.")
            .child(Input::new(&self.input))
            .children(self.error.clone())
            .child(Button::new("save-terminal-command").label("Save").on_click(cx.listener(|this,_,window,cx| {
                let value=this.input.read(cx).value().to_string();
                let result=(||->anyhow::Result<()> {
                    let path=setting_path(&this.project)?;
                    if value.trim().is_empty() {if path.exists(){std::fs::remove_file(path)?;}return Ok(());}
                    command::override_argv(&value).map_err(anyhow::Error::msg)?;
                    std::fs::create_dir_all(path.parent().unwrap())?;
                    use std::io::Write;
                    let mut file=tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
                    file.write_all(&serde_json::to_vec(&value)?)?;file.as_file().sync_all()?;file.persist(path)?;Ok(())
                })();
                match result { Ok(())=>window.remove_window(),Err(e)=>{this.error=Some(e.to_string());cx.notify();} }
            })))
    }
}
pub(crate) fn settings(project: PathBuf, cx: &mut App) {
    let _ = cx.open_window(WindowOptions::default(), |window, cx| {
        window.set_window_title("Project terminal settings");
        let input = cx.new(|cx| InputState::new(window, cx));
        if let Ok(Some(value)) = saved(&project) {
            input.update(cx, |v, cx| v.set_value(value, window, cx));
        }
        let view = cx.new(|_| TerminalSettings {
            project,
            input,
            error: None,
        });
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    });
}
