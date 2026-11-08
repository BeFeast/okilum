//! Read-only launchd liveness, never lifecycle or publication authority.
//!
//! An in-place app update can unlink the running helper's original image. Its
//! code-path inspection can then fail while launchd and its child remain live.
//! Health must not confuse that lifecycle-readiness failure with a stopped job.
use anyhow::{ensure, Result};

const LABEL: &str = "com.befeast.okilum.sync";

/// Parse only top-level launchctl list fields, excluding nested arguments and
/// dictionaries. LastExitStatus describes the previous run, not the current PID.
fn listed_pid(text: &str) -> Result<Option<u32>> {
    let mut label = None;
    let mut pid = None;
    for line in text
        .lines()
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t\t"))
    {
        let Some((key, value)) = line.trim().split_once(" = ") else {
            continue;
        };
        match key {
            "\"Label\"" => {
                ensure!(label.is_none(), "duplicate launchd label");
                label = Some(value.trim_end_matches(';').trim_matches('"'));
            }
            "\"PID\"" => {
                ensure!(pid.is_none(), "duplicate launchd PID");
                let value = value.trim_end_matches(';').parse::<u32>()?;
                ensure!(value > 0 && value <= i32::MAX as u32, "invalid launchd PID");
                pid = Some(value);
            }
            _ => {}
        }
    }
    ensure!(label == Some(LABEL), "unexpected launchd service");
    Ok(pid)
}

#[cfg(target_os = "macos")]
fn process_live(pid: u32) -> Result<bool> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    ensure!(read == size, "launchd process observation unavailable");
    ensure!(
        info.pbi_pid == pid && info.pbi_uid == unsafe { libc::geteuid() },
        "launchd process identity changed"
    );
    // Darwin SZOMB=5; a zombie no longer supervises a child. Image paths are
    // deliberately unnecessary here: Sparkle may have removed the old bundle.
    Ok(info.pbi_status != 5)
}

#[cfg(target_os = "macos")]
pub fn account_helper_running() -> Result<bool> {
    use super::inventory::output_status;
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    let read = || -> Result<Option<u32>> {
        let (status, bytes) = output_status(
            Command::new("/bin/launchctl").args(["list", LABEL]),
            deadline,
            65536,
        )?;
        if status.code() == Some(113) {
            return Ok(None);
        }
        ensure!(status.success(), "launchd observation failed: {status}");
        listed_pid(std::str::from_utf8(&bytes)?)
    };
    let Some(pid) = read()? else { return Ok(false) };
    let live = process_live(pid)?;
    ensure!(
        read()? == Some(pid),
        "launchd generation changed during observation"
    );
    Ok(live)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_launchd_pid_is_independent_of_old_exit_status_and_nested_fields() -> Result<()> {
        let running = "{\n\t\"Label\" = \"com.befeast.okilum.sync\";\n\t\"LastExitStatus\" = 256;\n\t\"PID\" = 42;\n\t\"Nested\" = {\n\t\t\"PID\" = 999;\n\t};\n};\n";
        assert_eq!(listed_pid(running)?, Some(42));
        assert_eq!(listed_pid(&running.replace("\t\"PID\" = 42;\n", ""))?, None);
        for invalid in [
            running.replace(LABEL, "org.example.other"),
            running.replace("42;", "0;"),
            running.replace("42;", "-1;"),
            running.replace("42;", "42;\n\t\"PID\" = 43;"),
            String::new(),
        ] {
            assert!(listed_pid(&invalid).is_err());
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn running_unlinked_image_remains_live_and_exit_is_observed() -> Result<()> {
        use std::{fs, process::Command};
        let root = tempfile::tempdir()?;
        let executable = root.path().join("sleep");
        fs::copy("/bin/sleep", &executable)?;
        let mut child = Command::new(&executable).arg("60").spawn()?;
        let observed = (|| -> Result<()> {
            assert!(process_live(child.id())?);
            fs::remove_file(&executable)?;
            assert!(!executable.exists());
            assert!(
                process_live(child.id())?,
                "an unlinked live helper is not down"
            );
            Ok(())
        })();
        let _ = child.kill();
        let _ = child.wait();
        observed?;
        assert!(process_live(child.id()).is_err());
        Ok(())
    }
}
