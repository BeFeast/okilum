//! Upgrade private admission metadata only; never rebind or change canonical data.
use super::*;

impl Authority {
    /// Returns the held `registry.lock` when it scanned the records, so the
    /// registration can keep it, and what this scan read, for its own scan.
    pub(super) fn migrate_legacy_bindings(
        account: &Path,
        requested: &Path,
        index: &mut registry_index::RecordIndex,
    ) -> Result<Option<File>> {
        // Normal admissions must not take the account-wide registration lock.
        // The outermost existing scope is the one existing() will select.
        for ancestor in requested.ancestors().collect::<Vec<_>>().into_iter().rev() {
            let path = account.join(format!("{}.json", key(ancestor)));
            if path.try_exists()? {
                if read::<Binding>(&path)?.schema == BINDING_SCHEMA {
                    return Ok(None);
                }
                break;
            }
        }
        let registry = file(&account.join("registry.lock"), true)?;
        registry
            .lock()
            .context("Cannot serialize writer identity migration")?;
        for entry in fs::read_dir(account)? {
            let Some((path, binding)) = Self::legacy_record(&entry?, index)? else {
                continue;
            };
            let relevant =
                requested.starts_with(&binding.root) || binding.root.starts_with(requested);
            let authority = Self {
                requested: None,
                directory: account.join(key(&binding.root)),
                account: account.to_owned(),
                binding_path: path,
                binding,
            };
            // Whatever it writes, this record is no longer what this hold read.
            index.rewritten(&authority.binding_path);
            let result = authority.migrate_legacy_binding();
            // An unrelated offline/replaced/busy volume cannot prevent opening
            // this vault. Registry alias checks still retain its durable fences.
            if relevant {
                result?;
            }
        }
        Ok(Some(registry))
    }

    /// The record at `entry` when it is a schema 1 record to migrate. Every
    /// record is read exactly as before, with the same errors; a record whose
    /// bytes the index knows not to be schema 1 is not parsed again (#3272).
    pub(super) fn legacy_record(
        entry: &fs::DirEntry,
        index: &mut registry_index::RecordIndex,
    ) -> Result<Option<(PathBuf, Binding)>> {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            return Ok(None);
        }
        let (bytes, digest) = registry_index::read_record(&path)?;
        let name = entry.file_name();
        if index
            .get(&name, &digest)
            .is_some_and(|summary| summary.schema != 1)
        {
            return Ok(None);
        }
        let binding: Binding = parse(&bytes)?;
        index.learn(name, digest, &binding);
        Ok((binding.schema == 1).then_some((path, binding)))
    }

    fn migrate_legacy_binding(&self) -> Result<()> {
        let admission = file(&self.directory.join("admission.lock"), false)?;
        admission
            .lock()
            .context("Cannot serialize writer identity migration")?;
        ensure!(
            read::<Binding>(&self.binding_path)? == self.binding,
            "Writer binding changed during identity migration; retry"
        );
        let writers = file(&self.directory.join("writers.lock"), false)?;
        writers
            .try_lock()
            .context("A writer is still active; retry opening the vault after it finishes")?;
        let owner = file(&self.directory.join("owner.lock"), false)?;
        owner
            .try_lock()
            .context("A writer hold is still active; retry opening the vault after it finishes")?;
        let root_identity = identity(&self.binding.root)?;
        if self.binding.root.canonicalize()? != self.binding.root
            || !self.binding.root_identity.legacy_matches(&root_identity)
        {
            return Err(RootReplaced {
                root: self.binding.root.clone(),
            }
            .into());
        }
        let directory = identity(&self.directory)?;
        let admission_identity = identity(&self.directory.join("admission.lock"))?;
        let writers_identity = identity(&self.directory.join("writers.lock"))?;
        let owner_identity = identity(&self.directory.join("owner.lock"))?;
        ensure!(
            self.binding.directory.legacy_matches(&directory)
                && self.binding.admission.legacy_matches(&admission_identity)
                && self.binding.writers.legacy_matches(&writers_identity)
                && self.binding.owner.legacy_matches(&owner_identity),
            "Writer authority identity replaced; recovery required"
        );
        #[cfg(target_os = "linux")]
        ensure!(
            identity_linux::legacy_filesystems_match(&[
                (&self.binding.root_identity, &root_identity),
                (&self.binding.directory, &directory),
                (&self.binding.admission, &admission_identity),
                (&self.binding.writers, &writers_identity),
                (&self.binding.owner, &owner_identity),
            ]),
            "Writer authority filesystem changed; recovery required"
        );
        ensure!(
            !self.account.starts_with(&self.binding.root),
            "Authority is inside canonical vault"
        );
        self.state()?; // prove epoch/phase consistency; never reset state or release a hold
        let upgraded = Binding {
            schema: BINDING_SCHEMA,
            root_identity,
            directory,
            admission: admission_identity,
            writers: writers_identity,
            owner: owner_identity,
            ..self.binding.clone()
        };
        persist(&self.binding_path, &upgraded, false)?;
        Ok(())
    }
}
