//! Native NTFS publication retains displaced inodes and uses the Reader's
//! no-replace primitives and source DACL/EFS preparation, never blind Replace.
use super::*;
use okilum_core::windows_files::Directory;
use std::{ffi::OsStr, fs::File};

fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}
fn read_file(mut file: File) -> Result<Vec<u8>> {
    okilum_write_authority::filesystem_identity::from_file(&file)?;
    let info = okilum_core::windows_files::information(&file)?;
    ensure!(
        info.nNumberOfLinks == 1 && file.metadata()?.len() <= MAX_BYTES as u64,
        "unsafe retained first Sync preimage"
    );
    let mut bytes = vec![];
    (&mut file)
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_BYTES,
        "retained first Sync file exceeds bound"
    );
    Ok(bytes)
}
fn parent(root: &Directory, path: &str, create: bool) -> Result<(Directory, String)> {
    tree::validate_path(path)?;
    let mut parts = path.split('/').peekable();
    let mut directory = tree::open_root(root.path())?;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return Ok((directory, part.into()));
        }
        let child = directory.child(OsStr::new(part))?;
        directory = match tree::open_root(&child) {
            Ok(child) => child,
            Err(error) if create && missing(&error) => {
                directory.create_directory(OsStr::new(part))?;
                tree::open_root(&child)?
            }
            Err(error) => return Err(error),
        };
    }
    anyhow::bail!("empty merge file path")
}
pub(super) fn read_retained(root: &Directory, path: &str, retained: &str) -> Result<Vec<u8>> {
    let (directory, _) = parent(root, path, false)?;
    read_file(directory.open_file(OsStr::new(retained))?)
}
impl Store {
    pub fn publish(&mut self, validate: impl Fn() -> Result<()>) -> Result<()> {
        ensure!(
            matches!(
                self.binding.phase,
                Phase::Prepared | Phase::Publishing | Phase::Published
            ),
            "first Sync not prepared"
        );
        validate()?;
        self.compatible(self.binding.phase != Phase::Prepared)?;
        self.binding.phase = Phase::Publishing;
        self.save()?;
        let plan = self.plan()?;
        let root = tree::open_root(&self.binding.source.path)?;
        let originals = if self.binding.remote_side {
            &plan.originals().1.files
        } else {
            &plan.originals().0.files
        };
        for (path, bytes) in plan.merged_files() {
            validate()?;
            tree::verify_root(&root, &self.binding.source.path)?;
            let (directory, name) = parent(&root, path, true)?;
            let retained = self.retained(path);
            let source = match directory.open_file(OsStr::new(&name)) {
                Ok(file) => {
                    let current = read_file(file)?;
                    if current == *bytes {
                        continue;
                    }
                    ensure!(
                        originals.get(path) == Some(&current),
                        "third version stops first Sync publication"
                    );
                    directory.rename(
                        OsStr::new(&name),
                        &directory,
                        OsStr::new(&retained),
                        &current,
                    )?;
                    checkpoint(
                        if self.binding.remote_side {
                            "source-captured"
                        } else {
                            "destination-captured"
                        },
                        self.binding.operation,
                    );
                    #[cfg(feature = "first-sync-fixture")]
                    if self.binding.remote_side && path == "different.md" {
                        checkpoint("source-note-captured", self.binding.operation);
                    }
                    ensure!(
                        read_retained(&root, path, &retained)? == current,
                        "writer changed captured first Sync preimage; recovery required"
                    );
                    Some(OsStr::new(&retained))
                }
                Err(error) if missing(&error) => match directory.open_file(OsStr::new(&retained)) {
                    Ok(file) => {
                        let captured = read_file(file)?;
                        ensure!(
                            originals.get(path) == Some(&captured),
                            "third version in retained first Sync preimage; recovery stopped"
                        );
                        Some(OsStr::new(&retained))
                    }
                    Err(error) if missing(&error) => None,
                    Err(error) => return Err(error),
                },
                Err(error) => return Err(error),
            };
            // Directory pins every ancestor; the staged bytes inherit exactly
            // the captured source DACL/encryption before no-replace publication.
            directory.create_preserving(OsStr::new(&name), bytes, source)?;
        }
        validate()?;
        ensure!(
            Tree::collect(&root, &self.binding.source.path, true)?.files == *plan.merged_files(),
            "first Sync readback differs; recovery required"
        );
        self.binding.phase = Phase::Published;
        self.save()?;
        checkpoint(
            if self.binding.remote_side {
                "source-published"
            } else {
                "destination-published"
            },
            self.binding.operation,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_header(peer: Uuid) -> Result<Header> {
        Ok(serde_json::from_value(json!({
            "auth":"fixture", "id":Uuid::new_v4(), "revision":"fixture", "manifest":"fixture", "files":3, "bytes":20,
            "binding": {"pair":peer,"owner":"source","reader":"destination","vault":"fixture","remote":true,
                "root_binding":"fixture","device":"fixture","certificate":"fixture","config":"fixture",
                "generation":Uuid::new_v4(),"started":"fixture","policy":"fixture"}
        }))?)
    }
    fn prepared(root: &Path) -> Result<(Store, merge_preview::Plan)> {
        let here = merge_preview::Snapshot {
            scope: "here".into(),
            revision: "here-revision".into(),
            files: BTreeMap::from([
                ("different.md".into(), b"destination\r\n".to_vec()),
                ("local.dat".into(), vec![0xff; 12345]),
                ("same.md".into(), b"equal".to_vec()),
            ]),
        };
        let there = merge_preview::Snapshot {
            scope: "there".into(),
            revision: "there-revision".into(),
            files: BTreeMap::from([
                ("different.md".into(), b"source\n".to_vec()),
                ("remote.dat".into(), vec![0x93; 12345]),
                ("same.md".into(), b"equal".to_vec()),
            ]),
        };
        let vault = root.join("vault");
        std::fs::create_dir(&vault)?;
        for (name, bytes) in &here.files {
            std::fs::write(vault.join(name), bytes)?;
        }
        let vault = vault.canonicalize()?;
        let state = root.join("operations");
        private::directory(&state)?;
        let peer = Uuid::new_v4();
        let operation = Uuid::new_v4();
        let header = fixture_header(peer)?;
        let authority = okilum_write_authority::Authority::for_root(&vault)?;
        let observed = Tree::collect(&tree::open_root(&vault)?, &vault, true)?;
        assert_eq!(observed.files, here.files);
        let revision = digest(&(&observed.stamps, manifest(&observed)))?;
        let mut here = here;
        here.revision = revision.clone();
        let plan = merge_preview::Plan::preview(here.clone(), there.clone()).unwrap();
        let metadata = Metadata {
            operation,
            peer,
            remote: header.clone(),
            created: 1,
            here: Image::from_snapshot(&here),
            there: Image::from_snapshot(&there),
            merged: Image::from_snapshot(&merge_preview::Snapshot {
                scope: "merged".into(),
                revision: "merged".into(),
                files: plan.merged_files().clone(),
            })
            .files,
        };
        let bytes = metadata.encoded()?;
        let root_identity = crate::direct::existing::VaultRoot::open(&vault)?;
        let source = ExistingVault {
            daemon: crate::daemon::DaemonIdentity {
                config_file: root.join("unused-config.xml"),
                device_id: "fixture".into(),
                certificate_hash: "fixture".into(),
            },
            version: crate::daemon::CLIENT_VERSION.into(),
            folder_id: "fixture".into(),
            path: vault,
            label: "Fixture".into(),
            paused: false,
            folder_type: "sendreceive".into(),
            peers: vec![],
            root_identity: Some(root_identity.identity()?),
            stable_root_identity: Some(root_identity.stable_identity()?),
        };
        let mut store = Store::create(
            &state,
            super::super::Binding {
                schema: 1,
                operation,
                peer,
                source,
                authority: authority.durable_binding()?,
                remote_side: false,
                revision,
                metadata_hash: wire::fingerprint(&bytes),
                metadata_size: bytes.len(),
                preview: header.clone(),
                own_inventory: header,
                phase: Phase::Staging,
            },
        )?;
        store.put(None, &bytes)?;
        for bytes in here
            .files
            .values()
            .chain(there.files.values())
            .chain(plan.merged_files().values())
        {
            store.put(Some(&wire::fingerprint(bytes)), bytes)?;
        }
        store.prepare()?;
        Ok((store, plan))
    }
    #[test]
    #[ignore = "requires the runner's explicitly isolated LOCALAPPDATA"]
    fn windows_native_publication_recovers_capture_gap_and_preserves_every_original() -> Result<()>
    {
        let isolated = PathBuf::from(
            std::env::var_os("OKILUM_WINDOWS_FIRST_SYNC_FIXTURE")
                .context("isolated fixture marker missing")?,
        );
        ensure!(
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from) == Some(isolated.clone()),
            "account state is not isolated"
        );
        for mode in ["normal", "captured", "third-version"] {
            let root = tempfile::tempdir_in(&isolated)?;
            let (mut store, plan) = prepared(root.path())?;
            let vault = store.binding.source.path.clone();
            if mode == "normal" {
                let revision = store.binding.revision.clone();
                store.binding.revision = "stale fixture revision".into();
                assert!(
                    store.publish(|| Ok(())).is_err(),
                    "unfired revision control"
                );
                assert_eq!(
                    Tree::collect(&tree::open_root(&vault)?, &vault, true)?.files,
                    plan.originals().0.files
                );
                store.binding.revision = revision;
            }
            if mode != "normal" {
                let directory = tree::open_root(&vault)?;
                let old = plan.originals().0.files.get("different.md").unwrap();
                directory.rename(
                    OsStr::new("different.md"),
                    &directory,
                    OsStr::new(&store.retained("different.md")),
                    old,
                )?;
                store.binding.phase = Phase::Publishing;
                store.save()?;
                // Reopen the durable transaction, without its in-memory caches.
                store = Store::open(store.directory.parent().unwrap(), store.binding.operation)?;
                if mode == "third-version" {
                    std::fs::write(vault.join("different.md"), b"unexpected third version")?;
                    assert!(store.publish(|| Ok(())).is_err());
                    assert_eq!(
                        std::fs::read(vault.join("different.md"))?,
                        b"unexpected third version"
                    );
                    assert_eq!(
                        read_retained(&directory, "different.md", &store.retained("different.md"))?,
                        *old
                    );
                    std::fs::remove_file(vault.join("different.md"))?;
                }
            }
            let calls = std::cell::Cell::new(0);
            store
                .publish(|| {
                    calls.set(calls.get() + 1);
                    Ok(())
                })
                .with_context(|| format!("native publication fixture {mode}"))?;
            assert!(
                calls.get() >= plan.merged_files().len() + 2,
                "publication must revalidate exclusion for every file"
            );
            let actual = Tree::collect(&tree::open_root(&vault)?, &vault, true)?.files;
            assert_eq!(&actual, plan.merged_files());
            for bytes in plan
                .originals()
                .0
                .files
                .values()
                .chain(plan.originals().1.files.values())
            {
                assert!(
                    actual.values().any(|value| value == bytes),
                    "original version lost"
                );
            }
            store.publish(|| Ok(()))?;
            assert_eq!(
                Tree::collect(&tree::open_root(&vault)?, &vault, true)?.files,
                actual
            );
        }
        Ok(())
    }
}
