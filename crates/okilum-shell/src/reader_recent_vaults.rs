//! The start screen's recent vaults as data, apart from how they are drawn
//! (#3208): which folders count, how many are listed, and why one cannot be
//! opened right now.
//!
//! Every folder ever opened used to stay on the list forever, including test
//! fixtures, scratch folders, app-support directories and folders that only
//! held a file opened on its own. The list now holds folders someone opened
//! as a vault, newest first, and says in plain words why one is unavailable.
use std::{
    path::{Component, Path, PathBuf},
    sync::{mpsc, OnceLock},
    time::{Duration, Instant},
};

/// The start screen lists at most this many vaults, most recent first.
pub(crate) const LIMIT: usize = 10;

/// How long the availability check waits for a folder before it calls its
/// volume unresponsive: a dead network share must not hold up the start
/// screen, and `stat` on one can block for minutes.
pub(crate) const CHECK_PATIENCE: Duration = Duration::from_millis(1500);

/// Places nobody picks as a vault: the system's temporary folders, QA
/// evidence, app-support and cache directories, and the trash. Folders there
/// were opened by a test, a script or another app, not chosen by a person.
#[derive(Clone, Debug, Default)]
pub(crate) struct Places {
    temp: Vec<PathBuf>,
}

/// A single path component that marks a scratch place, compared ignoring
/// case. `AppData` holds `Local\Temp` and every Windows app's own data.
const SCRATCH: &[&str] = &[
    "qa-evidence",
    "appdata",
    ".cache",
    ".trash",
    ".trashes",
    "$recycle.bin",
];

/// Consecutive components that mark an app's own data, compared ignoring
/// case. `~/Library/Mobile Documents` (iCloud Drive) is deliberately absent:
/// people keep vaults there.
const APP_DATA: &[&[&str]] = &[
    &["library", "application support"],
    &["library", "caches"],
    &["library", "logs"],
    &[".local", "state"],
    &[".local", "share", "trash"],
];

impl Places {
    /// The temporary folders of this machine, as typed and as resolved:
    /// recorded roots are canonical (`/private/var/folders/…` on macOS).
    pub(crate) fn current() -> &'static Self {
        static PLACES: OnceLock<Places> = OnceLock::new();
        PLACES.get_or_init(|| {
            let mut temp = vec![std::env::temp_dir()];
            #[cfg(unix)]
            temp.extend(
                [
                    "/tmp",
                    "/var/tmp",
                    "/private/tmp",
                    "/private/var/tmp",
                    "/var/folders",
                    "/private/var/folders",
                ]
                .map(PathBuf::from),
            );
            let resolved: Vec<PathBuf> =
                temp.iter().filter_map(|p| p.canonicalize().ok()).collect();
            temp.extend(resolved);
            Places::with_temp(temp)
        })
    }

    pub(crate) fn with_temp(temp: Vec<PathBuf>) -> Self {
        let mut temp: Vec<PathBuf> = temp
            .into_iter()
            .map(|path| okilum_core::vault::plain_path(&path))
            .collect();
        temp.sort();
        temp.dedup();
        Self { temp }
    }

    /// A folder nobody chose as a vault: it never belongs on the list.
    pub(crate) fn unchosen(&self, root: &Path) -> bool {
        let plain = okilum_core::vault::plain_path(root);
        if self.temp.iter().any(|temp| plain.starts_with(temp)) {
            return true;
        }
        let parts: Vec<String> = plain
            .components()
            .filter_map(|part| match part {
                Component::Normal(part) => Some(part.to_string_lossy().to_lowercase()),
                _ => None,
            })
            .collect();
        parts.iter().any(|part| SCRATCH.contains(&part.as_str()))
            || APP_DATA.iter().any(|sequence| {
                parts
                    .windows(sequence.len())
                    .any(|window| window.iter().zip(sequence.iter()).all(|(a, b)| a == b))
            })
    }
}

/// Whether a listed vault can be opened and edited now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Ready,
    /// It opens, but edits cannot be saved; the text says why.
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    ReadOnly(&'static str),
    Unavailable(Unavailable),
}

