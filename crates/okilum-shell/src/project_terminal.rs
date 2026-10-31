//! External terminal integration. No embedded terminal or automatic command execution.
use super::*;
use gpui_component::{
    input::{Input, InputState},
    WindowExt as _,
};
mod command;
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
    use sha2::{Digest, Sha256};
    let state = reader_history::state_directory()?;
    let id = Sha256::digest(project.as_os_str().as_encoded_bytes());
    Ok(state.join("project-terminal").join(format!("{id:x}.json")))
}
fn saved(project: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read(setting_path(project)?) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn available(program: &str) -> bool {
    if Path::new(program).is_absolute() {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join(program).is_file()))
}
fn launch(project: &okilum_t3::projects::Project) -> anyhow::Result<()> {
    let document = okilum_t3::area_files::read(&project.folder, "project")?;
    let manifest = document.as_ref().map(|d| &d.value);
    let custom = saved(&project.folder)?.or_else(|| {
        manifest.and_then(|d| {
            d["execution"]["terminal_command"]
                .as_str()
                .map(str::to_owned)
        })
    });
    let state = reader_history::state_directory()?;
    let origin = match project.environment.as_deref() {
        Some(alias) => project::prefill_browser_origin(&project.folder, alias, Some(&state))?,
        None => None,
    };
    anyhow::ensure!(
        custom.is_some()
            || project.environment.is_none()
            || origin.is_some()
            || manifest
                .and_then(|d| d["execution"]["terminal_host"].as_str())
                .is_some(),
        "Connect this project's T3 server or set its terminal command first"
    );
    let origin = origin.map(|s| url::Url::parse(&s)).transpose()?;
    let host = origin
        .as_ref()
        .and_then(|u| u.host_str())
        .filter(|h| !matches!(*h, "localhost" | "127.0.0.1" | "::1"));
    let host = manifest
        .and_then(|d| d["execution"]["terminal_host"].as_str())
        .or(host);
    let checkout = project
        .appearance
        .source_checkout
        .as_deref()
        .unwrap_or_else(|| project.folder.to_str().unwrap_or("."));
    let mut initial =
        command::initial(host, checkout, custom.as_deref()).map_err(anyhow::Error::msg)?;
    if cfg!(windows) && host.is_none() && custom.is_none() {
        initial = format!(
            "Set-Location -LiteralPath '{}'",
            checkout.replace('\'', "''")
        );
    }
    let os = std::env::consts::OS;
    let launcher = if cfg!(target_os = "macos") {
        "osascript".into()
    } else if cfg!(windows) {
        if available("wt.exe") {
            "wt.exe".into()
        } else {
            "cmd.exe".into()
        }
    } else if available("xdg-terminal-exec") {
        "xdg-terminal-exec".into()
    } else if let Ok(terminal) = std::env::var("TERMINAL") {
        terminal
    } else {
        "x-terminal-emulator".into()
    };
    let plan = command::plan(os, &initial, &launcher);
    let mut child = std::process::Command::new(&plan.program)
        .args(&plan.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
pub(crate) fn open_project(area: PathBuf, window: &mut Window, cx: &mut App) {
    match okilum_t3::projects::Project::read(&area) {
        Ok(project) => open(&project, window, cx),
        Err(e) => window.push_notification(format!("Terminal project could not be read: {e}"), cx),
    }
}
pub(crate) fn open(project: &okilum_t3::projects::Project, window: &mut Window, cx: &mut App) {
    if let Err(e) = launch(project) {
        window.push_notification(format!("Terminal could not open: {e}"), cx);
    }
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
            .child("Empty uses ssh to the T3 host, or the local checkout. This command runs only when you choose Open terminal.")
            .child(Input::new(&self.input))
            .children(self.error.clone())
            .child(Button::new("save-terminal-command").label("Save").on_click(cx.listener(|this,_,window,cx| {
                let value=this.input.read(cx).value().to_string();
                let result=(||->anyhow::Result<()> {
                    let path=setting_path(&this.project)?;
                    if value.trim().is_empty() {if path.exists(){std::fs::remove_file(path)?;}return Ok(());}
                    command::initial(None,"",Some(&value)).map_err(anyhow::Error::msg)?;
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
