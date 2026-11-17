//! Read-only, bounded discovery of Syncthing's default local version store.
//! Archives stay canonical to Syncthing: never prune or consume them here.
use super::{Listing, Version};
use anyhow::{ensure, Result};
use rustix::fs::{open, openat, Dir, FileType, Mode, OFlags};
use std::{
    fs::File,
    io::Read,
    path::{Component, Path},
};

const MAX_ENTRIES: usize = 10_000;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_VERSIONS: usize = 100;
fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// `note` restricts discovery to one vault-relative note. None discovers deleted
/// notes for Recover notes. Every ancestry is opened descriptor-relative.
pub fn list(root: &Path, note: Option<&Path>) -> Result<Listing> {
    if let Some(note) = note {
        ensure!(
            !note.as_os_str().is_empty()
                && note.components().all(|c| matches!(c, Component::Normal(_))),
            "Invalid history note path"
        );
    }
    let canonical = root.canonicalize()?;
    ensure!(root == canonical, "The history vault path changed");
    let root_fd = open(root, flags(), Mode::empty())?;
    let store = match openat(&root_fd, ".stversions", flags(), Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(Listing::default()),
        Err(error) => return Err(error.into()),
    };
    let mut listing = Listing::default();
    let mut entries = 0;
    let mut bytes = 0;
    let mut stamps = StampCounts::default();
    let now = super::now();
    visit(
        &store,
        Path::new(""),
        &canonical,
        note,
        &mut entries,
        &mut bytes,
        &mut listing,
        &mut stamps,
        now,
        0,
    )?;
    if entries >= MAX_ENTRIES || bytes >= MAX_BYTES || listing.versions.len() >= MAX_VERSIONS {
        listing.warnings.push("Sync history limit reached; results are incomplete. Open Note history for a specific note to narrow the search.".into());
    }
    for (count, message) in [
        (stamps.ambiguous, "the clock went back, so its local time occurred twice. It is listed at the earlier of the two times"),
        (stamps.skipped, "its local time never occurred in this computer's timezone, so it was archived under another timezone setting. Its position is approximate"),
        (stamps.future, "its local time is in the future here, so it was archived under another timezone setting. Its position is approximate"),
    ] {
        if count > 0 {
            listing.warnings.push(format!(
                "{count} Sync version{} archive time is uncertain: {message}. Details show the original archive stamp.",
                if count == 1 { "'s" } else { "s'" }
            ));
        }
    }
    listing
        .versions
        .sort_by_key(|v| std::cmp::Reverse(v.created));
    Ok(listing)
}

