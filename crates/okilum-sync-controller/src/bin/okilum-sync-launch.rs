//! Closed Linux service entry; never starts an external home or accepts a raw
//! Syncthing executable/config path. Every restart consumes durable ownership.
#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    fn run() -> anyhow::Result<()> {
        use anyhow::{ensure, Context};
        let mut state = None;
        let mut instance = None;
        for arg in std::env::args_os().skip(1) {
            let text = arg.to_str().context("invalid managed launch argument")?;
            if let Some(value) = text.strip_prefix("--state=") {
                ensure!(state.is_none(), "duplicate state");
                state = Some(std::path::PathBuf::from(value));
            } else if let Some(value) = text.strip_prefix("--instance=") {
                ensure!(instance.is_none(), "duplicate instance");
                instance = Some(value.parse::<uuid::Uuid>()?);
            } else {
                anyhow::bail!("unexpected managed launch argument");
            }
        }
        okilum_sync_controller::lifecycle::run_managed(
            &state.context("state required")?,
            instance.context("instance required")?,
        )
    }
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("okilum-sync-launch: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux managed launch is unavailable on this platform");
    std::process::exit(1);
}