/// Why a listed vault cannot be opened right now, specific enough to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// The volume the folder lives on is not there: an unmounted network
    /// share or disk (`/Volumes/<name>`, `/mnt/<name>`), a drive letter, a
    /// Windows share.
    VolumeMissing {
        name: String,
        kind: Volume,
    },
    /// The volume is there but does not answer (a dead network mount).
    NotResponding,
    /// The volume is there; the folder is not.
    Missing,
    NotAFolder,
    /// macOS privacy settings keep Okilum out (`EPERM`), or the folder's
    /// permissions do (`EACCES`).
    NoAccess {
        privacy: bool,
    },
    /// Anything else, in the system's words.
    CantOpen(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Volume {
    /// A mount point whose `fstab` entry is a network filesystem.
    Network,
    /// A mount point nothing more is known about once it is gone.
    Unknown,
    /// A Windows drive letter.
    Drive,
    /// A Windows `\\server\share`.
    Share,
}

impl Unavailable {
    /// The reason the start screen shows on the vault's row (#3208 spec).
    pub(crate) fn reason(&self) -> String {
        match self {
            Unavailable::VolumeMissing { name, kind } => match kind {
                Volume::Network if cfg!(target_os = "macos") => {
                    format!("Network volume “{name}” isn’t connected")
                }
                Volume::Network => format!("Network share “{name}” isn’t mounted"),
                Volume::Unknown if cfg!(target_os = "macos") => {
                    format!("Volume “{name}” isn’t connected")
                }
                Volume::Unknown => format!("Volume “{name}” isn’t mounted"),
                Volume::Drive => format!("Drive “{name}” isn’t connected"),
                Volume::Share => format!("Network location {name} isn’t reachable"),
            },
            Unavailable::NotResponding => "Network folder isn’t responding".into(),
            Unavailable::Missing => "Folder was moved or deleted".into(),
            Unavailable::NotAFolder => "This path is now a file, not a folder".into(),
            Unavailable::NoAccess { .. } => "Okilum isn’t allowed to read this folder".into(),
            Unavailable::CantOpen(detail) => format!("Couldn’t open: {detail}"),
        }
    }

    /// The one fitting action next to the reason.
    pub(crate) fn action(&self) -> Fix {
        match self {
            Unavailable::VolumeMissing { .. } | Unavailable::NotResponding => Fix::TryAgain,
            Unavailable::Missing | Unavailable::NotAFolder => Fix::Locate,
            Unavailable::NoAccess { .. } if cfg!(target_os = "macos") => Fix::AllowAccess,
            Unavailable::NoAccess { .. } => Fix::Locate,
            Unavailable::CantOpen(_) => Fix::Details,
        }
    }
}

/// What the inline button on an unavailable row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fix {
    /// Check again: the volume may be back.
    TryAgain,
    /// Choose the folder where it is now; it replaces the entry.
    Locate,
    /// macOS: choose the same folder in the picker, which grants access.
    AllowAccess,
    /// Show the full error.
    Details,
}

impl Fix {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Fix::TryAgain => "Try again",
            Fix::Locate => "Locate…",
            Fix::AllowAccess => "Allow access…",
            Fix::Details => "Details…",
        }
    }
}

impl Status {
    pub(crate) fn unavailable(&self) -> Option<&Unavailable> {
        match self {
            Status::Unavailable(reason) => Some(reason),
            _ => None,
        }
    }

    /// The inline text for an unavailable vault. A read-only vault is
    /// available: its row shows a lock and «Read-only» instead.
    pub(crate) fn reason(&self) -> Option<String> {
        self.unavailable().map(Unavailable::reason)
    }

    pub(crate) fn read_only(&self) -> bool {
        matches!(self, Status::ReadOnly(_))
    }
}

/// Check one folder. Blocking: a network folder may take long, so callers
/// run this off the UI thread with a deadline ([`check_all`]).
/// `network`: the folder was on a network volume when last opened, so a
/// volume that is gone now is named a network volume.
pub(crate) fn check(root: &Path, network: bool) -> Status {
    match check_in(root, &mount_areas(), &fstab()) {
        Status::Unavailable(Unavailable::VolumeMissing {
            name,
            kind: Volume::Unknown,
        }) if network => Status::Unavailable(Unavailable::VolumeMissing {
            name,
            kind: Volume::Network,
        }),
        status => status,
    }
}

