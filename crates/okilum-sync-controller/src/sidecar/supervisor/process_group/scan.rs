//! Live same-user processes running a given executable, for the "left the group"
//! check. Read-only. An unreadable process is skipped (it is not ours to see); a
//! failure of the scan itself is an error, never "none found".
use anyhow::Result;
use std::path::Path;

/// Platform start time of `pid`, comparable only with other values from here.
pub fn started(pid: u32) -> Result<u64> {
    imp::started(pid)
}
/// Pids of live processes of `uid` whose image is exactly `image` and whose start
/// time is not earlier than `since`.
pub fn live_copies(image: &Path, uid: u32, since: u64) -> Result<Vec<u32>> {
    imp::live_copies(image, uid, since)
}

/// Start time and process group from the same Darwin kernel snapshot, including
/// an exited leader retained by waitid(NOWAIT).
#[cfg(target_os = "macos")]
pub(super) fn leader(pid: u32) -> Result<(u64, i32)> {
    imp::leader(pid)
}

#[cfg(target_os = "linux")]
mod imp {
    use anyhow::{ensure, Context, Result};
    use rustix::fs::{openat, readlinkat, Mode, OFlags};
    use std::{
        ffi::OsStr,
        fs::File,
        io::Read,
        os::unix::{ffi::OsStrExt, fs::MetadataExt},
        path::Path,
    };

    #[derive(Debug, PartialEq, Eq)]
    struct Identity {
        pid: u32,
        parent: u32,
        start: u64,
    }

