//! Bounded native QA control plane, absent from main/release builds. Enabled
//! only by an explicit CLI argument, never by updater environment or feed data.
//! Commands drive real Reader, Settings and protected updater paths under Xvfb.
use crate::*;
use anyhow::{ensure, Context as _, Result};
use serde_json::{json, Value};
use std::sync::OnceLock;

static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn arguments() -> Vec<std::ffi::OsString> {
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    if args
        .first()
        .is_some_and(|a| a == "--appimage-gui-acceptance")
    {
        assert!(args.len() >= 2, "Missing explicit native QA directory");
        let directory = PathBuf::from(args.remove(1));
        assert!(directory.is_absolute() && directory.is_dir());
        DIRECTORY
            .set(directory)
            .expect("Duplicate QA initialization");
        args.remove(0);
    }
    args
}

pub(crate) fn active() -> bool {
    DIRECTORY.get().is_some()
}

pub(crate) fn restart_arguments(args: &mut Vec<std::ffi::OsString>) {
    if let Some(directory) = DIRECTORY.get() {
        args.splice(
            0..0,
            [
                "--appimage-gui-acceptance".into(),
                directory.clone().into_os_string(),
            ],
        );
    }
}

fn snapshot(cx: &mut App) -> Result<Value> {
    let mut windows = Vec::new();
    for reader in reader_open::readers(cx) {
        let (handle, mut state) = {
            let reader = reader.read(cx);
            (
                reader.reader_window,
                json!({"vault": reader.vault_root, "note": reader.current_rel,
                "ready": !reader.current_rel.is_empty() && !reader.loading.as_ref().is_some_and(|l| l.active && !l.published),
                    "editor": reader.editing.as_ref().map(|e| e.input().read(cx).value().to_string())}),
            )
        };
        state["bounds"] = handle.update(cx, |_, window, _| {
            let bounds = window.bounds();
            json!([
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.size.width),
                f32::from(bounds.size.height)
            ])
        })?;
        windows.push(state);
    }
    Ok(json!({"identity": okilum_appimage::acceptance_identity(
        env!("OKILUM_BUILD_VERSION"), env!("OKILUM_SOURCE_COMMIT"))?,
        "windows": windows, "channel": updater::channel(),
        "action": updater::action_label(), "status": updater::status_line(),
        "settings_section": reader_settings::acceptance_section(cx)}))
}

fn command(request: &Value, directory: &std::path::Path, cx: &mut App) -> Result<Value> {
    match request["action"].as_str().context("Missing action")? {
        "snapshot" => {}
        "settings" => reader_settings::acceptance_show(cx),
        "about" => reader_settings::show_about(cx),
        "channel" => reader_settings::acceptance_channel(
            request["beta"].as_bool().context("Missing beta")?,
            cx,
        )?,
        "activate" => {
            // arm() links the original into recovery before GPUI quits. Capture
            // identity first, while Target's no-hardlink precondition still holds.
            let mut state = snapshot(cx)?;
            reader_settings::acceptance_activate(cx)?;
            state["status"] = json!(updater::status_line());
            return Ok(state);
        }
        "open" => {
            reader_open::open_window(
                Opts {
                    vault: Some(PathBuf::from(
                        request["vault"].as_str().context("Missing vault")?,
                    )),
                    note: Some(request["note"].as_str().context("Missing note")?.into()),
                    ..Default::default()
                },
                cx,
            )?;
        }
        "edit" => {
            let text = request["text"].as_str().context("Missing text")?;
            let readers = reader_open::readers(cx);
            let reader = readers.first().context("No Reader")?;
            let handle = reader.read(cx).reader_window;
            handle.update(cx, |_, window, cx| {
                reader.update(cx, |reader, cx| {
                    if reader.editing.is_none() {
                        reader.toggle_source(window, cx);
                    }
                    reader
                        .editing
                        .as_ref()
                        .context("Source editor did not open")?
                        .input()
                        .update(cx, |input, cx| input.set_value(text, window, cx));
                    Ok::<_, anyhow::Error>(())
                })
            })??;
        }
        "block-journal" => {
            let readers = reader_open::readers(cx);
            let reader = readers.first().context("No Reader")?.read(cx);
            let state = reader
                .session_directory
                .as_ref()
                .context("No editor state directory")?;
            let journal = std::fs::read_dir(state.join("editor-drafts"))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|e| e == "json"))
                .context("Positive control: editor has not written a real recovery journal")?;
            std::fs::remove_file(&journal)?;
            std::fs::create_dir(&journal)?;
            std::fs::write(
                directory.join("blocked-journal"),
                journal.as_os_str().as_encoded_bytes(),
            )?;
        }
        "repair-journal" => {
            let journal =
                PathBuf::from(std::fs::read_to_string(directory.join("blocked-journal"))?);
            ensure!(journal.is_dir(), "Journal failure was not armed");
            std::fs::remove_dir(journal)?;
        }
        "stage" | "restage-current-control" => {
            ensure!(
                updater::channel() == "Beta",
                "Select Beta in Settings first"
            );
            let config: Value =
                serde_json::from_slice(&std::fs::read(directory.join("fixture.json"))?)?;
            if request["action"] == "restage-current-control" {
                okilum_appimage::acceptance_restage_current(
                    std::path::Path::new(config["fixture"].as_str().context("Missing fixture")?),
                    config["fingerprint"]
                        .as_str()
                        .context("Missing fingerprint")?,
                    env!("OKILUM_BUILD_VERSION"),
                    env!("OKILUM_SOURCE_COMMIT"),
                )?;
                return snapshot(cx);
            }
            let staged = okilum_appimage::acceptance_prepare(
                std::path::Path::new(config["fixture"].as_str().context("Missing fixture")?),
                config["fingerprint"]
                    .as_str()
                    .context("Missing fingerprint")?,
                env!("OKILUM_BUILD_VERSION"),
                env!("OKILUM_SOURCE_COMMIT"),
            )?;
            updater::appimage::acceptance_stage(staged);
        }
        "quit" => cx.quit(),
        _ => anyhow::bail!("Unknown native QA action"),
    }
    snapshot(cx)
}

pub(crate) fn install(cx: &mut App) {
    let Some(directory) = DIRECTORY.get().cloned() else {
        return;
    };
    // Hard deadline: abandoned harnesses do not leave native GUI processes alive.
    cx.spawn(async move |cx| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                .await;
            cx.update(|cx| {
                if std::time::Instant::now() >= deadline {
                    cx.quit();
                    return;
                }
                let path = directory.join("command.json");
                if !path.is_file() {
                    return;
                }
                let result = (|| -> Result<()> {
                    let request: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
                    std::fs::remove_file(&path)?;
                    let id = request["id"]
                        .as_u64()
                        .context("Missing numeric command identity")?;
                    let result = match command(&request, &directory, cx) {
                        Ok(value) => json!({"ok":true,"state":value}),
                        Err(error) => json!({"ok":false,"error":format!("{error:#}")}),
                    };
                    let tmp = directory.join("response.tmp");
                    std::fs::write(&tmp, serde_json::to_vec_pretty(&result)?)?;
                    std::fs::rename(tmp, directory.join(format!("response-{id}.json")))?;
                    Ok(())
                })();
                if let Err(error) = result {
                    eprintln!("Native QA command failed: {error:#}");
                    cx.quit();
                }
            });
        }
    })
    .detach();
}