/// The folder is on a network filesystem (NFS, SMB, AFP, WebDAV, …), as the
/// kernel reports it. Recorded when a vault opens (#3208), so an unmounted
/// share can later be named a network volume.
pub(crate) fn on_network_volume(root: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        const NETWORK: &[&str] = &["nfs", "smbfs", "afpfs", "webdav", "ftp", "cifs"];
        rustix::fs::statfs(root).is_ok_and(|fs| {
            let name: String = fs
                .f_fstypename
                .iter()
                .take_while(|c| **c != 0)
                .map(|c| char::from(c.to_ne_bytes()[0]))
                .collect();
            NETWORK.contains(&name.as_str())
        })
    }
    #[cfg(target_os = "linux")]
    {
        // NFS, SMB, CIFS, SMB2, AFS, 9P, Ceph.
        const NETWORK: &[u32] = &[
            0x6969,
            0x517B,
            0xFF53_4D42,
            0xFE53_4D42,
            0x5346_414F,
            0x0102_1997,
            0x00C3_6400,
        ];
        rustix::fs::statfs(root).is_ok_and(|fs| {
            let kind = (i128::from(fs.f_type) & 0xFFFF_FFFF) as u32;
            NETWORK.contains(&kind) || gvfs_share(root).is_some()
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = root;
        false
    }
}

/// A GNOME gvfs mount (`/run/user/<uid>/gvfs/smb-share:server=nas,share=notes/…`):
/// its directory, the share's name and its `smb://server/share` spelling.
pub(crate) fn gvfs_share(root: &Path) -> Option<(PathBuf, String, String)> {
    let parts: Vec<&std::ffi::OsStr> = root
        .components()
        .filter_map(|part| match part {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    let [run, user, _uid, gvfs, mount, ..] = parts.as_slice() else {
        return None;
    };
    if *run != "run" || *user != "user" || *gvfs != "gvfs" || !root.has_root() {
        return None;
    }
    let mount = mount.to_str()?;
    let (scheme, rest) = mount.split_once(':')?;
    let field = |key: &str| {
        rest.split(',')
            .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
    };
    let server = field("server").or_else(|| field("host"))?;
    let share = field("share").unwrap_or(server);
    let scheme = scheme.strip_suffix("-share").unwrap_or(scheme);
    let directory: PathBuf = std::iter::once(std::ffi::OsStr::new(std::path::MAIN_SEPARATOR_STR))
        .chain(parts[..5].iter().copied())
        .collect();
    let uri = if share == server {
        format!("{scheme}://{server}")
    } else {
        format!("{scheme}://{server}/{share}")
    };
    Some((directory, share.to_owned(), uri))
}

fn check_in(root: &Path, areas: &[PathBuf], fstab: &str) -> Status {
    let result = std::fs::metadata(root).and_then(|metadata| {
        if !metadata.is_dir() {
            return Ok(Some(Status::Unavailable(Unavailable::NotAFolder)));
        }
        std::fs::read_dir(root).map(|_| None)
    });
    match result {
        Ok(Some(status)) => status,
        Ok(None) => writable(root),
        Err(error) => Status::Unavailable(from_error(root, &error, areas, fstab)),
    }
}

fn from_error(root: &Path, error: &std::io::Error, areas: &[PathBuf], fstab: &str) -> Unavailable {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => volume_missing(root, areas, fstab).unwrap_or(Unavailable::Missing),
        ErrorKind::PermissionDenied => Unavailable::NoAccess {
            privacy: cfg!(target_os = "macos") && error.raw_os_error() == Some(1),
        },
        ErrorKind::TimedOut
        | ErrorKind::HostUnreachable
        | ErrorKind::NetworkUnreachable
        | ErrorKind::NetworkDown
        | ErrorKind::NotConnected
        | ErrorKind::StaleNetworkFileHandle => Unavailable::NotResponding,
        _ => Unavailable::CantOpen(plain_error(error)),
    }
}

/// The system's message without the `(os error N)` tail.
fn plain_error(error: &std::io::Error) -> String {
    let text = error.to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

/// A folder that exists and can be read: can edits be saved there?
fn writable(root: &Path) -> Status {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use rustix::fs::{access, statvfs, Access, StatVfsMountFlags};
        if statvfs(root).is_ok_and(|fs| fs.f_flag.contains(StatVfsMountFlags::RDONLY)) {
            return Status::ReadOnly("read-only volume");
        }
        if access(root, Access::WRITE_OK).is_err() {
            return Status::ReadOnly("no write permission for this folder");
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = root;
    Status::Ready
}

/// Where removable and network volumes are mounted on this system.
fn mount_areas() -> Vec<PathBuf> {
    let areas: &[&str] = if cfg!(target_os = "macos") {
        &["/Volumes"]
    } else if cfg!(unix) {
        &["/mnt", "/media", "/run/media"]
    } else {
        &[]
    };
    areas.iter().map(PathBuf::from).collect()
}

fn fstab() -> String {
    #[cfg(unix)]
    {
        std::fs::read_to_string("/etc/fstab").unwrap_or_default()
    }
    #[cfg(not(unix))]
    {
        String::new()
    }
}

/// The folder is missing because its volume is: the drive or share is gone
/// (Windows), or the folder sits under a mount area (`/Volumes`, `/mnt`, …)
/// and the volume directory there is absent or no longer a mount point.
fn volume_missing(root: &Path, areas: &[PathBuf], fstab: &str) -> Option<Unavailable> {
    if let Some(Component::Prefix(prefix)) = root.components().next() {
        use std::path::Prefix;
        let (name, kind) = match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => (
                format!("{}:", char::from(letter).to_ascii_uppercase()),
                Volume::Drive,
            ),
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => (
                format!(
                    r"\\{}\{}",
                    server.to_string_lossy(),
                    share.to_string_lossy()
                ),
                Volume::Share,
            ),
            _ => return None,
        };
        let mut top = PathBuf::from(prefix.as_os_str());
        top.push(std::path::MAIN_SEPARATOR_STR);
        return (!top.exists()).then_some(Unavailable::VolumeMissing { name, kind });
    }
    if let Some((mount, share, _)) = gvfs_share(root) {
        return (!mount.exists()).then_some(Unavailable::VolumeMissing {
            name: share,
            kind: Volume::Network,
        });
    }
    // The nearest folder that is still there.
    let present = root.ancestors().skip(1).find(|path| path.is_dir())?;
    for area in areas {
        let Ok(below) = root.strip_prefix(area) else {
            continue;
        };
        // `/media/<user>/<volume>`: the user's folder is not the volume.
        let user_level = present != area.as_path()
            && present.parent() == Some(area.as_path())
            && !is_mount_point(present)
            && !area.ends_with("mnt")
            && !area.ends_with("Volumes");
        let mount = if present == area.as_path() || user_level {
            let name = root.strip_prefix(present).ok()?.components().next()?;
            present.join(name)
        } else {
            let first = below.components().next()?;
            let mount = area.join(first);
            if mount != present || is_mount_point(present) {
                return None;
            }
            mount
        };
        let name = mount.file_name()?.to_string_lossy().into_owned();
        let kind = if network_mount(fstab, &mount) {
            Volume::Network
        } else {
            Volume::Unknown
        };
        return Some(Unavailable::VolumeMissing { name, kind });
    }
    None
}

/// A folder on another device than its parent: something is mounted there.
fn is_mount_point(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (Ok(own), Some(Ok(parent))) = (
            std::fs::metadata(path),
            path.parent().map(std::fs::metadata),
        ) else {
            return false;
        };
        own.dev() != parent.dev()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// `fstab` mounts a network filesystem at this folder.
fn network_mount(fstab: &str, mount: &Path) -> bool {
    const NETWORK: &[&str] = &[
        "nfs",
        "nfs4",
        "cifs",
        "smbfs",
        "smb3",
        "afpfs",
        "webdav",
        "davfs",
        "fuse.sshfs",
        "sshfs",
        "9p",
        "url",
    ];
    fstab.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let (Some(source), Some(target), Some(kind)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return false;
        };
        !source.starts_with('#')
            && Path::new(&target.replace("\\040", " ")) == mount
            && (NETWORK.contains(&kind) || source.starts_with("//") || source.contains(":/"))
    })
}

/// Check each folder on its own thread and wait at most `patience` in all;
/// a folder that has not answered by then is on a volume that does not
/// respond. A thread stuck in the kernel is left behind, never joined.
pub(crate) fn check_all(
    roots: &[PathBuf],
    network: &std::collections::BTreeSet<PathBuf>,
    patience: Duration,
) -> Vec<(PathBuf, Status)> {
    let network: Vec<bool> = roots.iter().map(|root| network.contains(root)).collect();
    check_all_with(roots, &network, patience, check)
}

fn check_all_with(
    roots: &[PathBuf],
    network: &[bool],
    patience: Duration,
    check: fn(&Path, bool) -> Status,
) -> Vec<(PathBuf, Status)> {
    let (sender, receiver) = mpsc::channel();
    let mut statuses: Vec<Option<Status>> = vec![None; roots.len()];
    for (index, root) in roots.iter().enumerate() {
        let sender = sender.clone();
        let root = root.clone();
        let network = network.get(index).copied().unwrap_or(false);
        let spawned = std::thread::Builder::new()
            .name("okilum-recent-check".into())
            .spawn(move || {
                let _ = sender.send((index, check(&root, network)));
            });
        if let Err(error) = spawned {
            diagnostic_error!("Cannot check a recent vault: {error}");
            statuses[index] = Some(Status::Ready);
        }
    }
    drop(sender);
    let deadline = Instant::now() + patience;
    while statuses.iter().any(Option::is_none) {
        let left = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(left) {
            Ok((index, status)) => statuses[index] = Some(status),
            Err(_) => break,
        }
    }
    roots
        .iter()
        .cloned()
        .zip(statuses)
        .map(|(root, status)| {
            (
                root,
                status.unwrap_or(Status::Unavailable(Unavailable::NotResponding)),
            )
        })
        .collect()
}

/// The history is the user's own: neither a QA instance's
/// (`OKILUM_STATE_DIR`) nor a test's explicit state directory. Only the
/// user's own history drops folders nobody chose as a vault; QA fixtures
/// live in temporary folders on purpose.
pub(crate) fn users_history(directory: &Path) -> bool {
    super::reader_history::isolated_state_directory().is_none()
        && super::reader_history::state_directory().is_ok_and(|own| own == directory)
}

/// Take folders nobody chose as a vault off the list for good (#3208):
/// listed roots under [`Places`] become hidden. Only for the user's own
/// history; a QA instance with its own state keeps what it opens.
pub(crate) fn prune(directory: &Path, places: &Places) -> anyhow::Result<Vec<PathBuf>> {
    let (_, listed) = super::reader_history::ReadingHistory::startup_roots(directory)?;
    let unchosen: Vec<PathBuf> = listed
        .into_iter()
        .filter(|root| places.unchosen(root))
        .collect();
    if !unchosen.is_empty() {
        super::reader_history::ReadingHistory::hide_roots(directory, &unchosen)?;
    }
    Ok(unchosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(parts: &[&str]) -> PathBuf {
        let mut path = PathBuf::from(std::path::MAIN_SEPARATOR_STR);
        path.extend(parts);
        path
    }

    #[test]
    fn issue3208_scratch_app_data_and_temp_folders_are_unchosen() {
        let temp = path(&["scratch", "tmp"]);
        let places = Places::with_temp(vec![temp.clone()]);
        for junk in [
            // The entries in Oleg's report.
            path(&[
                "Users",
                "o",
                "work",
                "qa-evidence",
                "panels-2693",
                "fixture-a",
            ]),
            path(&[
                "Users",
                "o",
                "work",
                "QA-Evidence",
                "sync-1108",
                "production-fix-vault",
            ]),
            path(&[
                "Users",
                "o",
                "Library",
                "Application Support",
                "com.prakashjoshipax.VoiceInk",
                "Meetings",
                "D8B1ABFA-1731-48C9-BC91-259810B1639C",
            ]),
            path(&["Users", "o", "Library", "Caches", "app", "notes"]),
            path(&["home", "o", ".cache", "thing"]),
            path(&["home", "o", ".local", "state", "okilum", "x"]),
            path(&["home", "o", ".local", "share", "Trash", "files", "Notes"]),
            path(&["Users", "o", ".Trash", "Notes"]),
            path(&["Users", "o", "AppData", "Local", "Temp", "x"]),
            temp.join("okilum-fixture-1"),
        ] {
            assert!(
                places.unchosen(&junk),
                "{} is not a chosen vault",
                junk.display()
            );
        }
        for vault in [
            path(&["Users", "o", "vaults", "brain"]),
            path(&[
                "Volumes",
                "Maestro",
                "src",
                "_DropBox",
                "okilum",
                "theme-showcase",
            ]),
            // iCloud Drive, where Obsidian keeps vaults.
            path(&[
                "Users",
                "o",
                "Library",
                "Mobile Documents",
                "iCloud~md~obsidian",
                "Documents",
                "Notes",
            ]),
            path(&["Users", "o", "Developer", "SAP", "vault"]),
            // Only the whole component counts.
            path(&["Users", "o", "qa-evidence-notes"]),
            path(&["Users", "o", "Library Notes", "Application Support"]),
            path(&["scratch", "tmpfiles", "vault"]),
        ] {
            assert!(!places.unchosen(&vault), "{} is a vault", vault.display());
        }
        // Positive control for the temp rule: without it the fixture is kept.
        assert!(!Places::default().unchosen(&temp.join("okilum-fixture-1")));
    }

    #[test]
    fn issue3208_current_places_include_the_system_temp_folder() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap().join("vault");
        assert!(Places::current().unchosen(&root), "{}", root.display());
    }

    #[test]
    fn issue3208_reasons_are_specific_and_plain() {
        let volume = |name: &str, kind| Unavailable::VolumeMissing {
            name: name.into(),
            kind,
        };
        let mac = cfg!(target_os = "macos");
        let cases = [
            (
                volume("Maestro", Volume::Network),
                if mac {
                    "Network volume “Maestro” isn’t connected"
                } else {
                    "Network share “Maestro” isn’t mounted"
                },
                Fix::TryAgain,
            ),
            (
                volume("Backup", Volume::Unknown),
                if mac {
                    "Volume “Backup” isn’t connected"
                } else {
                    "Volume “Backup” isn’t mounted"
                },
                Fix::TryAgain,
            ),
            (
                volume("Z:", Volume::Drive),
                "Drive “Z:” isn’t connected",
                Fix::TryAgain,
            ),
            (
                volume(r"\\nas\notes", Volume::Share),
                r"Network location \\nas\notes isn’t reachable",
                Fix::TryAgain,
            ),
            (
                Unavailable::NotResponding,
                "Network folder isn’t responding",
                Fix::TryAgain,
            ),
            (
                Unavailable::Missing,
                "Folder was moved or deleted",
                Fix::Locate,
            ),
            (
                Unavailable::NotAFolder,
                "This path is now a file, not a folder",
                Fix::Locate,
            ),
            (
                Unavailable::NoAccess { privacy: false },
                "Okilum isn’t allowed to read this folder",
                if mac { Fix::AllowAccess } else { Fix::Locate },
            ),
            (
                Unavailable::CantOpen("Input/output error".into()),
                "Couldn’t open: Input/output error",
                Fix::Details,
            ),
        ];
        for (reason, text, fix) in cases {
            assert_eq!(reason.reason(), text);
            assert_eq!(reason.action(), fix);
            assert_eq!(Status::Unavailable(reason).reason().as_deref(), Some(text));
        }
        assert_eq!(Status::ReadOnly("read-only volume").reason(), None);
        assert!(Status::ReadOnly("read-only volume").read_only());
        assert_eq!(Status::Ready.reason(), None);
        let error = std::io::Error::from_raw_os_error(if cfg!(windows) { 87 } else { 22 });
        let detail = plain_error(&error);
        assert!(!detail.contains("os error"), "{detail}");
        assert!(!detail.is_empty());
    }

    #[test]
    fn issue3208_a_volume_last_seen_on_the_network_is_named_so() {
        let fixture = tempfile::tempdir().unwrap();
        let area = fixture.path().canonicalize().unwrap().join("Volumes");
        std::fs::create_dir(&area).unwrap();
        let root = area.join("Maestro").join("theme-showcase");
        let unknown = check_in(&root, std::slice::from_ref(&area), "");
        assert_eq!(
            unknown,
            Status::Unavailable(Unavailable::VolumeMissing {
                name: "Maestro".into(),
                kind: Volume::Unknown,
            })
        );
        // `check` upgrades an unknown volume when the history says network;
        // the same path checked through `check_in` stays unknown (control).
        let upgraded = match unknown {
            Status::Unavailable(Unavailable::VolumeMissing { name, .. }) => {
                Status::Unavailable(Unavailable::VolumeMissing {
                    name,
                    kind: Volume::Network,
                })
            }
            other => other,
        };
        assert_eq!(
            upgraded.reason().unwrap(),
            if cfg!(target_os = "macos") {
                "Network volume “Maestro” isn’t connected"
            } else {
                "Network share “Maestro” isn’t mounted"
            }
        );
    }

    #[test]
    fn issue3208_gvfs_shares_are_recognised() {
        let root = Path::new("/run/user/1000/gvfs/smb-share:server=nas,share=notes/Work/Vault");
        let (mount, share, uri) = gvfs_share(root).unwrap();
        assert_eq!(
            mount,
            Path::new("/run/user/1000/gvfs/smb-share:server=nas,share=notes")
        );
        assert_eq!(share, "notes");
        assert_eq!(uri, "smb://nas/notes");
        assert!(gvfs_share(Path::new("/home/o/run/user/1/gvfs/x:server=a")).is_none());
        assert!(gvfs_share(Path::new("/run/user/1000/other/smb-share:server=nas")).is_none());
        // A gvfs mount that is gone names its share as a network share.
        if cfg!(unix) {
            assert_eq!(
                volume_missing(root, &[], ""),
                Some(Unavailable::VolumeMissing {
                    name: "notes".into(),
                    kind: Volume::Network,
                })
            );
        }
    }

    #[test]
    fn issue3208_ready_missing_and_not_a_folder() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().canonicalize().unwrap();
        let vault = base.join("Vault");
        std::fs::create_dir(&vault).unwrap();
        let file = base.join("note.md");
        std::fs::write(&file, "# Note").unwrap();
        assert_eq!(check_in(&vault, &[], ""), Status::Ready);
        assert_eq!(
            check_in(&base.join("Gone"), &[], ""),
            Status::Unavailable(Unavailable::Missing)
        );
        assert_eq!(
            check_in(&file, &[], ""),
            Status::Unavailable(Unavailable::NotAFolder)
        );
    }

    #[test]
    fn issue3208_a_folder_on_a_volume_that_is_not_mounted_names_the_volume() {
        let fixture = tempfile::tempdir().unwrap();
        let area = fixture.path().canonicalize().unwrap().join("Volumes");
        std::fs::create_dir(&area).unwrap();
        let areas = [area.clone()];
        // `/Volumes/Maestro` is gone altogether (macOS unmount).
        let root = area.join("Maestro").join("src").join("theme-showcase");
        assert_eq!(
            check_in(&root, &areas, ""),
            Status::Unavailable(Unavailable::VolumeMissing {
                name: "Maestro".into(),
                kind: Volume::Unknown,
            })
        );
        // `fstab` says what was mounted there.
        let fstab = format!(
            "# comment\n10.0.0.14:/srv/nfs/Maestro {} nfs ro 0 0\n",
            area.join("Maestro").display()
        );
        assert_eq!(
            check_in(&root, &areas, &fstab),
            Status::Unavailable(Unavailable::VolumeMissing {
                name: "Maestro".into(),
                kind: Volume::Network,
            })
        );
        // `/mnt/nas` is still there as an empty folder, not a mount point.
        let empty = area.join("nas");
        std::fs::create_dir(&empty).unwrap();
        assert_eq!(
            check_in(&empty.join("Notes"), &areas, ""),
            Status::Unavailable(Unavailable::VolumeMissing {
                name: "nas".into(),
                kind: Volume::Unknown,
            })
        );
        // Control: a folder missing deeper inside a present volume directory
        // is just missing, and nothing outside a mount area names a volume.
        let deep = empty.join("Notes");
        std::fs::create_dir(&deep).unwrap();
        assert_eq!(
            check_in(&deep.join("Gone"), &areas, ""),
            Status::Unavailable(Unavailable::Missing)
        );
        assert_eq!(
            check_in(&root, &[], ""),
            Status::Unavailable(Unavailable::Missing)
        );
    }

    #[test]
    fn issue3208_fstab_network_entries() {
        let mount = Path::new("/mnt/My Notes");
        assert!(network_mount(
            "nas:/export /mnt/My\\040Notes nfs4 rw 0 0",
            mount
        ));
        assert!(network_mount(
            "//nas/notes /mnt/My\\040Notes cifs rw 0 0",
            mount
        ));
        assert!(network_mount(
            "nas:/export /mnt/My\\040Notes fuse.sshfs rw 0 0",
            mount
        ));
        assert!(!network_mount(
            "UUID=1234 /mnt/My\\040Notes ext4 rw 0 0",
            mount
        ));
        assert!(!network_mount(
            "#nas:/export /mnt/My\\040Notes nfs rw 0 0",
            mount
        ));
        assert!(!network_mount("nas:/export /mnt/Other nfs rw 0 0", mount));
    }

    #[cfg(unix)]
    #[test]
    fn issue3208_permissions_name_what_cannot_be_done() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().canonicalize().unwrap();
        let read_only = base.join("Read only");
        let closed = base.join("Closed");
        std::fs::create_dir(&read_only).unwrap();
        std::fs::create_dir(&closed).unwrap();
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o555)).unwrap();
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads and writes everything: the permission checks cannot fire.
        let privileged = std::fs::read_dir(&closed).is_ok();
        let results = (check_in(&read_only, &[], ""), check_in(&closed, &[], ""));
        for path in [&read_only, &closed] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        if privileged {
            return;
        }
        assert_eq!(
            results.0,
            Status::ReadOnly("no write permission for this folder")
        );
        assert_eq!(
            results.1,
            Status::Unavailable(Unavailable::NoAccess { privacy: false })
        );
    }

    #[cfg(windows)]
    #[test]
    fn issue3208_a_drive_that_is_not_connected_is_named() {
        let letter = ('D'..='Z')
            .rev()
            .find(|letter| !Path::new(&format!("{letter}:\\")).exists())
            .expect("a free drive letter");
        let root = PathBuf::from(format!(r"\\?\{letter}:\Notes\Vault"));
        assert_eq!(
            check(&root, false),
            Status::Unavailable(Unavailable::VolumeMissing {
                name: format!("{letter}:"),
                kind: Volume::Drive,
            })
        );
    }

    #[test]
    fn issue3208_a_folder_that_does_not_answer_is_not_responding() {
        fn hangs(root: &Path, _: bool) -> Status {
            if root.ends_with("dead") {
                std::thread::sleep(Duration::from_secs(5));
            }
            Status::Ready
        }
        let roots = [path(&["live"]), path(&["dead"]), path(&["also-live"])];
        let started = Instant::now();
        let statuses = check_all_with(&roots, &[], Duration::from_millis(200), hangs);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the deadline holds"
        );
        assert_eq!(
            statuses,
            vec![
                (roots[0].clone(), Status::Ready),
                (
                    roots[1].clone(),
                    Status::Unavailable(Unavailable::NotResponding)
                ),
                (roots[2].clone(), Status::Ready),
            ]
        );
    }

    #[test]
    fn issue3208_prune_hides_unchosen_vaults_and_keeps_the_rest() {
        use super::super::reader_history::ReadingHistory;
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().canonicalize().unwrap();
        let state = base.join("state");
        let kept = base.join("Notes");
        let qa = base.join("qa-evidence").join("panels").join("fixture-a");
        let scratch = base.join("scratch").join("vault");
        for root in [&kept, &qa, &scratch] {
            std::fs::create_dir_all(root).unwrap();
            ReadingHistory::record_usable_document(&state, root, "a.md").unwrap();
        }
        ReadingHistory::set_default_vault(&state, Some(qa.as_path())).unwrap();
        let places = Places::with_temp(vec![base.join("scratch")]);
        let mut pruned = prune(&state, &places).unwrap();
        pruned.sort();
        let mut expected = vec![qa.clone(), scratch.clone()];
        expected.sort();
        assert_eq!(pruned, expected);
        let (last, listed) = ReadingHistory::startup_roots(&state).unwrap();
        assert_eq!(listed, vec![kept.clone()]);
        assert_eq!(
            last,
            Some(kept.clone()),
            "an unchosen folder never reopens at startup"
        );
        assert_eq!(ReadingHistory::default_vault(&state).unwrap(), None);
        assert!(
            prune(&state, &places).unwrap().is_empty(),
            "nothing left to prune"
        );
    }
}
