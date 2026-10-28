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
fn fixture(source: &str) -> Result<Option<Vec<u8>>> {
    Ok(Some(match source.as_bytes()[0] {
        b'a' => include_bytes!("fixtures/15053.json").to_vec(),
        b'b' => include_bytes!("fixtures/15032.json").to_vec(),
        b'c' => include_bytes!("fixtures/15001.json").to_vec(),
        _ => anyhow::bail!("Offline"),
    }))
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
    assert!(range.unavailable.is_none());
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
    assert!(earliest.unavailable.is_none());
    assert!(earliest.builds[0].items.is_empty());
}

#[test]
fn offline_and_partial_failure_keep_the_current_build_and_safe_links() {
    let current = build(15053, 'a');
    let offline = collect(&current, Some(15001), TRACK, |_| anyhow::bail!("Offline"));
    assert_eq!(offline.unavailable, Some(Unavailable::FetchFailed));
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
    assert_eq!(partial.unavailable, Some(Unavailable::FetchFailed));
    assert_eq!(partial.builds.len(), 1);
    assert_eq!(partial.builds[0].items[0].pr, 1482);
}

#[test]
fn malformed_mismatched_and_oversized_notes_are_bounded_fallbacks() {
    let current = build(15053, 'a');
    for invalid in [
        b"not json".to_vec(),
        fixture(&"b".repeat(40)).unwrap().unwrap(),
        vec![b' '; MAX_BYTES as usize + 1],
    ] {
        let range = collect(&current, None, TRACK, |_| Ok(Some(invalid.clone())));
        assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
        assert!(range.builds.is_empty());
    }
    let mut cycle: serde_json::Value =
        serde_json::from_slice(&fixture(&current.source).unwrap().unwrap()).unwrap();
    cycle["previous_build"] = 15054.into();
    cycle["previous_source"] = "../not-a-source".into();
    let mut calls = 0;
    let range = collect(&current, Some(1), TRACK, |_| {
        calls += 1;
        Ok(Some(serde_json::to_vec(&cycle).unwrap()))
    });
    assert_eq!(calls, 1);
    assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
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
        assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
        assert!(range.builds.is_empty());
    }
}

#[test]
fn half_a_predecessor_pair_keeps_current_notes_but_marks_history_incomplete() {
    let current = build(15053, 'a');
    for missing in ["previous_source", "previous_build"] {
        let mut notes: serde_json::Value =
            serde_json::from_slice(&fixture(&current.source).unwrap().unwrap()).unwrap();
        notes[missing] = serde_json::Value::Null;
        let range = collect(&current, Some(15001), TRACK, |_| {
            Ok(Some(serde_json::to_vec(&notes).unwrap()))
        });
        assert_eq!(range.unavailable, Some(Unavailable::FetchFailed));
        assert_eq!(range.builds.len(), 1);
        assert_eq!(range.builds[0].items[0].pr, 1482);
    }
    // Positive end-of-history control: both absent is a legitimate first release.
    let earliest = collect(&build(15001, 'c'), Some(1), TRACK, fixture);
    assert!(earliest.unavailable.is_none());
    assert_eq!(earliest.builds.len(), 1);
}

#[test]
fn unpublished_current_or_predecessor_keeps_its_distinct_status_and_available_items() {
    let current = build(15053, 'a');
    let unpublished = collect(&current, None, TRACK, |_| Ok(None));
    assert_eq!(unpublished.unavailable, Some(Unavailable::NotPublished));
    assert!(unpublished.builds.is_empty());
    let partial = collect(&current, Some(15001), TRACK, |sha| {
        if sha == current.source {
            fixture(sha)
        } else {
            Ok(None)
        }
    });
    assert_eq!(partial.unavailable, Some(Unavailable::NotPublished));
    assert_eq!(partial.builds.len(), 1);
    assert_eq!(partial.builds[0].items[0].pr, 1482);
    // Same chain, fully published: the fallback disappears.
    assert!(collect(&current, Some(15001), TRACK, fixture)
        .unavailable
        .is_none());
}

#[test]
fn http_404_is_unpublished_but_server_errors_stay_retryable() {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/notes.json", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for status in [200, 404, 503] {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 2048];
            assert!(socket.read(&mut request).unwrap() > 0);
            write!(
                socket,
                "HTTP/1.1 {status} Status\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
        }
    });
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    // Positive transport control: this mock really serves a readable body.
    assert_eq!(
        fetch_notes(&client, &url, Duration::from_secs(3)).unwrap(),
        Some(b"{}".to_vec())
    );
    assert_eq!(
        fetch_notes(&client, &url, Duration::from_secs(3)).unwrap(),
        None
    );
    assert!(fetch_notes(&client, &url, Duration::from_secs(3)).is_err());
    server.join().unwrap();
}

#[test]
fn appimage_chain_uses_embedded_builds_and_rejects_publication_scale() {
    let track = Track {
        platform: "linux-appimage",
        channel: "beta",
    };
    let current = build(6955, 'a');
    let fetch = |source: &str| {
        let (number, previous_build, previous_source) = if source == "a".repeat(40) {
            (6955, Some(6950), Some("b".repeat(40)))
        } else {
            (6950, Some(6931), Some("c".repeat(40)))
        };
        Ok(Some(
            serde_json::to_vec(&serde_json::json!({
                "platform": "linux-appimage", "channel": "beta", "build": number,
                "source": source, "previous_build": previous_build,
                "previous_source": previous_source, "items": []
            }))
            .unwrap(),
        ))
    };
    let range = collect(&current, Some(6931), track, fetch);
    assert!(range.unavailable.is_none());
    assert_eq!(
        range.builds.iter().map(|n| n.build).collect::<Vec<_>>(),
        [6955, 6950]
    );
    let wrong_scale = collect(&current, None, track, |source| {
        let mut value: serde_json::Value = serde_json::from_slice(&fetch(source)?.unwrap())?;
        value["build"] = serde_json::json!(18581);
        Ok(Some(serde_json::to_vec(&value)?))
    });
    assert_eq!(wrong_scale.unavailable, Some(Unavailable::FetchFailed));
    assert!(wrong_scale.builds.is_empty());
}
