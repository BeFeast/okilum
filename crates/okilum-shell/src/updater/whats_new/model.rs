//! App-owned high-water mark and the publisher's per-source release-note chain.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};

pub(super) const RELEASES: &str = "https://git.oklabs.uk/BeFeast/okilum/releases";
const NOTES_ROOT: &str = "https://updates.befeast.com/okilum/releases";
const MAX_BYTES: u64 = 256 * 1024;
const MAX_BUILDS: usize = 100;
const MAX_TOTAL_BYTES: usize = 2 * 1024 * 1024;
/// One attempt's wall-clock budget; `load` stays bounded at ATTEMPTS × BUDGET.
const BUDGET: Duration = Duration::from_secs(10);
/// The first request of each attempt can be slow right after an update or
/// restart (cold DNS and TLS, #2451), so it gets a cap far above a normal
/// ~0.1 s notes fetch instead of the ordinary per-request one.
const FIRST_CONNECT: Duration = Duration::from_secs(8);
const REQUEST: Duration = Duration::from_secs(3);
/// The initial attempt plus one automatic retry (#2451).
const ATTEMPTS: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Build {
    pub build: u64,
    pub source: String,
}
impl Build {
    pub fn current() -> Option<Self> {
        let build = env!("OKILUM_BUILD_VERSION").parse().ok()?;
        let source = env!("OKILUM_SOURCE_COMMIT").to_owned();
        valid_source(&source).then_some(Self { build, source })
    }
    pub fn link(&self) -> String {
        format!(
            "https://git.oklabs.uk/BeFeast/okilum/commit/{}",
            self.source
        )
    }
}
fn valid_source(source: &str) -> bool {
    source.len() == 40 && source.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Compare and persist while holding an OS file lock. Keep the high-water mark
/// across downgrades so returning to an already-seen build stays silent.
/// Missing state is a first install. Invalid/unreadable state stays untouched.
pub(super) fn record_launch(directory: &Path, current: &Build) -> Result<Option<u64>> {
    fs::create_dir_all(directory)?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("whats-new.lock"))?;
    lock.try_lock().context("What's new state is busy")?;
    let path = directory.join("whats-new.json");
    let previous: Option<Build> = match fs::File::open(&path) {
        Ok(file) => Some(serde_json::from_slice(&read_bounded(file, 4096)?)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if previous
        .as_ref()
        .is_some_and(|seen| seen.build >= current.build)
    {
        return Ok(None);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&serde_json::to_vec(current)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path)?;
    Ok(previous.map(|seen| seen.build))
}

#[derive(Clone, Copy)]
pub(super) struct Track {
    pub platform: &'static str,
    pub channel: &'static str,
}
impl Track {
    fn current() -> Option<Self> {
        let platform = if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(windows) {
            "windows"
        } else if std::env::var_os("APPIMAGE").is_some() {
            "linux-appimage"
        } else {
            "linux"
        };
        let channel = match crate::updater::channel() {
            "Beta" => "beta",
            "Stable" => "stable",
            _ => return None,
        };
        Some(Self { platform, channel })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Kind {
    Feature,
    Fix,
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct Item {
    pub pr: u64,
    pub kind: Kind,
    pub text: String,
}
impl Item {
    pub fn link(&self) -> String {
        format!("https://git.oklabs.uk/BeFeast/okilum/pulls/{}", self.pr)
    }
}
/// Exact Slice 1 wire format. Optional previous fields allow the first release.
#[derive(Clone, Debug, Deserialize)]
pub(super) struct Notes {
    pub platform: String,
    pub channel: String,
    pub build: u64,
    pub source: String,
    pub previous_build: Option<u64>,
    pub previous_source: Option<String>,
    pub items: Vec<Item>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unavailable {
    NotPublished,
    FetchFailed,
    /// The build's channel has no published notes (Track::current() is None).
    /// Deterministic: retrying cannot change the answer (#2451).
    UnknownChannel,
    /// The HTTP client could not be created. Deterministic like UnknownChannel.
    NoClient,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Range {
    pub builds: Vec<Notes>,
    pub unavailable: Option<Unavailable>,
}

fn read_bounded(reader: impl Read, maximum: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(maximum + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= maximum,
        "Release notes exceeded size limit"
    );
    Ok(bytes)
}

/// Follow previous_source only at the fixed public origin, newest first.
/// Failure keeps already-fetched builds. `Ok(None)` means HTTP 404 (not
/// published); transport, HTTP and validation failures remain retryable.
/// `since=None` requests this build only (About); Some requests the update gap.
pub(super) fn collect(
    current: &Build,
    since: Option<u64>,
    track: Track,
    mut fetch: impl FnMut(&str) -> Result<Option<Vec<u8>>>,
) -> Range {
    let mut result = Range::default();
    let mut next = current.clone();
    let mut visited = BTreeSet::new();
    let mut total = 0;
    for _ in 0..MAX_BUILDS {
        if !valid_source(&next.source) || !visited.insert(next.source.clone()) {
            break;
        }
        let bytes = match fetch(&next.source) {
            Ok(Some(bytes)) => bytes,
            outcome => {
                result.unavailable = Some(if matches!(outcome, Ok(None)) {
                    Unavailable::NotPublished
                } else {
                    Unavailable::FetchFailed
                });
                return result;
            }
        };
        let parsed = (|| -> Result<Notes> {
            total += bytes.len();
            ensure!(
                bytes.len() <= MAX_BYTES as usize && total <= MAX_TOTAL_BYTES,
                "Notes too large"
            );
            let notes: Notes = serde_json::from_slice(&bytes)?;
            ensure!(
                notes.build == next.build
                    && notes.source == next.source
                    && notes.platform == track.platform
                    && notes.channel == track.channel,
                "Notes source/build/platform/channel mismatch"
            );
            ensure!(notes.items.len() <= 1000, "Too many release items");
            ensure!(
                notes
                    .items
                    .iter()
                    .all(|i| i.pr > 0 && !i.text.trim().is_empty() && i.text.len() <= 4096),
                "Invalid release item"
            );
            Ok(notes)
        })();
        let Ok(notes) = parsed else { break };
        let malformed_previous = notes.previous_build.is_some() != notes.previous_source.is_some();
        let previous = notes.previous_build.zip(notes.previous_source.clone());
        result.builds.push(notes);
        let Some(since) = since else { return result };
        if malformed_previous {
            result.unavailable = Some(Unavailable::FetchFailed);
            return result;
        }
        let Some((build, source)) = previous else {
            // Reached the start of this platform/channel's published history.
            return result;
        };
        if build <= since {
            return result;
        }
        if build >= next.build {
            break;
        }
        next = Build { build, source };
    }
    result.unavailable = Some(Unavailable::FetchFailed);
    result
}

pub(super) fn load(current: &Build, since: Option<u64>) -> Range {
    load_on_track(current, since, Track::current(), &production_profile())
}

/// Timing and origin knobs, injected so tests bound attempts with a local
/// fake server instead of real feeds (#2451).
struct Profile {
    notes_root: String,
    first_connect: Duration,
    request: Duration,
    budget: Duration,
    attempts: usize,
}
fn production_profile() -> Profile {
    Profile {
        notes_root: NOTES_ROOT.to_owned(),
        first_connect: FIRST_CONNECT,
        request: REQUEST,
        budget: BUDGET,
        attempts: ATTEMPTS,
    }
}

fn load_on_track(
    current: &Build,
    since: Option<u64>,
    track: Option<Track>,
    profile: &Profile,
) -> Range {
    let Some(track) = track else {
        // A missing channel or client is not a network blip: never fetch, never
        // retry, and let the UI say what is actually wrong (#2451).
        return Range {
            unavailable: Some(Unavailable::UnknownChannel),
            ..Range::default()
        };
    };
    let Ok(client) = reqwest::blocking::Client::builder()
        .connect_timeout(profile.first_connect)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return Range {
            unavailable: Some(Unavailable::NoClient),
            ..Range::default()
        };
    };
    // One automatic retry keeps a transient blip from hiding notes that exist
    // (#2451). Each attempt gets a fresh budget, so the total stays bounded at
    // profile.attempts × profile.budget. A retry that loads less than an
    // earlier partial attempt must not discard what did load.
    let mut best: Option<Range> = None;
    for _ in 0..profile.attempts.max(1) {
        let deadline = Instant::now() + profile.budget;
        let mut first_request = true;
        let outcome = collect(current, since, track, |source| {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("Notes deadline")?;
            let cap = if std::mem::take(&mut first_request) {
                profile.first_connect
            } else {
                profile.request
            };
            let url = format!(
                "{}/{source}/{}/{}/notes.json",
                profile.notes_root, track.platform, track.channel
            );
            fetch_notes(&client, &url, cap.min(remaining))
        });
        if outcome.unavailable != Some(Unavailable::FetchFailed) {
            // Success and NotPublished are deterministic answers; keep the
            // richest view seen so far when a server flaps between answers.
            return match best {
                Some(best) if best.builds.len() > outcome.builds.len() => best,
                _ => outcome,
            };
        }
        if best
            .as_ref()
            .is_none_or(|best| best.builds.len() <= outcome.builds.len())
        {
            best = Some(outcome);
        }
    }
    best.unwrap_or_default()
}

fn fetch_notes(
    client: &reqwest::blocking::Client,
    url: &str,
    timeout: Duration,
) -> Result<Option<Vec<u8>>> {
    let response = client.get(url).timeout(timeout).send()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    read_bounded(response.error_for_status()?, MAX_BYTES).map(Some)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
