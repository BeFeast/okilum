//! Exact, portable knowledge archives. No operational state or provider actions.
use crate::source::{SourceStore, WriteBoundary};
use anyhow::{bail, ensure, Context, Result};
use comrak::{nodes::NodeValue, parse_document, Arena};
use rustix::fs::{openat, Dir, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::Path,
};

pub const EXPORT_SCHEMA: &str = "tessera-knowledge-export/v1";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportFile {
    /// Original path relative to the selected root (under brain/ in the tar).
    pub path: String,
    pub bytes: u64,
    pub revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportExclusion {
    pub path: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportDependency {
    pub source: String,
    pub target: String,
    pub status: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportManifest {
    pub schema: String,
    pub archive_root: String,
    pub execution_restored: bool,
    pub files: Vec<ExportFile>,
    pub exclusions: Vec<ExportExclusion>,
    pub dependencies: Vec<ExportDependency>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportReceipt {
    pub destination: String,
    pub manifest: ExportManifest,
}

/// Save an archive on the client host, even when its brain lives elsewhere.
/// The transport supplies bounded chunks; a failed/interrupted transfer never
/// publishes a partial file. Content hash and length are checked independently
/// of chunk metadata before an atomic, non-overwriting publication.
pub fn save_download(
    destination: &Path,
    expected_bytes: u64,
    expected_revision: &str,
    mut next_chunk: impl FnMut(u64) -> Result<Vec<u8>>,
) -> Result<()> {
    ensure!(
        expected_revision.len() == 71
            && expected_revision.starts_with("sha256:")
            && expected_revision[7..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "invalid archive revision"
    );
    let parent = destination
        .parent()
        .context("destination needs a parent")?
        .canonicalize()?;
    let destination = parent.join(
        destination
            .file_name()
            .context("destination needs a filename")?,
    );
    ensure!(
        !destination.try_exists()?,
        "export destination already exists"
    );
    let mut temporary = tempfile::NamedTempFile::new_in(&parent)?;
    let mut offset = 0;
    let mut hash = Sha256::new();
    while offset < expected_bytes {
        let bytes = next_chunk(offset)?;
        ensure!(
            !bytes.is_empty() && bytes.len() <= 1024 * 1024,
            "invalid export chunk size"
        );
        ensure!(
            bytes.len() as u64 <= expected_bytes - offset,
            "export exceeds declared size"
        );
        temporary.write_all(&bytes)?;
        hash.update(&bytes);
        offset += bytes.len() as u64;
    }
    ensure!(
        format!("sha256:{:x}", hash.finalize()) == expected_revision,
        "archive checksum mismatch; no file published"
    );
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(destination)
        .map_err(|e| e.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

impl SourceStore {
    /// Export only saved knowledge. All managed source writers share this lock;
    /// the owning runner must also serialize this call with record transactions.
    /// A second inventory detects external additions/removals/content changes.
    /// Unmanaged concurrent writers are not a supported snapshot boundary.
    pub fn export_exact(&self, destination: &Path) -> Result<ExportReceipt> {
        self.export_with_hook(destination, || Ok(()))
    }

    fn export_with_hook(
        &self,
        destination: &Path,
        hook: impl FnOnce() -> Result<()>,
    ) -> Result<ExportReceipt> {
        ensure!(
            self.boundary == WriteBoundary::Managed,
            "exact export requires a managed brain"
        );
        let parent = destination
            .parent()
            .context("export destination needs a parent directory")?
            .canonicalize()?;
        ensure!(
            !parent.starts_with(&self.root_path),
            "export destination must be outside the brain"
        );
        let name = destination
            .file_name()
            .context("export destination needs a filename")?;
        let destination = parent.join(name);
        ensure!(
            !destination.try_exists()?,
            "export destination already exists"
        );
        let _lock = self.lock()?;
        let mut temporary = tempfile::NamedTempFile::new_in(&parent)?;
        let mut archive = tar::Builder::new(temporary.as_file_mut());
        let mut notes = BTreeMap::new();
        let (files, exclusions) = inventory(&self.root, "", &mut |path, bytes| {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, format!("brain/{path}"), bytes)?;
            if path.to_ascii_lowercase().ends_with(".md") {
                // Invalid UTF-8 is still preserved exactly in the archive.
                notes.insert(path.to_owned(), String::from_utf8_lossy(bytes).into_owned());
            }
            Ok(())
        })?;
        hook()?;
        let (checked_files, checked_exclusions) = inventory(&self.root, "", &mut |_, _| Ok(()))?;
        ensure!(
            files == checked_files && exclusions == checked_exclusions,
            "brain changed during export; no archive published, retry"
        );
        let manifest = ExportManifest {
            schema: EXPORT_SCHEMA.into(),
            archive_root: "brain".into(),
            execution_restored: false,
            dependencies: dependencies(&files, &notes),
            files,
            exclusions,
        };
        let bytes = serde_json::to_vec_pretty(&manifest)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, "manifest.json", bytes.as_slice())?;
        archive.finish()?;
        drop(archive);
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(&destination)
            .map_err(|e| e.error)?;
        File::open(parent)?.sync_all()?;
        Ok(ExportReceipt {
            destination: destination.to_string_lossy().into_owned(),
            manifest,
        })
    }
}

// Explicit reserved metadata names only: hidden Markdown and adjacent media
// remain canonical. Operational journals/configuration live outside the root by
// SourceStore contract. This is a path boundary, not secret-content detection.
fn excluded(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".obsidian"
            | ".tessera"
            | ".tessera-index"
            | ".env"
            | ".env.local"
            | ".env.production"
            | "credentials.json"
    )
}

fn inventory(
    root: &File,
    prefix: &str,
    visitor: &mut impl FnMut(&str, &[u8]) -> Result<()>,
) -> Result<(Vec<ExportFile>, Vec<ExportExclusion>)> {
    let mut entries = Vec::new();
    for entry in Dir::read_from(root)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .context("export requires UTF-8 paths")?
            .to_owned();
        if name != "." && name != ".." {
            entries.push((name, entry.file_type()));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut files = Vec::new();
    let mut exclusions = Vec::new();
    for (name, kind) in entries {
        let path = format!("{prefix}{name}");
        ensure!(
            !name.contains(['\\', '\0']),
            "unsupported archive path: {path}"
        );
        if excluded(&name) {
            exclusions.push(ExportExclusion {
                path,
                reason: "reserved_metadata_or_credentials".into(),
            });
            continue;
        }
        if kind == FileType::Symlink {
            exclusions.push(ExportExclusion {
                path,
                reason: "symlink_not_followed".into(),
            });
            continue;
        }
        let fd = openat(
            root,
            name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| format!("cannot read {path}; no archive published"))?;
        let mut file = File::from(fd);
        let metadata = file.metadata()?;
        if metadata.is_dir() {
            let (nested, omitted) = inventory(&file, &format!("{path}/"), visitor)?;
            files.extend(nested);
            exclusions.extend(omitted);
        } else if metadata.is_file() {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .with_context(|| format!("cannot read {path}"))?;
            ensure!(
                bytes.len() as u64 == metadata.len(),
                "file changed while reading {path}; retry export"
            );
            visitor(&path, &bytes)?;
            files.push(ExportFile {
                path,
                bytes: bytes.len() as u64,
                revision: format!("sha256:{:x}", Sha256::digest(&bytes)),
            });
        } else {
            bail!("unsupported non-regular source {path}; no archive published");
        }
    }
    Ok((files, exclusions))
}

fn local_path(from: &str, target: &str) -> Option<String> {
    if target.starts_with('/') || target.contains(':') || target.contains('\\') {
        return None;
    }
    let mut parts: Vec<_> = from.split('/').collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

fn dependencies(files: &[ExportFile], notes: &BTreeMap<String, String>) -> Vec<ExportDependency> {
    let mut output = Vec::new();
    let frontmatter_wiki =
        regex::Regex::new(r"\[\[([^\]\[|]+)(?:\|[^\]\[]*)?\]\]").expect("constant regex");
    for (source, text) in notes {
        let arena = Arena::new();
        let processed = crate::render::preprocess(text);
        let root = parse_document(&arena, &processed, &crate::render::comrak_options());
        let mut targets = Vec::new();
        for node in root.descendants() {
            let data = node.data.borrow();
            match &data.value {
                NodeValue::WikiLink(link) => targets.push((link.url.clone(), true)),
                NodeValue::Link(link) | NodeValue::Image(link) => {
                    targets.push((link.url.clone(), false))
                }
                NodeValue::FrontMatter(frontmatter) => {
                    for capture in frontmatter_wiki.captures_iter(frontmatter) {
                        targets.push((capture[1].to_owned(), true));
                    }
                }
                _ => {}
            }
        }
        for (raw, wiki) in targets {
            let raw = crate::render::percent_decode(&raw);
            let target = raw.split(['#', '^', '?']).next().unwrap_or("").trim();
            if target.is_empty() {
                continue;
            }
            // Remote web/provider references are historical text, not local dependencies.
            if target.contains(':') && !target.starts_with("file:") {
                continue;
            }
            let candidate = local_path(source, target);
            let status = if let Some(candidate) = candidate {
                let exact = |p: &str| {
                    files.iter().any(|f| {
                        if wiki {
                            f.path.to_lowercase() == p.to_lowercase()
                                || f.path.to_lowercase() == format!("{p}.md").to_lowercase()
                        } else {
                            f.path == p
                        }
                    })
                };
                // Bare wikilinks use root/suffix identity, not a preferred
                // sibling. Explicit ./ and ../ references are source-relative.
                let resolved = if wiki && !target.starts_with('.') {
                    exact(target)
                } else {
                    exact(&candidate) || (!target.starts_with('.') && exact(target))
                };
                if resolved {
                    continue;
                }
                if wiki && !target.starts_with('.') {
                    let matches = files
                        .iter()
                        .filter(|f| {
                            let p = f.path.to_lowercase();
                            let t = target.to_lowercase();
                            p.ends_with(&format!("/{t}")) || p.ends_with(&format!("/{t}.md"))
                        })
                        .count();
                    if matches == 1 {
                        continue;
                    }
                    if matches > 1 {
                        "ambiguous"
                    } else {
                        "unresolved"
                    }
                } else {
                    "unresolved"
                }
            } else {
                "external"
            };
            let dependency = ExportDependency {
                source: source.clone(),
                target: target.into(),
                status: status.into(),
            };
            if !output.contains(&dependency) {
                output.push(dependency);
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn setup() -> (tempfile::TempDir, SourceStore) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("brain")).unwrap();
        fs::create_dir(dir.path().join("state")).unwrap();
        let store = SourceStore::open(
            "01000000-0000-4000-8000-000000000001",
            &dir.path().join("brain"),
            &dir.path().join("state"),
            WriteBoundary::Managed,
        )
        .unwrap();
        (dir, store)
    }
    fn write(root: &Path, path: &str, bytes: &[u8]) {
        let target = root.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    #[test]
    fn extract_exact_notes_records_and_unknown_attachments_without_original_database() {
        let (temp, store) = setup();
        let root = temp.path().join("brain");
        let originals = [
            (
                "notes/hello world.md",
                b"---\r\ntype: Note\r\n---\r\n# Hello\r\n![](image.bin)\n[[../records/result]]\n"
                    .as_slice(),
            ),
            (
                "notes/image.bin",
                b"\0\xff\xfe\r\nnon-renderable media".as_slice(),
            ),
            (
                "records/result.md",
                b"---\nstatus: verified\nengine_ref: historical-thread\n---\n# Result\n".as_slice(),
            ),
            ("invalid.md", b"# exact\n\xff".as_slice()),
            (".private-note.md", b"# Canonical hidden note".as_slice()),
            ("РЕШЕНИЕ.md", "# Решение\n[[решение]]".as_bytes()),
            (".assets/image.bin", b"hidden attachment".as_slice()),
            ("index/real-note.md", b"# Real note named index".as_slice()),
        ];
        for (path, bytes) in originals {
            write(&root, path, bytes);
        }
        write(&root, ".env", b"must not export");
        write(&root, ".git/config", b"must not export");
        write(&root, ".tessera-index/search.data", b"derived");
        write(&root, "credentials.json", b"private");
        write(&temp.path().join("state"), "journal.json", b"operational");
        let out = temp.path().join("brain.tar");
        let receipt = store.export_exact(&out).unwrap();
        assert_eq!(receipt.manifest.files.len(), originals.len());
        assert!(receipt.manifest.dependencies.is_empty());
        assert!(!receipt.manifest.execution_restored);
        let extracted = temp.path().join("extracted");
        fs::create_dir(&extracted).unwrap();
        tar::Archive::new(File::open(out).unwrap())
            .unpack(&extracted)
            .unwrap();
        drop(store);
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(temp.path().join("state")).unwrap();
        let manifest: ExportManifest =
            serde_json::from_slice(&fs::read(extracted.join("manifest.json")).unwrap()).unwrap();
        for file in &manifest.files {
            let bytes = fs::read(extracted.join("brain").join(&file.path)).unwrap();
            assert_eq!(
                format!("sha256:{:x}", Sha256::digest(&bytes)),
                file.revision
            );
            assert_eq!(
                bytes,
                originals.iter().find(|(p, _)| *p == file.path).unwrap().1
            );
        }
        assert!(!extracted.join("brain/.env").exists());
        assert!(!extracted.join("brain/credentials.json").exists());
        assert!(!extracted.join("brain/.tessera-index").exists());
        assert!(crate::Vault::scan(&extracted.join("brain")).is_ok());
    }
    #[test]
    fn mutation_addition_and_removal_fail_without_publishing_partial_archive() {
        for change in 0..3 {
            let (temp, store) = setup();
            let root = temp.path().join("brain");
            write(&root, "note.md", b"original");
            let out = temp.path().join("out.tar");
            let mut fired = false;
            let err = store
                .export_with_hook(&out, || {
                    fired = true;
                    match change {
                        0 => write(&root, "note.md", b"modified"),
                        1 => write(&root, "another.md", b"new"),
                        _ => fs::remove_file(root.join("note.md")).unwrap(),
                    }
                    Ok(())
                })
                .unwrap_err();
            assert!(fired);
            assert!(err.to_string().contains("changed during export"));
            assert!(!out.exists());
            // Positive control: retry captures the new stable state.
            assert!(store.export_exact(&out).is_ok());
        }
    }
    #[test]
    fn external_and_unresolved_dependencies_are_reported_without_importing_them() {
        let (temp, store) = setup();
        let root = temp.path().join("brain");
        write(temp.path(), "outside.md", b"outside");
        write(&root, "note.md", b"[outside](../outside.md)\n[missing](missing.pdf)\n![](escape/outside.md)\n[[duplicate]]\n[remote](https://example.com/a?token=hidden)\n`[[code]]`\n");
        write(
            &root,
            "a/duplicate.md",
            b"---\nrelated_to: \"[[missing-relation]]\"\n---\n[[duplicate]]\n[[NOTE]]",
        );
        write(&root, "b/duplicate.md", b"b");
        symlink(temp.path(), root.join("escape")).unwrap();
        let manifest = store
            .export_exact(&temp.path().join("out.tar"))
            .unwrap()
            .manifest;
        assert_eq!(manifest.files.len(), 3);
        assert!(manifest
            .exclusions
            .iter()
            .any(|e| e.path == "escape" && e.reason == "symlink_not_followed"));
        for (target, status) in [
            ("../outside.md", "external"),
            ("missing.pdf", "unresolved"),
            ("escape/outside.md", "unresolved"),
            ("duplicate", "ambiguous"),
        ] {
            assert!(manifest
                .dependencies
                .iter()
                .any(|d| d.target == target && d.status == status));
        }
        assert_eq!(manifest.dependencies.len(), 6);
        assert!(manifest
            .dependencies
            .iter()
            .any(|d| d.source == "a/duplicate.md"
                && d.target == "duplicate"
                && d.status == "ambiguous"));
        assert!(manifest
            .dependencies
            .iter()
            .any(|d| d.target == "missing-relation" && d.status == "unresolved"));
        assert!(!serde_json::to_string(&manifest)
            .unwrap()
            .contains("token=hidden"));
    }
    #[test]
    fn symlink_swap_never_gets_read_or_published() {
        let (temp, store) = setup();
        let root = temp.path().join("brain");
        write(&root, "note.md", b"inside");
        write(temp.path(), "outside.md", b"outside");
        let out = temp.path().join("out.tar");
        assert!(store
            .export_with_hook(&out, || {
                fs::remove_file(root.join("note.md"))?;
                symlink(temp.path().join("outside.md"), root.join("note.md"))?;
                Ok(())
            })
            .is_err());
        assert!(!out.exists());
    }
    // rustix exposes mkfifoat on Linux, but not on macOS. Keep the symlink
    // substitution regression above available on every supported Unix target.
    #[cfg(target_os = "linux")]
    #[test]
    fn special_file_never_gets_read_or_published() {
        let (temp, store) = setup();
        let root = temp.path().join("brain");
        write(&root, "note.md", b"inside");
        let out = temp.path().join("out.tar");
        rustix::fs::mkfifoat(rustix::fs::CWD, root.join("pipe"), Mode::RUSR | Mode::WUSR).unwrap();
        assert!(store
            .export_exact(&out)
            .unwrap_err()
            .to_string()
            .contains("non-regular"));
        assert!(!out.exists());
    }
    #[test]
    fn outside_destination_and_no_clobber_are_enforced_even_through_parent_symlink() {
        let (temp, store) = setup();
        let root = temp.path().join("brain");
        write(&root, "note.md", b"note");
        symlink(&root, temp.path().join("alias")).unwrap();
        for out in [
            root.join("export.tar"),
            temp.path().join("alias/export.tar"),
        ] {
            assert!(store
                .export_exact(&out)
                .unwrap_err()
                .to_string()
                .contains("outside"));
        }
        let out = temp.path().join("existing.tar");
        fs::write(&out, b"original archive").unwrap();
        assert!(store.export_exact(&out).is_err());
        assert_eq!(fs::read(out).unwrap(), b"original archive");
    }
    // A concurrent fork can retain the flock open-file description until exec,
    // even though SourceStore opens it with CLOEXEC. Availability is a bounded
    // blocking assertion; exclusion inside the export remains an immediate probe.
    fn pending_lock_probe(path: &Path) -> std::sync::mpsc::Receiver<std::io::Result<File>> {
        let path = path.to_owned();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = File::open(path).and_then(|file| {
                file.lock()?;
                Ok(file)
            });
            let _ = sender.send(result);
        });
        receiver
    }

    fn available_lock(receiver: std::sync::mpsc::Receiver<std::io::Result<File>>) -> File {
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("writer lock must become available after its owner releases it")
            .expect("lock probe must open and acquire the writer lock")
    }

    #[test]
    fn export_lock_positive_control_waits_for_inherited_pre_exec_descriptor() {
        // Keep the deliberately paused pre-exec child from inheriting locks
        // owned by other tests in the parallel test harness.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "export::tests::inherited_export_lock_probe_process",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "isolated subprocess inheritance probe"]
    fn inherited_export_lock_probe_process() {
        use std::os::unix::{net::UnixStream, process::CommandExt};

        let (temp, store) = setup();
        let lock = store.lock().unwrap();
        let lock_path = temp.path().join("state/writer.lock");
        let (mut parent, child) = UnixStream::pair().unwrap();
        parent
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut command = std::process::Command::new("/usr/bin/true");
        // SAFETY: the child only performs read/write syscalls before exec. The
        // fixed-byte barrier neither allocates nor acquires a userspace lock.
        unsafe {
            command.pre_exec(move || {
                rustix::io::write(&child, b"R")?;
                let mut release = [0];
                if rustix::io::read(&child, &mut release)? != 1 || release != *b"X" {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                Ok(())
            });
        }
        let process = std::thread::spawn(move || command.spawn()?.wait());
        let mut ready = [0];
        let readiness = parent.read_exact(&mut ready);
        drop(lock);
        // The child deliberately still holds the inherited descriptor. This is
        // the old immediate positive probe's false failure, with no timing race.
        let immediate = File::open(&lock_path).unwrap().try_lock();
        let pending = pending_lock_probe(&lock_path);
        let while_inherited = pending.try_recv();
        // Release and reap the child before assertions so failed checks cannot
        // leave a deliberately blocked pre-exec process behind.
        let release = parent.write_all(b"X");
        let status = process.join().unwrap().unwrap();
        readiness.unwrap();
        release.unwrap();
        assert_eq!(ready, *b"R");
        assert!(status.success());
        assert!(matches!(immediate, Err(std::fs::TryLockError::WouldBlock)));
        assert!(matches!(
            while_inherited,
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        drop(available_lock(pending));
    }

    #[test]
    fn managed_writer_lock_covers_both_export_inventories() {
        let (temp, store) = setup();
        write(&temp.path().join("brain"), "note.md", b"saved");
        let probe = || File::open(temp.path().join("state/writer.lock")).unwrap();
        let lock_path = temp.path().join("state/writer.lock");
        drop(available_lock(pending_lock_probe(&lock_path)));
        let mut checked = false;
        store
            .export_with_hook(&temp.path().join("out.tar"), || {
                checked = true;
                assert!(matches!(
                    probe().try_lock(),
                    Err(std::fs::TryLockError::WouldBlock)
                ));
                Ok(())
            })
            .unwrap();
        assert!(checked);
        drop(available_lock(pending_lock_probe(&lock_path)));
    }

    #[test]
    fn client_download_checks_hash_and_length_before_atomic_local_publication() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"portable archive bytes";
        let revision = format!("sha256:{:x}", Sha256::digest(bytes));
        let out = temp.path().join("download.tar");
        save_download(&out, bytes.len() as u64, &revision, |offset| {
            Ok(bytes[offset as usize..].iter().take(5).copied().collect())
        })
        .unwrap();
        assert_eq!(fs::read(&out).unwrap(), bytes);
        assert!(
            save_download(&out, bytes.len() as u64, &revision, |_| panic!(
                "existing target must not start transfer"
            ))
            .is_err()
        );
        for failure in 0..4 {
            let out = temp.path().join(format!("failed-{failure}.tar"));
            let result = save_download(&out, bytes.len() as u64, &revision, |_| match failure {
                0 => bail!("disconnected"),
                1 => Ok(Vec::new()),
                2 => Ok(vec![1; bytes.len()]),
                _ => Ok(vec![1; bytes.len() + 1]),
            });
            assert!(result.is_err());
            assert!(!out.exists());
        }
    }
}
