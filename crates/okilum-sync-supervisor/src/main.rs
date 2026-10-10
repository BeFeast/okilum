#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! `okilum-sync-supervisor`: see the crate documentation. A release build on Windows is
//! a GUI-subsystem program, so a login task never flashes a console. Exit status: 0 after an
//! authorized Stop or when the lifecycle intent is not Enabled (launchd's
//! `SuccessfulExit=false` does not relaunch), 1 for a refusal or a runtime crash (the
//! OS restart policy decides what happens).
#[cfg(unix)]
mod unix_main {
    use anyhow::{Context, Result};
    #[cfg(feature = "dev-same-user")]
    use okilum_sync_controller::sidecar::supervisor::ipc::unix_transport::SameUser;
    use okilum_sync_controller::sidecar::{
        store::UnixStore, supervisor::ipc::unix_transport::PeerCheck,
    };
    use okilum_sync_supervisor::{
        args, serve,
        startup::{self, Startup},
    };
    use std::{
        process::ExitCode,
        time::{Duration, Instant},
    };

    /// The signature policy of this build. A release build carries the signing
    /// configuration (Team ID and bundle identifier, compiled in; see `policy`) and
    /// accepts only the app signed by that team. Without one it refuses to serve rather
    /// than accept an unauthenticated peer, unless built with the development feature,
    /// which accepts any peer of the same user.
    fn peer_check() -> Result<Box<dyn Fn() -> Box<dyn PeerCheck>>> {
        #[cfg(target_os = "macos")]
        {
            use okilum_sync_controller::sidecar::supervisor::ipc::macos_peer::SignedPeer;
            if let Some(release) = okilum_sync_supervisor::policy::build_policy()? {
                let own = std::env::current_exe().context("cannot resolve this executable")?;
                let app = own
                    .parent()
                    .context("the helper has no directory")?
                    .join(&release.app_executable);
                let peer = SignedPeer::new(release.app_requirement, &app)?;
                return Ok(Box::new(move || {
                    Box::new(peer.clone()) as Box<dyn PeerCheck>
                }));
            }
        }
        #[cfg(feature = "dev-same-user")]
        {
            Ok(Box::new(|| Box::new(SameUser) as Box<dyn PeerCheck>))
        }
        #[cfg(not(feature = "dev-same-user"))]
        {
            anyhow::bail!("no signature policy is configured in this build; refusing to serve")
        }
    }

    pub fn main() -> Result<ExitCode> {
        let args = args::parse(std::env::args_os().skip(1))?;
        let state = match args.state.clone() {
            Some(state) => state,
            None => default_state_directory()?,
        };
        let own = std::env::current_exe().context("cannot resolve this executable")?;
        let store = UnixStore::open_existing(&state)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        match startup::prepare(&store, &state, args.instance, &own, deadline)? {
            Startup::Idle => Ok(ExitCode::SUCCESS),
            Startup::Run(prepared) => {
                // Only a real run needs a signature policy; checked before anything starts.
                let peer_check = peer_check()?;
                let exit = serve::run(
                    &state,
                    store,
                    *prepared,
                    &serve::Settings::default(),
                    peer_check,
                )?;
                match exit {
                    serve::Exit::Stopped => Ok(ExitCode::SUCCESS),
                    other => {
                        eprintln!("okilum-sync-supervisor: {}", serve::describe(&other));
                        Ok(ExitCode::from(1))
                    }
                }
            }
        }
    }

    /// macOS: the owner's Application Support directory, found through the account
    /// database rather than the environment.
    #[cfg(target_os = "macos")]
    fn default_state_directory() -> Result<std::path::PathBuf> {
        use std::{ffi::CStr, ffi::OsStr, os::unix::ffi::OsStrExt};
        let mut buffer = vec![0u8; 4096];
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        anyhow::ensure!(
            status == 0 && !found.is_null() && !entry.pw_dir.is_null(),
            "cannot determine the home directory"
        );
        let home = unsafe { CStr::from_ptr(entry.pw_dir) };
        Ok(std::path::PathBuf::from(OsStr::from_bytes(home.to_bytes()))
            .join("Library/Application Support/Okilum/Sync"))
    }
    #[cfg(not(target_os = "macos"))]
    fn default_state_directory() -> Result<std::path::PathBuf> {
        anyhow::bail!("--state is required on this platform")
    }
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    match unix_main::main() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("okilum-sync-supervisor: {error:#}");
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
mod windows_main {
    use anyhow::{Context, Result};
    use okilum_sync_controller::sidecar::{
        store::WindowsStore, supervisor::ipc::windows_discovery::ImagePolicy,
    };
    use okilum_sync_supervisor::{
        args, serve,
        startup::{self, Startup},
    };
    use std::{
        process::ExitCode,
        sync::Arc,
        time::{Duration, Instant},
    };

    /// Same rule as on Unix: without a configured signature policy a release build
    /// refuses to serve; the development feature accepts any peer of the same user.
    fn policy() -> Result<Arc<dyn ImagePolicy + Send + Sync>> {
        #[cfg(feature = "dev-same-user")]
        {
            struct AnySameUser;
            impl ImagePolicy for AnySameUser {
                fn verify_image(&self, _: &std::path::Path) -> Result<()> {
                    Ok(())
                }
            }
            Ok(Arc::new(AnySameUser))
        }
        #[cfg(not(feature = "dev-same-user"))]
        {
            anyhow::bail!("no signature policy is configured in this build; refusing to serve")
        }
    }

    pub fn main() -> Result<ExitCode> {
        let args = args::parse(std::env::args_os().skip(1))?;
        let state = args
            .state
            .clone()
            .context("--state is required on Windows")?;
        let own = std::env::current_exe().context("cannot resolve this executable")?;
        let state_text = state
            .to_str()
            .context("the state path must be valid UTF-8")?;
        let store = WindowsStore::open_existing(state_text)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        match startup::prepare(&store, &state, args.instance, &own, deadline)? {
            Startup::Idle => Ok(ExitCode::SUCCESS),
            Startup::Run(prepared) => {
                let policy = policy()?;
                match serve::run(
                    &state,
                    store,
                    *prepared,
                    &serve::Settings::default(),
                    policy,
                )? {
                    serve::Exit::Stopped => Ok(ExitCode::SUCCESS),
                    other => {
                        eprintln!("okilum-sync-supervisor: {}", serve::describe(&other));
                        Ok(ExitCode::from(1))
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match windows_main::main() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("okilum-sync-supervisor: {error:#}");
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn main() -> std::process::ExitCode {
    eprintln!("okilum-sync-supervisor: this platform is not supported");
    std::process::ExitCode::from(2)
}
