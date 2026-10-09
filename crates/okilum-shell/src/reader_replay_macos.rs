use anyhow::{ensure, Context, Result};
use core_foundation_sys::uuid::{CFUUIDGetUUIDBytes, CFUUIDRef};
use fsevent_sys::{self as fs, core_foundation as cf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::{c_void, CStr, CString};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn FSEventsCopyUUIDForDevice(dev: i32) -> CFUUIDRef;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRunLoopRunInMode(mode: cf::CFStringRef, seconds: f64, return_after_source: u8) -> i32;
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    root: PathBuf,
    volume: [u8; 16],
    inode: u64,
    event: u64,
    snapshot: String,
}

pub struct Replay {
    pub force_all: bool,
    pub dirty: Vec<String>,
    pub directories: Vec<String>,
    checkpoint: Option<(PathBuf, Cursor)>,
}
impl Replay {
    pub fn save(&self, snapshot: &str) -> Result<()> {
        let Some((path, cursor)) = &self.checkpoint else {
            return Ok(());
        };
        let parent = path.parent().unwrap();
        let ancestor = parent
            .ancestors()
            .find(|p| p.exists())
            .context("Cursor ancestor missing")?;
        let resolved = ancestor
            .canonicalize()?
            .join(parent.strip_prefix(ancestor)?);
        ensure!(
            !resolved.starts_with(&cursor.root),
            "Replay state must be outside vault"
        );
        std::fs::create_dir_all(parent)?;
        let mut value = serde_json::to_value(cursor)?;
        value["snapshot"] = snapshot.into();
        let temp = parent.join(format!(".cursor-{}", uuid::Uuid::new_v4()));
        // Durable cursor publication follows complete snapshot publication. A
        // mismatch after interruption always falls back to full reconciliation.
        let result = (|| -> Result<()> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&serde_json::to_vec(&value)?)?;
            file.sync_all()?;
            std::fs::rename(&temp, path)?;
            std::fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
}

fn volume(root: &Path) -> Result<([u8; 16], u64)> {
    let meta = std::fs::metadata(root)?;
    unsafe {
        let uuid = FSEventsCopyUUIDForDevice(meta.dev() as i32);
        ensure!(!uuid.is_null(), "Volume has no FSEvents UUID");
        let b = CFUUIDGetUUIDBytes(uuid);
        core_foundation_sys::base::CFRelease(uuid.cast());
        Ok((
            [
                b.byte0, b.byte1, b.byte2, b.byte3, b.byte4, b.byte5, b.byte6, b.byte7, b.byte8,
                b.byte9, b.byte10, b.byte11, b.byte12, b.byte13, b.byte14, b.byte15,
            ],
            meta.ino(),
        ))
    }
}

pub fn prepare(root: &Path, state: Option<&Path>, snapshot: Option<&str>) -> Replay {
    let mut result = Replay {
        force_all: true,
        dirty: Vec::new(),
        directories: Vec::new(),
        checkpoint: None,
    };
    let attempt = (|| -> Result<()> {
        let root = root.canonicalize()?;
        let state = state.context("No external state directory")?;
        let ancestor = state
            .ancestors()
            .find(|p| p.exists())
            .context("State ancestor missing")?;
        let resolved = ancestor.canonicalize()?.join(state.strip_prefix(ancestor)?);
        ensure!(
            !resolved.starts_with(&root),
            "Replay state must be outside vault"
        );
        let path = state.join("reader-replay").join(format!(
            "{:x}.json",
            Sha256::digest(root.to_string_lossy().as_bytes())
        ));
        let (volume, inode) = volume(&root)?;
        // A real ID captured BEFORE reconciliation, never kFSEventStreamEventIdSinceNow.
        let event = unsafe { fs::FSEventsGetCurrentEventId() };
        ensure!(
            event > 0 && event != u64::MAX,
            "Invalid FSEvents checkpoint"
        );
        let previous = std::fs::symlink_metadata(&path)
            .ok()
            .filter(|m| m.is_file() && m.len() < 16384)
            .and_then(|_| std::fs::read(&path).ok())
            .filter(|b| b.len() < 16384)
            .and_then(|bytes| serde_json::from_slice::<Cursor>(&bytes).ok());
        result.checkpoint = Some((
            path,
            Cursor {
                root: root.clone(),
                volume,
                inode,
                event,
                snapshot: String::new(),
            },
        ));
        if let Some(old) = previous.filter(|p| {
            p.root == root
                && p.volume == volume
                && p.inode == inode
                && p.event > 0
                && p.event <= event
                && snapshot == Some(p.snapshot.as_str())
        }) {
            (result.dirty, result.directories) = replay(&root, old.event)?;
            result.force_all = false;
        }
        Ok(())
    })();
    if let Err(error) = attempt {
        eprintln!("FSEvents replay unavailable; reconciling vault: {error}");
    }
    result
}

struct Batch {
    root: PathBuf,
    dirty: Vec<String>,
    directories: Vec<String>,
    done: bool,
    invalid: bool,
}
extern "C" fn callback(
    _: fs::FSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const fs::FSEventStreamEventFlags,
    _: *const fs::FSEventStreamEventId,
) {
    // Invoked synchronously by this worker's run loop; the Box outlives the stream.
    unsafe {
        let batch = &mut *info.cast::<Batch>();
        for n in 0..count {
            let flag = *flags.add(n);
            if flag
                & (fs::kFSEventStreamEventFlagMustScanSubDirs
                    | fs::kFSEventStreamEventFlagUserDropped
                    | fs::kFSEventStreamEventFlagKernelDropped
                    | fs::kFSEventStreamEventFlagEventIdsWrapped
                    | fs::kFSEventStreamEventFlagRootChanged
                    | fs::kFSEventStreamEventFlagMount
                    | fs::kFSEventStreamEventFlagUnmount)
                != 0
            {
                batch.invalid = true;
            }
            if flag & fs::kFSEventStreamEventFlagHistoryDone != 0 {
                batch.done = true;
                continue;
            }
            let path =
                CStr::from_ptr(*paths.cast::<*const std::ffi::c_char>().add(n)).to_string_lossy();
            match Path::new(path.as_ref()).strip_prefix(&batch.root) {
                Ok(relative) => super::record_path(
                    relative,
                    flag & fs::kFSEventStreamEventFlagItemIsDir != 0,
                    &mut batch.dirty,
                    &mut batch.directories,
                ),
                _ => batch.invalid = true,
            }
        }
    }
}

fn replay(root: &Path, since: u64) -> Result<(Vec<String>, Vec<String>)> {
    let path = CString::new(root.to_str().context("Non-UTF8 root")?)?;
    let mut batch = Box::new(Batch {
        root: root.into(),
        dirty: Vec::new(),
        directories: Vec::new(),
        done: false,
        invalid: false,
    });
    unsafe {
        let string = cf::CFStringCreateWithCString(
            cf::kCFAllocatorDefault,
            path.as_ptr(),
            cf::kCFStringEncodingUTF8,
        );
        ensure!(!string.is_null(), "Cannot create FSEvents path");
        let paths =
            cf::CFArrayCreateMutable(cf::kCFAllocatorDefault, 1, &cf::kCFTypeArrayCallBacks);
        if paths.is_null() {
            cf::CFRelease(string);
            anyhow::bail!("Cannot create FSEvents path array");
        }
        cf::CFArrayAppendValue(paths, string);
        cf::CFRelease(string);
        let context = fs::FSEventStreamContext {
            version: 0,
            info: (&mut *batch as *mut Batch).cast(),
            retain: None,
            release: None,
            copy_description: None,
        };
        let stream = fs::FSEventStreamCreate(
            cf::kCFAllocatorDefault,
            callback,
            &context,
            paths,
            since,
            0.01,
            fs::kFSEventStreamCreateFlagFileEvents
                | fs::kFSEventStreamCreateFlagWatchRoot
                | fs::kFSEventStreamCreateFlagNoDefer,
        );
        cf::CFRelease(paths);
        ensure!(!stream.is_null(), "Cannot create FSEvents stream");
        let run_loop = cf::CFRunLoopGetCurrent();
        fs::FSEventStreamScheduleWithRunLoop(stream, run_loop, cf::kCFRunLoopDefaultMode);
        let started = fs::FSEventStreamStart(stream) != 0;
        let deadline = Instant::now() + Duration::from_secs(3);
        while started && !batch.done && !batch.invalid && Instant::now() < deadline {
            CFRunLoopRunInMode(cf::kCFRunLoopDefaultMode, 0.05, 1);
        }
        if started {
            fs::FSEventStreamStop(stream);
        }
        fs::FSEventStreamInvalidate(stream);
        fs::FSEventStreamRelease(stream);
        ensure!(
            started && batch.done && !batch.invalid,
            "FSEvents history reset, incomplete or dropped"
        );
    }
    batch.dirty.sort();
    batch.dirty.dedup();
    batch.directories.sort();
    batch.directories.dedup();
    Ok((batch.dirty, batch.directories))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_separates_directories_and_keeps_lost_history_invalid() {
        let root = Path::new("/tmp/okilum-replay-callback");
        let paths = [
            CString::new(root.to_str().unwrap()).unwrap(),
            CString::new(root.join("Changed.md").to_str().unwrap()).unwrap(),
        ];
        let mut pointers = paths.iter().map(|p| p.as_ptr()).collect::<Vec<_>>();
        let flags = [
            fs::kFSEventStreamEventFlagItemIsDir | fs::kFSEventStreamEventFlagItemInodeMetaMod,
            fs::kFSEventStreamEventFlagItemIsFile | fs::kFSEventStreamEventFlagItemModified,
        ];
        let mut batch = Batch {
            root: root.into(),
            dirty: Vec::new(),
            directories: Vec::new(),
            done: false,
            invalid: false,
        };
        callback(
            std::ptr::null_mut(),
            (&mut batch as *mut Batch).cast(),
            2,
            pointers.as_mut_ptr().cast(),
            flags.as_ptr(),
            std::ptr::null(),
        );
        assert_eq!(batch.directories, [""]);
        assert_eq!(batch.dirty, ["Changed.md"]);
        assert!(!batch.invalid);

        for flag in [
            fs::kFSEventStreamEventFlagMustScanSubDirs,
            fs::kFSEventStreamEventFlagUserDropped,
            fs::kFSEventStreamEventFlagKernelDropped,
            fs::kFSEventStreamEventFlagEventIdsWrapped,
            fs::kFSEventStreamEventFlagRootChanged,
            fs::kFSEventStreamEventFlagMount,
            fs::kFSEventStreamEventFlagUnmount,
        ] {
            batch.invalid = false;
            callback(
                std::ptr::null_mut(),
                (&mut batch as *mut Batch).cast(),
                1,
                pointers.as_mut_ptr().cast(),
                &flag,
                std::ptr::null(),
            );
            assert!(
                batch.invalid,
                "uncertain history flag {flag:#x} must force reconciliation"
            );
        }
    }

    #[test]
    fn native_replay_observes_between_launch_writes_and_rejects_mismatched_snapshot() {
        let temp = std::env::temp_dir().join(format!("okilum-replay-{}", uuid::Uuid::new_v4()));
        let root = temp.join("vault");
        let state = temp.join("state");
        std::fs::create_dir_all(&root).unwrap();
        let first = prepare(&root, Some(&state), None);
        assert!(
            first.checkpoint.is_some(),
            "positive control: native FSEvents cursor available"
        );
        first.save("snapshot-one").unwrap();
        let before_write = unsafe { fs::FSEventsGetCurrentEventId() };
        std::fs::write(root.join("Changed.md"), "created while Reader is closed").unwrap();
        // HistoryDone means the currently available journal was replayed, not
        // that this write has reached fseventsd. Poll the journal ID and replay
        // from the SAME saved cursor until this fixture's event is observable.
        // Unrelated filesystem traffic advancing the global ID is not enough.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_dirty = Vec::new();
        let replay = loop {
            let observed_id = unsafe { fs::FSEventsGetCurrentEventId() };
            if observed_id > before_write {
                let replay = prepare(&root, Some(&state), Some("snapshot-one"));
                assert!(
                    !replay.force_all,
                    "native replay must not fall back while waiting for delivery"
                );
                if replay.dirty.iter().any(|p| p == "Changed.md") {
                    break replay;
                }
                last_dirty = replay.dirty;
            }
            assert!(Instant::now() < deadline,
                "Changed.md event was not delivered: before={before_write}, observed={observed_id}, dirty={last_dirty:?}");
            std::thread::sleep(Duration::from_millis(25));
        };
        assert!(
            !replay.force_all,
            "native replay must finish, not silently fall back"
        );
        assert!(replay.dirty.iter().any(|p| p == "Changed.md"));
        assert!(!replay.dirty.iter().any(String::is_empty));
        assert!(prepare(&root, Some(&state), Some("wrong-snapshot")).force_all);
        // Simulate replacement volume: never reuse its old event ID.
        let (path, _) = replay.checkpoint.as_ref().unwrap();
        let mut cursor: Cursor = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        cursor.volume = [0; 16];
        std::fs::write(path, serde_json::to_vec(&cursor).unwrap()).unwrap();
        assert!(prepare(&root, Some(&state), Some("snapshot-one")).force_all);
        std::fs::remove_dir_all(temp).unwrap();
    }
}
