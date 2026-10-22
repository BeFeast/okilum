use super::*;
const TRACK: Track = Track {
    platform: "macos",
    channel: "beta",
};

fn build(number: u64, source: char) -> Build {
    Build {
        build: number,
        source: source.to_string().repeat(40),
    }
}
fn fixture(source: &str) -> Result<Vec<u8>> {
    Ok(match source.as_bytes()[0] {
        b'a' => include_bytes!("fixtures/15053.json").to_vec(),
        b'b' => include_bytes!("fixtures/15032.json").to_vec(),
        b'c' => include_bytes!("fixtures/15001.json").to_vec(),
        _ => anyhow::bail!("Offline"),
    })
}

#[test]
fn first_install_is_silent_update_once_and_downgrade_keeps_high_water_mark() {
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        record_launch(directory.path(), &build(15001, 'c')).unwrap(),
        None
    );
    assert_eq!(
        record_launch(directory.path(), &build(15053, 'a')).unwrap(),
        Some(15001)
    );
    assert_eq!(
        record_launch(directory.path(), &build(15053, 'a')).unwrap(),
        None
    );
    assert_eq!(
        record_launch(directory.path(), &build(15032, 'b')).unwrap(),
        None
    );
    assert_eq!(
        record_launch(directory.path(), &build(15053, 'a')).unwrap(),
        None
    );
    assert_eq!(
        record_launch(directory.path(), &build(15054, 'd')).unwrap(),
        Some(15053)
    );
}

#[test]
fn failed_or_busy_bookkeeping_never_announces_or_overwrites_the_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("whats-new.json");
    fs::write(&path, b"corrupt").unwrap();
    assert!(record_launch(directory.path(), &build(15053, 'a')).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"corrupt");
    fs::remove_file(&path).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.path().join("whats-new.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert!(record_launch(directory.path(), &build(15053, 'a')).is_err());
    assert!(!path.exists());
    drop(lock);
    assert_eq!(
        record_launch(directory.path(), &build(15053, 'a')).unwrap(),
        None
    );
}

#[test]
fn update_range_is_cumulative_newest_first_and_about_only_fetches_current() {
    let current = build(15053, 'a');
    let range = collect(&current, Some(15001), TRACK, fixture);
    assert!(!range.incomplete);
    assert_eq!(
        range.builds.iter().map(|n| n.build).collect::<Vec<_>>(),
        [15053, 15032]
    );
    assert_eq!(range.builds[0].items[0].pr, 1482);
    assert_eq!(
        range.builds[0].items[0].link(),
        "https://git.oklabs.uk/BeFeast/okilum/pulls/1482"
    );
    assert_eq!(
        collect(&current, Some(15032), TRACK, fixture).builds.len(),
        1
    );
    assert_eq!(collect(&current, None, TRACK, fixture).builds.len(), 1);
    let earliest = collect(&build(15001, 'c'), Some(1), TRACK, fixture);
    assert!(!earliest.incomplete);
    assert!(earliest.builds[0].items.is_empty());
}

#[test]
fn offline_and_partial_failure_keep_the_current_build_and_safe_links() {
    let current = build(15053, 'a');
    let offline = collect(&current, Some(15001), TRACK, |_| anyhow::bail!("Offline"));
    assert!(offline.incomplete);
    assert!(offline.builds.is_empty());
    assert!(current.link().ends_with(&"a".repeat(40)));
    // Positive control: the exact same first fixture succeeds before the gap.
    let partial = collect(&current, Some(15001), TRACK, |sha| {
        if sha == current.source {
            fixture(sha)
        } else {
            anyhow::bail!("Offline")
        }
    });
    assert!(partial.incomplete);
    assert_eq!(partial.builds.len(), 1);
    assert_eq!(partial.builds[0].items[0].pr, 1482);
}

#[test]
fn malformed_mismatched_and_oversized_notes_are_bounded_fallbacks() {
    let current = build(15053, 'a');
    for invalid in [
        b"not json".to_vec(),
        fixture(&"b".repeat(40)).unwrap(),
        vec![b' '; MAX_BYTES as usize + 1],
    ] {
        let range = collect(&current, None, TRACK, |_| Ok(invalid.clone()));
        assert!(range.incomplete);
        assert!(range.builds.is_empty());
    }
    let mut cycle: serde_json::Value =
        serde_json::from_slice(&fixture(&current.source).unwrap()).unwrap();
    cycle["previous_build"] = 15054.into();
    cycle["previous_source"] = "../not-a-source".into();
    let mut calls = 0;
    let range = collect(&current, Some(1), TRACK, |_| {
        calls += 1;
        Ok(serde_json::to_vec(&cycle).unwrap())
    });
    assert_eq!(calls, 1);
    assert!(range.incomplete);
    assert!(read_bounded(&vec![0; 4097][..], 4096).is_err());
}

#[test]
fn notes_from_another_platform_or_channel_are_not_presented() {
    let current = build(15053, 'a');
    for track in [
        Track {
            platform: "windows",
            channel: "beta",
        },
        Track {
            platform: "macos",
            channel: "stable",
        },
    ] {
        let range = collect(&current, None, track, fixture);
        assert!(range.incomplete);
        assert!(range.builds.is_empty());
    }
}