#[allow(clippy::too_many_arguments)]
fn visit(
    directory: &rustix::fd::OwnedFd,
    relative: &Path,
    root: &Path,
    note: Option<&Path>,
    entries: &mut usize,
    bytes: &mut usize,
    listing: &mut Listing,
    stamps: &mut StampCounts,
    now: u64,
    depth: usize,
) -> Result<()> {
    for entry in Dir::read_from(directory)? {
        if *entries >= MAX_ENTRIES || *bytes >= MAX_BYTES || listing.versions.len() >= MAX_VERSIONS
        {
            break;
        }
        *entries += 1;
        let entry = entry?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        let rel = relative.join(name);
        let kind = if entry.file_type() == FileType::Unknown {
            match rustix::fs::statat(
                directory,
                entry.file_name(),
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            ) {
                Ok(stat) => FileType::from_raw_mode(stat.st_mode),
                Err(error) => {
                    listing.warnings.push(format!(
                        "Cannot inspect Sync archive {}: {error}",
                        rel.display()
                    ));
                    continue;
                }
            }
        } else {
            entry.file_type()
        };
        if kind == FileType::Symlink {
            listing.warnings.push(format!(
                "Sync archive {} is a symbolic link; it was not followed",
                rel.display()
            ));
            continue;
        }
        if kind == FileType::Directory {
            if note.is_some_and(|n| !n.starts_with(&rel)) {
                continue;
            }
            if depth >= 64 {
                listing.warnings.push(
                    "Sync history directory depth limit reached; results are incomplete.".into(),
                );
                continue;
            }
            let result = (|| -> Result<()> {
                let child = openat(directory, entry.file_name(), flags(), Mode::empty())?;
                visit(
                    &child,
                    &rel,
                    root,
                    note,
                    entries,
                    bytes,
                    listing,
                    stamps,
                    now,
                    depth + 1,
                )
            })();
            if let Err(error) = result {
                listing.warnings.push(format!(
                    "Cannot read Sync history folder {}: {error}",
                    rel.display()
                ));
            }
            continue;
        }
        let Some((original, stamp, wall)) = original_name(name) else {
            continue;
        };
        // Pre-1970 stamps were never written by Syncthing; skip them as before.
        let Some(when) = resolve(wall, &chrono::Local) else {
            continue;
        };
        let original = relative.join(original);
        if crate::vault::service_path(&original) {
            continue;
        }
        if note.is_some_and(|n| n != original) {
            continue;
        }
        // Recover notes has no current-note selector. Live notes must not use
        // its version or byte budget and hide unrelated deletions.
        if note.is_none() {
            match std::fs::symlink_metadata(root.join(&original)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => continue,
                Err(error) => {
                    listing.warnings.push(format!(
                        "Cannot inspect original note {}: {error}",
                        original.display()
                    ));
                    continue;
                }
            }
        }
        let read = (|| -> Result<Vec<u8>> {
            let fd = openat(
                directory,
                entry.file_name(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            let file = File::from(fd);
            ensure!(file.metadata()?.is_file(), "Not a regular archive");
            let mut data = Vec::new();
            file.take((MAX_BYTES - *bytes + 1) as u64)
                .read_to_end(&mut data)?;
            if data.len() > MAX_BYTES - *bytes {
                *bytes = MAX_BYTES;
                anyhow::bail!("Sync version exceeds preview limit");
            }
            Ok(data)
        })();
        match read {
            Ok(data) => {
                *bytes += data.len();
                let (created, label) = describe(stamp, when, now, stamps);
                listing.versions.push(Version {
                    note: root.join(original),
                    text: String::from_utf8_lossy(&data).into_owned(),
                    raw: Some(data),
                    created,
                    label,
                    protected: false,
                    link_move: false,
                });
            }
            Err(error) => listing.warnings.push(format!(
                "Cannot read Sync version {}: {error}",
                rel.display()
            )),
        }
    }
    Ok(())
}

/// Split `name~YYYYMMDD-HHMMSS.md` into the original name, the stamp spelling
/// and its wall time. The stamp carries no timezone or offset (#2567).
fn original_name(name: &str) -> Option<(String, &str, chrono::NaiveDateTime)> {
    // Syncthing tags immediately before the extension (or after a bare name).
    let (stem, extension) = name
        .rsplit_once('.')
        .map_or((name, String::new()), |(s, e)| (s, format!(".{e}")));
    let (stem, stamp) = stem.rsplit_once('~')?;
    let format = time::macros::format_description!("[year][month][day]-[hour][minute][second]");
    if stamp.len() != 15 {
        return None;
    }
    let date = time::PrimitiveDateTime::parse(stamp, &format).ok()?;
    let wall = chrono::NaiveDate::from_ymd_opt(
        date.year(),
        u32::from(u8::from(date.month())),
        u32::from(date.day()),
    )?
    .and_hms_opt(
        u32::from(date.hour()),
        u32::from(date.minute()),
        u32::from(date.second()),
    )?;
    let original = format!("{stem}{extension}");
    if original
        .rsplit_once('.')
        .is_none_or(|(_, extension)| !extension.eq_ignore_ascii_case("md"))
    {
        return None;
    }
    Some((original, stamp, wall))
}

/// The instant(s) an archive stamp can denote, in Unix microseconds.
///
/// Syncthing writes `time.Now()` in its local timezone and its staggered
/// cleanup parses the same stamp with `time.Local`, so this computer's local
/// timezone is the one the stamp and the real retention both use. The managed
/// runtime receives Okilum's `TZ`; a clock change still leaves wall times that
/// map to two instants or none, and Okilum never picks one silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StampTime {
    Exact(u64),
    /// The clock went back: the wall time occurred twice, an offset change apart.
    Ambiguous {
        earlier: u64,
        later: u64,
    },
    /// The clock went forward over this wall time, so this timezone never showed
    /// it. Placed with the offset in effect a day earlier (before the gap).
    Skipped(u64),
}

fn resolve<Tz: chrono::TimeZone>(wall: chrono::NaiveDateTime, zone: &Tz) -> Option<StampTime> {
    use chrono::{MappedLocalTime, Offset, TimeZone as _};
    let micros = |t: i64| u64::try_from(t).ok();
    Some(match zone.from_local_datetime(&wall) {
        MappedLocalTime::Single(t) => StampTime::Exact(micros(t.timestamp_micros())?),
        MappedLocalTime::Ambiguous(a, b) => {
            let (a, b) = (micros(a.timestamp_micros())?, micros(b.timestamp_micros())?);
            StampTime::Ambiguous {
                earlier: a.min(b),
                later: a.max(b),
            }
        }
        MappedLocalTime::None => {
            let before = wall.checked_sub_signed(chrono::TimeDelta::days(1))?;
            let offset = zone.offset_from_local_datetime(&before).earliest()?.fix();
            StampTime::Skipped(micros(
                offset
                    .from_local_datetime(&wall)
                    .single()?
                    .timestamp_micros(),
            )?)
        }
    })
}

#[derive(Default)]
struct StampCounts {
    ambiguous: usize,
    skipped: usize,
    future: usize,
}

/// Sort key and label. The original stamp spelling is always kept as evidence;
/// uncertain times say so in the label and in one listing warning per kind.
fn describe(stamp: &str, when: StampTime, now: u64, counts: &mut StampCounts) -> (u64, String) {
    const LABEL: &str = super::SYNC_ARCHIVE_LABEL;
    // A stamp ahead of this clock cannot come from this zone's daemon. A minute
    // absorbs second truncation and a stamp written while listing.
    const FUTURE: u64 = 60 * 1_000_000;
    let utc = |micros: u64| {
        time::OffsetDateTime::from_unix_timestamp((micros / 1_000_000) as i64)
            .map(|d| format!("{} {:02}:{:02} UTC", d.date(), d.hour(), d.minute()))
            .unwrap_or_else(|_| "an unknown time".into())
    };
    match when {
        StampTime::Exact(created) if created > now.saturating_add(FUTURE) => {
            counts.future += 1;
            (
                created,
                format!(
                    "{LABEL} · archive stamp {stamp} is later than this computer's clock in its \
                     timezone; it was archived under another timezone setting"
                ),
            )
        }
        StampTime::Exact(created) => (
            created,
            format!("{LABEL} · archive stamp {stamp}, this computer's local time"),
        ),
        StampTime::Ambiguous { earlier, later } => {
            counts.ambiguous += 1;
            (
                earlier,
                format!(
                    "{LABEL} · archive stamp {stamp} is ambiguous: the clock went back, so it is \
                     either {} or {}; listed at the earlier time",
                    utc(earlier),
                    utc(later)
                ),
            )
        }
        StampTime::Skipped(created) => {
            counts.skipped += 1;
            (
                created,
                format!(
                    "{LABEL} · archive stamp {stamp} never occurred in this computer's timezone \
                     (the clock went forward); it was archived under another timezone setting \
                     and is placed at {} using the offset before the change",
                    utc(created)
                ),
            )
        }
    }
}

/// Restore a reviewed deletion by exclusive creation. An appearing local edit
/// fails publication; the retained archive remains available for another try.
pub fn restore_deleted(root: &Path, note: &Path, drafts: &Path, bytes: &[u8]) -> Result<()> {
    super::save_copy_bytes(root, note, drafts, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn issue2045_archives_are_exact_bounded_and_restore_never_clobbers() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        fs::create_dir_all(root.join(".stversions/nested"))?;
        let raw = b"\xef\xbb\xbf# old\r\n\xff\r\n";
        fs::write(root.join(".stversions/nested/note~20261102-120000.md"), raw)?;
        fs::write(
            root.join(".stversions/nested/note~20260230-120000.md"),
            "invalid",
        )?;
        fs::create_dir(root.join("nested"))?;
        let listing = list(&root, Some(Path::new("nested/note.md")))?;
        assert_eq!(listing.versions.len(), 1);
        let v = &listing.versions[0];
        assert_eq!(v.bytes(), raw);
        let state = tempfile::tempdir()?;
        restore_deleted(&root, Path::new("nested/note.md"), state.path(), v.bytes())?;
        assert_eq!(fs::read(&v.note)?, raw);
        fs::write(&v.note, "new local edit")?;
        assert!(restore_deleted(&root, Path::new("nested/note.md"), state.path(), raw).is_err());
        assert_eq!(fs::read(&v.note)?, b"new local edit");
        assert!(list(&root, Some(Path::new("../escape.md"))).is_err());
        assert!(list(&root, None)?.versions.is_empty());
        let mut editor = crate::file_editor::FileEditor::open(&v.note, state.path())?;
        editor.set_text("unsaved local draft".into())?;
        drop(editor);
        fs::remove_file(&v.note)?;
        assert!(restore_deleted(&root, Path::new("nested/note.md"), state.path(), raw).is_err());
        assert!(
            !v.note.exists(),
            "a dirty recovery journal must block Restore"
        );
        Ok(())
    }
    #[test]
    fn issue2045_symlinks_never_escape_and_scan_limits_are_visible() -> Result<()> {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let outside = tempfile::tempdir()?;
        fs::write(outside.path().join("secret~20261102-120000.md"), "secret")?;
        symlink(outside.path(), root.join(".stversions"))?;
        assert!(list(&root, None).is_err());
        fs::remove_file(root.join(".stversions"))?;
        fs::create_dir(root.join(".stversions"))?;
        symlink(outside.path(), root.join(".stversions/escape"))?;
        symlink(
            outside.path().join("secret~20261102-120000.md"),
            root.join(".stversions/note~20261102-120000.md"),
        )?;
        let l = list(&root, None)?;
        assert!(l.versions.is_empty());
        assert!(!l.warnings.is_empty());
        fs::remove_file(root.join(".stversions/note~20261102-120000.md"))?;
        for n in 0..101 {
            fs::write(
                root.join(format!(".stversions/n{n}~20261102-120000.md")),
                "retained",
            )?;
        }
        let l = list(&root, None)?;
        assert_eq!(l.versions.len(), 100);
        assert!(!l.warnings.is_empty());
        Ok(())
    }
    #[test]
    fn issue2045_live_versions_do_not_hide_deleted_notes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        fs::create_dir_all(root.join(".stversions/live"))?;
        fs::create_dir(root.join("live"))?;
        fs::write(root.join("live/note.md"), "current")?;
        for n in 0..150 {
            fs::write(
                root.join(format!(
                    ".stversions/live/note~20261102-12{:02}{:02}.md",
                    n / 60,
                    n % 60
                )),
                "old live version",
            )?;
        }
        // A separate subtree ensures the live archives are encountered as a
        // group, independently of filesystem directory ordering.
        fs::create_dir(root.join(".stversions/deleted"))?;
        fs::write(
            root.join(".stversions/deleted/note~20261102-130000.md"),
            "deleted bytes",
        )?;
        let listing = list(&root, None)?;
        assert_eq!(listing.versions.len(), 1);
        assert_eq!(listing.versions[0].note, root.join("deleted/note.md"));
        assert_eq!(listing.versions[0].bytes(), b"deleted bytes");
        assert!(listing.warnings.is_empty());
        // Specific Note history still discovers live versions and caps them.
        assert_eq!(
            list(&root, Some(Path::new("live/note.md")))?.versions.len(),
            100
        );
        Ok(())
    }
    #[test]
    fn issue2045_oversized_archive_exhausts_the_read_budget_once() -> Result<()> {
        let root = tempfile::tempdir()?;
        let root = root.path().canonicalize()?;
        fs::create_dir(root.join(".stversions"))?;
        for name in ["first", "second"] {
            fs::File::create(root.join(format!(".stversions/{name}~20261102-120000.md")))?
                .set_len((MAX_BYTES + 1) as u64)?;
        }
        let listing = list(&root, None)?;
        assert!(listing.versions.is_empty());
        assert_eq!(
            listing
                .warnings
                .iter()
                .filter(|w| w.contains("exceeds preview limit"))
                .count(),
            1,
            "the first oversized read must stop further reads"
        );
        assert!(listing.warnings.iter().any(|w| w.contains("limit reached")));
        Ok(())
    }

    const S: u64 = 1_000_000;
    fn wall(stamp: &str) -> chrono::NaiveDateTime {
        original_name(&format!("n~{stamp}.md")).unwrap().2
    }

    #[test]
    fn issue2567_stamp_policy_keeps_evidence_and_never_picks_silently() {
        // Zone rules independent of this process's TZ.
        assert_eq!(
            resolve(wall("20261102-120000"), &chrono::Utc),
            Some(StampTime::Exact(1_793_620_800 * S))
        );
        let plus2 = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        assert_eq!(
            resolve(wall("20261102-120000"), &plus2),
            Some(StampTime::Exact(1_793_613_600 * S))
        );
        assert_eq!(resolve(wall("19691231-235959"), &chrono::Utc), None);
        assert!(original_name("n~20261102-120000.txt").is_none());
        let now = 1_793_620_800 * S;
        let mut counts = StampCounts::default();
        let (created, label) = describe(
            "20261025-013000",
            StampTime::Ambiguous {
                earlier: 1_792_881_000 * S,
                later: 1_792_884_600 * S,
            },
            now,
            &mut counts,
        );
        assert_eq!(created, 1_792_881_000 * S);
        assert!(label.contains("20261025-013000") && label.contains("ambiguous"));
        assert!(label.contains("2026-10-24 22:30 UTC") && label.contains("2026-10-24 23:30 UTC"));
        let (_, label) = describe(
            "20260327-023000",
            StampTime::Skipped(1_774_571_400 * S),
            now,
            &mut counts,
        );
        assert!(label.contains("20260327-023000") && label.contains("never occurred"));
        let (_, label) = describe(
            "20261102-120200",
            StampTime::Exact(now + 120 * S),
            now,
            &mut counts,
        );
        assert!(label.contains("later than this computer's clock"));
        let (_, label) = describe("20261102-120000", StampTime::Exact(now), now, &mut counts);
        assert_eq!(label, "From another device · Sync · archive stamp 20261102-120000, this computer's local time");
        assert_eq!((counts.ambiguous, counts.skipped, counts.future), (1, 1, 1));
    }

    /// Runs the real `chrono::Local` parser and `list` in fresh processes whose
    /// TZ differs, so no test mutates this process's environment (#2567).
    #[test]
    fn issue2567_local_zone_controls_run_in_separate_processes() {
        for zone in ["UTC", "Asia/Jerusalem"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "source_history::sync::tests::issue2567_zone_child",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("TZ", zone)
                .env("OKILUM_2567_ZONE", zone)
                .output()
                .unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.status.success(), "{zone} control failed:\n{text}");
            assert!(
                text.contains(&format!("issue2567 zone control passed in {zone}")),
                "{zone} control did not run:\n{text}"
            );
        }
    }

    #[test]
    #[ignore = "child process of issue2567_local_zone_controls_run_in_separate_processes"]
    fn issue2567_zone_child() -> Result<()> {
        let Ok(zone) = std::env::var("OKILUM_2567_ZONE") else {
            return Ok(());
        };
        assert_eq!(std::env::var("TZ")?, zone);
        let local = |stamp: &str| resolve(wall(stamp), &chrono::Local).unwrap();
        use StampTime::*;
        // Independent clock: epochs from GNU date with explicit offsets
        // (`date -d '2026-10-25 01:30:00 +0300' +%s`), not from the tz database.
        let (winter, summer, ambiguous, skipped) = match zone.as_str() {
            "UTC" => (
                Exact(1_793_620_800 * S),
                Exact(1_782_907_200 * S),
                Exact(1_792_891_800 * S),
                Exact(1_774_578_600 * S),
            ),
            "Asia/Jerusalem" => (
                Exact(1_793_613_600 * S),
                Exact(1_782_896_400 * S),
                // 2026-10-25 02:00 IDT -> 01:00 IST: 01:30 occurs at +03 and +02.
                Ambiguous {
                    earlier: 1_792_881_000 * S,
                    later: 1_792_884_600 * S,
                },
                // 2026-03-27 02:00 IST -> 03:00 IDT: 02:30 never occurs.
                Skipped(1_774_571_400 * S),
            ),
            other => panic!("no expectations for {other}"),
        };
        // A missing tz database would silently fall back to UTC.
        assert_eq!(local("20260701-120000"), summer, "tz database for {zone}");
        assert_eq!(local("20261102-120000"), winter);
        assert_eq!(local("20261025-013000"), ambiguous);
        assert_eq!(local("20260327-023000"), skipped);

        // End to end: actual discovery order, sort keys and visible evidence.
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        fs::create_dir(root.join(".stversions"))?;
        let stamps = [
            "20260327-023000",
            "20261102-120000",
            "20990101-000000",
            "20260701-120000",
            "20261025-013000",
        ];
        for stamp in stamps {
            fs::write(root.join(format!(".stversions/note~{stamp}.md")), stamp)?;
        }
        let listing = list(&root, Some(Path::new("note.md")))?;
        let order: Vec<_> = listing.versions.iter().map(|v| v.text.as_str()).collect();
        assert_eq!(
            order,
            [
                "20990101-000000",
                "20261102-120000",
                "20261025-013000",
                "20260701-120000",
                "20260327-023000"
            ]
        );
        let key = |t: StampTime| match t {
            Exact(t) | Skipped(t) | Ambiguous { earlier: t, .. } => t,
        };
        let created: Vec<_> = listing.versions[1..].iter().map(|v| v.created).collect();
        assert_eq!(
            created,
            [key(winter), key(ambiguous), key(summer), key(skipped)]
        );
        for version in &listing.versions {
            assert!(version.label.contains(&version.text), "{}", version.label);
            // #2568's Restore notice must still recognise the Sync origin.
            let notice = super::super::restore_notice(&root, version)?
                .lines
                .join("\n");
            assert!(notice.contains("another computer"), "{notice}");
            assert_eq!(
                version.label.split(" · ").take(2).collect::<Vec<_>>(),
                ["From another device", "Sync"]
            );
        }
        assert!(listing.versions[0]
            .label
            .contains("later than this computer's clock"));
        let uncertain = listing
            .warnings
            .iter()
            .filter(|w| w.contains("archive time is uncertain"));
        if zone == "UTC" {
            assert_eq!(uncertain.count(), 1, "{:?}", listing.warnings);
            assert!(listing.versions[2]
                .label
                .ends_with("this computer's local time"));
        } else {
            assert_eq!(uncertain.count(), 3, "{:?}", listing.warnings);
            assert!(listing.versions[2].label.contains("ambiguous"));
            assert!(listing.versions[4].label.contains("never occurred"));
            assert!(listing
                .warnings
                .iter()
                .any(|w| w.contains("clock went back")));
        }
        diagnostic_println!("issue2567 zone control passed in {zone}");
        Ok(())
    }
}
