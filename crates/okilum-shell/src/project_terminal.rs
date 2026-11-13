//! External terminal integration. No embedded terminal or automatic command execution.
use super::*;
use gpui_component::{
    input::{Input, InputState},
    WindowExt as _,
};
mod command;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[derive(Default)]
struct Launches(Vec<PathBuf>);
#[cfg(test)]
impl gpui::Global for Launches {}
gpui::actions!(project_terminal, [OpenTerminal]);

pub(crate) fn bind_keys(cx: &mut App) {
    // Input/editor-specific bindings retain their meaning, including Ctrl+J.
    let context = Some("Reader && !Input && !Editor && !SourceEditor && !Textarea");
    cx.bind_keys([
        KeyBinding::new("secondary-j", OpenTerminal, context),
        KeyBinding::new("ctrl-`", OpenTerminal, context),
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
fn available(program: &str) -> bool {
    fn executable(path: &Path) -> bool {
        let Ok(metadata) = path.metadata() else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
    if Path::new(program).is_absolute() {
        return executable(Path::new(program));
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| executable(&p.join(program))))
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
    use anyhow::Context as _;
    let initial = initial_for_project(project, &reader_history::state_directory()?)?;
    let os = std::env::consts::OS;
    let terminal = std::env::var("TERMINAL").ok();
    let launcher =
        command::launcher(os, terminal.as_deref(), available).map_err(anyhow::Error::msg)?;
    let plan = command::plan(os, &initial, &launcher);
    let mut child = std::process::Command::new(&plan.program)
        .args(&plan.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("Cannot start system terminal {}", plan.program))?;
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
        plan.program,
        status,
        String::from_utf8_lossy(&diagnostic).trim()
    );
    Ok(())
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
    // A terminal can remain alive for hours. Neither the UI thread nor GPUI's
    // shared background worker pool waits for that process. Reap on a dedicated
    // thread and deliver any launch/exit failure back to the originating window.
    let project = project.clone();
    let (send, receive) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = send.send_blocking(launch(&project));
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
