//! Everything the supervisor checks before it launches anything (contract: refuse when
//! the journal, the binding, its own identity or the selected runtime is not exactly as
//! recorded). Generic over the store's platform directory, so Linux tests exercise the
//! same code the Windows and macOS binaries run.
use anyhow::{ensure, Context, Result};
use okilum_sync_controller::sidecar::{
    authority::Stored,
    store::{Selection, StateDir, Store},
    supervisor::Launch,
    Binding, Intent,
};
use std::{path::Path, time::Instant};
use uuid::Uuid;

/// What a valid start needs.
#[derive(Debug)]
pub struct Prepared {
    pub binding: Binding,
    pub selection: Selection,
    pub launch: Launch,
}
#[derive(Debug)]
pub enum Startup {
    Run(Box<Prepared>),
    /// The lifecycle intent is not Enabled: there is nothing to supervise, and the
    /// process ends successfully so launchd's `SuccessfulExit=false` does not relaunch.
    Idle,
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub fn prepare<D: StateDir>(
    store: &Store<D>,
    state: &Path,
    instance: Option<Uuid>,
    own_executable: &Path,
    deadline: Instant,
) -> Result<Startup> {
    let binding = {
        let transaction = store.begin(deadline)?;
        match transaction.state()? {
            Stored::Current(envelope) => {
                if envelope.intent() != Intent::Enabled {
                    return Ok(Startup::Idle);
                }
                envelope.binding().clone()
            }
            Stored::Absent => anyhow::bail!("no managed Sync journal for this instance"),
            Stored::Legacy { .. } => anyhow::bail!("legacy journal: migrate it before starting"),
        }
    };
    if let Some(instance) = instance {
        ensure!(
            instance == binding.instance,
            "--instance differs from the journal's instance"
        );
    }
    ensure!(
        same_file(Path::new(&binding.state_directory), state),
        "the journal names another state directory"
    );
    ensure!(
        same_file(Path::new(&binding.supervisor), own_executable),
        "the journal names another supervisor executable"
    );
    let selection = store
        .read_selection(deadline)?
        .context("no Syncthing runtime is selected")?;
    let executable = selection.verify_file(state)?;
    let config = state.join("config");
    let data = state.join("data");
    for directory in [&config, &data] {
        ensure!(
            std::fs::symlink_metadata(directory)
                .with_context(|| format!("{} is missing", directory.display()))?
                .is_dir(),
            "{} is not a directory",
            directory.display()
        );
    }
    let text = |path: &Path| -> Result<String> {
        path.to_str()
            .map(str::to_string)
            .context("state paths must be valid UTF-8")
    };
    let launch = Launch {
        executable: text(&executable)?,
        config: text(&config)?,
        data: text(&data)?,
    };
    Ok(Startup::Run(Box::new(Prepared {
        binding,
        selection,
        launch,
    })))
}