    fn parse_stat(stat: &[u8], expected_pid: u32) -> Result<Identity> {
        // comm is arbitrary bytes and may contain spaces, newlines and ')'.
        // Only the final ')' ends it; the remaining fields are ASCII numbers.
        let open = stat
            .iter()
            .position(|&b| b == b'(')
            .context("missing comm")?;
        let close = stat
            .iter()
            .rposition(|&b| b == b')')
            .context("missing comm end")?;
        ensure!(
            open > 0 && close > open && stat[open - 1] == b' ',
            "malformed stat"
        );
        let number = |bytes: &[u8]| -> Result<u64> {
            ensure!(
                !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit),
                "bad stat number"
            );
            Ok(std::str::from_utf8(bytes)?.parse()?)
        };
        let pid = number(&stat[..open - 1])?;
        ensure!(
            expected_pid > 0 && expected_pid <= i32::MAX as u32 && pid == u64::from(expected_pid),
            "stat PID mismatch"
        );
        ensure!(stat.get(close + 1) == Some(&b' '), "missing stat separator");
        let fields: Vec<_> = stat[close + 2..]
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty())
            .collect();
        ensure!(fields.len() >= 20, "short stat");
        ensure!(
            matches!(
                fields[0],
                b"R" | b"S" | b"D" | b"Z" | b"T" | b"t" | b"X" | b"x" | b"K" | b"W" | b"P" | b"I"
            ),
            "bad process state"
        );
        // Validate every field through starttime, including signed kernel fields.
        for (index, field) in fields.iter().enumerate().take(20).skip(1) {
            if matches!(index, 4 | 5 | 13..=16 | 18) {
                let digits = field.strip_prefix(b"-").unwrap_or(field);
                number(digits)?;
                std::str::from_utf8(field)?.parse::<i64>()?;
            } else {
                number(field)?;
            }
        }
        let parent = u32::try_from(number(fields[1])?)?;
        ensure!(
            parent <= i32::MAX as u32 && parent != expected_pid,
            "invalid parent PID"
        );
        Ok(Identity {
            pid: expected_pid,
            parent,
            start: number(fields[19])?,
        })
    }

    fn stat_bytes(directory: &File) -> std::io::Result<Vec<u8>> {
        let mut file = File::from(openat(
            directory,
            "stat",
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn matches_copy(
        before: &Identity,
        after: &Identity,
        observed_uid: u32,
        observed_image: &Path,
        image: &Path,
        uid: u32,
        since: u64,
    ) -> Result<bool> {
        // Reparenting is possible without PID reuse, but either change makes this
        // observation incomplete. Do not turn an ambiguous scan into Stopped.
        ensure!(before == after, "process identity changed during scan");
        Ok(observed_uid == uid && observed_image == image && before.start >= since)
    }

    pub fn started(pid: u32) -> Result<u64> {
        let directory = File::open(format!("/proc/{pid}"))?;
        Ok(parse_stat(&stat_bytes(&directory)?, pid)?.start)
    }
    pub fn live_copies(image: &Path, uid: u32, since: u64) -> Result<Vec<u32>> {
        let mut found = vec![];
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            // A proc directory handle stays attached to that process generation;
            // relative reads cannot follow a newly reused numeric PID path.
            let Ok(directory) = File::open(entry.path()) else {
                continue;
            };
            let Ok(meta) = directory.metadata() else {
                continue;
            };
            if meta.uid() != uid {
                continue;
            }
            let Ok(bytes) = stat_bytes(&directory) else {
                continue;
            };
            let Ok(exe) = readlinkat(&directory, "exe", Vec::new()) else {
                continue;
            };
            let exe = Path::new(OsStr::from_bytes(exe.to_bytes()));
            if exe != image {
                continue;
            }
            let before = parse_stat(&bytes, pid)?;
            // Gone between the listing and the read: no longer live.
            let Ok(bytes) = stat_bytes(&directory) else {
                continue;
            };
            let after = parse_stat(&bytes, pid)?;
            let Ok(current_meta) = directory.metadata() else {
                continue;
            };
            ensure!(
                meta.uid() == current_meta.uid(),
                "process owner changed during scan"
            );
            if matches_copy(&before, &after, meta.uid(), exe, image, uid, since)? {
                found.push(pid);
            }
        }
        Ok(found)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn stat(pid: u32, parent: u32, start: u64) -> Vec<u8> {
            let mut fields = vec!["0".to_owned(); 20];
            fields[0] = "S".into();
            fields[1] = parent.to_string();
            fields[17] = "1".into();
            fields[19] = start.to_string();
            format!("{pid} (runtime (worker)) {}\n", fields.join(" ")).into_bytes()
        }

        fn observed(pid: u32, parent: u32, start: u64) -> Identity {
            parse_stat(&stat(pid, parent, start), pid).unwrap()
        }

        #[test]
        fn valid_stat_and_exact_owner_are_positive_controls() {
            let identity = observed(42, 7, 100);
            assert_eq!(
                identity,
                Identity {
                    pid: 42,
                    parent: 7,
                    start: 100
                }
            );
            assert!(matches_copy(
                &identity,
                &identity,
                1000,
                Path::new("/runtime/syncthing"),
                Path::new("/runtime/syncthing"),
                1000,
                100,
            )
            .unwrap());

            // comm is not a UTF-8 string and may itself look like stat fields.
            let mut bytes = b"42 (worker\xff )\n42 (name)".to_vec();
            let fixture = stat(42, 7, 100);
            let close = fixture.iter().rposition(|&b| b == b')').unwrap();
            bytes.extend_from_slice(&fixture[close..]);
            assert_eq!(parse_stat(&bytes, 42).unwrap(), identity);
        }

        #[test]
        fn malformed_stat_never_becomes_an_identity() {
            let valid = String::from_utf8(stat(42, 7, 100)).unwrap();
            for bad in [
                valid.replacen("42 (", "43 (", 1),
                valid.replacen("42 (", "+42 (", 1),
                valid.replacen("42 (", "42(", 1),
                valid.replacen(") S", ")S", 1),
                valid.replacen(") S", ") unknown", 1),
                valid.replacen("S 7", "S -7", 1),
                valid.replacen("S 7", "S 42", 1),
                valid.replacen("S 7", "S 4294967296", 1),
                valid.replacen("S 7 0", "S 7 bad", 1),
                valid.replacen("100\n", "+100\n", 1),
                valid.replacen("100\n", "-100\n", 1),
                valid.replacen("100\n", "18446744073709551616\n", 1),
                valid.replacen("100\n", "", 1),
                "42 no-comm S 7".into(),
                "42 (unterminated S 7".into(),
            ] {
                assert!(parse_stat(bad.as_bytes(), 42).is_err(), "accepted {bad:?}");
            }
            assert!(parse_stat(&stat(0, 7, 100), 0).is_err());
            assert!(parse_stat(&stat(u32::MAX, 7, 100), u32::MAX).is_err());
        }

        #[test]
        fn reused_pid_or_changed_parent_leaves_scan_incomplete() {
            let before = observed(42, 7, 100);
            for after in [
                observed(42, 7, 101),
                observed(42, 8, 100),
                observed(43, 7, 100),
            ] {
                assert!(
                    matches_copy(
                        &before,
                        &after,
                        1000,
                        Path::new("/runtime/syncthing"),
                        Path::new("/runtime/syncthing"),
                        1000,
                        100,
                    )
                    .is_err(),
                    "accepted changed identity {after:?}"
                );
            }
        }

        #[test]
        fn similar_foreign_owners_and_older_generations_do_not_match() {
            let identity = observed(42, 7, 100);
            for (owner, image, since) in [
                (100, "/runtime/syncthing", 100),
                (1001, "/runtime/syncthing", 100),
                (1000, "/runtime/syncthing-helper", 100),
                (1000, "/foreign/runtime/syncthing", 100),
                (1000, "/runtime/Syncthing", 100),
                (1000, "/runtime/syncthing (deleted)", 100),
                (1000, "/runtime/syncthing", 101),
            ] {
                assert!(
                    !matches_copy(
                        &identity,
                        &identity,
                        owner,
                        Path::new(image),
                        Path::new("/runtime/syncthing"),
                        1000,
                        since,
                    )
                    .unwrap(),
                    "matched foreign copy {owner} {image} {since}"
                );
            }
        }

        #[test]
        fn pinned_inventory_directory_does_not_follow_a_reused_pid_path() {
            let root = tempfile::tempdir().unwrap();
            let pid_path = root.path().join("42");
            std::fs::create_dir(&pid_path).unwrap();
            std::fs::write(pid_path.join("stat"), stat(42, 7, 100)).unwrap();
            let directory = File::open(&pid_path).unwrap();
            // A local fixture only: no /proc access or real processes.
            std::fs::rename(&pid_path, root.path().join("old-generation")).unwrap();
            std::fs::create_dir(&pid_path).unwrap();
            std::fs::write(pid_path.join("stat"), stat(42, 8, 101)).unwrap();
            assert_eq!(
                parse_stat(&stat_bytes(&directory).unwrap(), 42).unwrap(),
                observed(42, 7, 100)
            );
            let replacement = File::open(&pid_path).unwrap();
            assert_eq!(
                parse_stat(&stat_bytes(&replacement).unwrap(), 42).unwrap(),
                observed(42, 8, 101)
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use anyhow::{ensure, Result};
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};

    fn info(pid: u32, include_zombies: bool) -> Option<libc::proc_bsdinfo> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let read = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                // XNU proc_pidinfo explicitly includes unreaped zombies when arg != 0.
                u64::from(include_zombies),
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        (read == size).then_some(info)
    }
    fn start_of(info: &libc::proc_bsdinfo) -> u64 {
        info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec
    }
    pub fn started(pid: u32) -> Result<u64> {
        Ok(leader(pid)?.0)
    }
    pub fn leader(pid: u32) -> Result<(u64, i32)> {
        // An unreaped leader still reserves its PID/group after exit. Ordinary
        // proc_pidinfo and getpgid omit zombies on Darwin, so read both identity
        // fields in one zombie-inclusive kernel snapshot; never substitute a
        // cached generation when inspection fails.
        let info = info(pid, true).ok_or_else(|| anyhow::anyhow!("process {pid} not readable"))?;
        ensure!(
            info.pbi_pid == pid,
            "process identity changed during inspection"
        );
        Ok((start_of(&info), i32::try_from(info.pbi_pgid)?))
    }
    pub fn live_copies(image: &Path, uid: u32, since: u64) -> Result<Vec<u32>> {
        // The process table can grow between sizing and filling; a full buffer may
        // have been truncated, so grow and retry until the result leaves room. An
        // undercount would let an escaped copy go unseen, so give up loudly.
        let mut slots = {
            let size = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
            ensure!(size > 0, "cannot list processes");
            size as usize * 2
        };
        let (pids, count) = loop {
            let mut pids = vec![0 as libc::pid_t; slots];
            let bytes = (slots * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
            let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
            ensure!(count > 0, "cannot list processes");
            if (count as usize) < slots {
                break (pids, count);
            }
            ensure!(slots < (1 << 22), "process table too large to scan");
            slots *= 2;
        };
        let mut found = vec![];
        for &pid in &pids[..count as usize] {
            if pid <= 0 {
                continue;
            }
            let Some(info) = info(pid as u32, false) else {
                continue;
            };
            if info.pbi_uid != uid || start_of(&info) < since {
                continue;
            }
            let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
            let length =
                unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
            if length > 0 && Path::new(OsStr::from_bytes(&path[..length as usize])) == image {
                found.push(pid as u32);
            }
        }
        Ok(found)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use anyhow::Result;
    use std::path::Path;
    pub fn started(_: u32) -> Result<u64> {
        anyhow::bail!("process scan is not implemented for this platform")
    }
    pub fn live_copies(_: &Path, _: u32, _: u64) -> Result<Vec<u32>> {
        anyhow::bail!("process scan is not implemented for this platform")
    }
}
