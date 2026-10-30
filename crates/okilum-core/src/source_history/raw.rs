//! Additive byte-exact encoding recovery in the existing History store.
//! Legacy builds enumerate only `.json`; they skip this record kind entirely.
use super::*;
use std::io::Read;

const EXTENSION: &str = "raw-history";
#[cfg(test)]
thread_local! { static FAIL_COMPLETE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RawRecord {
    schema: u32,
    pub note: PathBuf,
    pub created: u64,
    #[serde(with = "raw_bytes")]
    pub before: Vec<u8>,
    #[serde(with = "raw_bytes")]
    pub after: Vec<u8>,
    pub displaced: PathBuf,
    pub prepared: Option<PathBuf>,
    pub completed: bool,
}

impl RawRecord {
    pub(crate) fn is_path(path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension == EXTENSION)
    }

    pub(crate) fn begin(
        drafts: &Path,
        note: &Path,
        before: &[u8],
        after: &[u8],
        displaced: &Path,
        prepared: Option<&Path>,
    ) -> Result<PathBuf> {
        ensure!(note.is_absolute(), "Invalid encoding recovery identity");
        ensure!(
            displaced.parent() == note.parent()
                && displaced
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".okilum-save-")),
            "Invalid encoding recovery preimage"
        );
        let record = Self {
            schema: 1,
            note: note.to_owned(),
            created: now(),
            before: before.to_vec(),
            after: after.to_vec(),
            displaced: displaced.to_owned(),
            prepared: prepared.map(Path::to_owned),
            completed: false,
        };
        let path = directory(drafts).join(format!("{}.{}", uuid::Uuid::new_v4(), EXTENSION));
        record.persist(&path)?;
        Ok(path)
    }

    pub(crate) fn load(path: &Path) -> Result<Self> {
        ensure!(Self::is_path(path), "Not an encoding History record");
        let file = fs::File::open(path)?;
        ensure!(
            file.metadata()?.len() <= MAX_BYTES as u64,
            "Encoding History record is too large"
        );
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_BYTES,
            "Encoding History record is too large"
        );
        let record: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            record.schema == 1 && record.note.is_absolute(),
            "Invalid encoding History record"
        );
        Ok(record)
    }

    fn persist(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec(self)?;
        ensure!(
            bytes.len() <= MAX_BYTES,
            "Encoding History exceeds recovery storage limit"
        );
        let parent = path.parent().context("Missing History folder")?;
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        crate::source_state::persist(path, &bytes)
    }

    #[cfg(test)]
    pub(crate) fn fail_next_complete() {
        FAIL_COMPLETE.with(|flag| flag.set(true));
    }

    pub(crate) fn complete(path: &Path) -> Result<()> {
        #[cfg(test)]
        if FAIL_COMPLETE.with(|flag| flag.replace(false)) {
            return Err(
                std::io::Error::other("injected History acknowledgement write failure").into(),
            );
        }
        let mut record = Self::load(path)?;
        record.completed = true;
        record.prepared = None;
        record.persist(path)
    }

    pub(crate) fn discard(path: &Path, note: &Path, before: &[u8], after: &[u8]) -> Result<()> {
        let record = Self::load(path)?;
        ensure!(
            record.note == note && record.before == before && record.after == after,
            "Encoding recovery changed; retained"
        );
        fs::remove_file(path)?;
        #[cfg(unix)]
        fs::File::open(path.parent().unwrap())?.sync_all()?;
        Ok(())
    }
}

pub(super) fn list(drafts: &Path, root: &Path) -> Result<Listing> {
    let mut result = Listing::default();
    let folder = directory(drafts);
    if !folder.exists() {
        return Ok(result);
    }
    let root = root.canonicalize()?;
    for item in fs::read_dir(folder)? {
        let path = item?.path();
        if !RawRecord::is_path(&path) {
            continue;
        }
        match RawRecord::load(&path) {
            Ok(record) if record.note.starts_with(&root) => {
                result.versions.push(Version {
                    note: record.note.clone(),
                    text: String::from_utf8_lossy(&record.before).into_owned(),
                    raw: Some(record.before),
                    created: record.created,
                    label: "Before encoding change · original bytes".into(),
                    protected: !record.completed || record.displaced.exists(),
                    link_move: false,
                });
                if !record.completed {
                    result.versions.push(Version {
                        note: record.note,
                        text: String::from_utf8_lossy(&record.after).into_owned(),
                        raw: Some(record.after),
                        created: record.created,
                        label: "Encoding change recovery · proposed bytes".into(),
                        protected: true,
                        link_move: false,
                    });
                }
            }
            Ok(_) => {}
            Err(error) => result
                .warnings
                .push(format!("Cannot read encoding History: {error:#}; retained")),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_history_is_additive_and_legacy_json_remains_identical() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let drafts = fixture.path().join("state/editor-drafts");
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let note = root.join("Synthetic.md");
        let before = b"# Synthetic\n\nBefore \xff After\n";
        fs::write(&note, before).unwrap();
        let displaced = root.join(".okilum-save-legacy");
        fs::write(&displaced, b"legacy text").unwrap();
        let legacy = Preimage::begin(&drafts, &note, "legacy text", &displaced).unwrap();
        let legacy_bytes = fs::read(&legacy).unwrap();
        let proposed = String::from_utf8_lossy(before);
        let raw = RawRecord::begin(
            &drafts,
            &note,
            before,
            proposed.as_bytes(),
            &root.join(".okilum-save-raw"),
            None,
        )
        .unwrap();
        assert_ne!(raw.extension(), legacy.extension());
        // This is the legacy list/prune admission rule: the raw record cannot
        // be mistaken for an older text preimage or break legacy enumeration.
        let legacy_paths: Vec<_> = fs::read_dir(directory(&drafts))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        assert_eq!(legacy_paths.as_slice(), std::slice::from_ref(&legacy));
        assert_eq!(Preimage::load(&legacy).unwrap().text, "legacy text");
        assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);
        let listing = super::list(&drafts, &root).unwrap();
        assert!(listing.warnings.is_empty());
        assert!(listing.versions.iter().any(|v| v.bytes() == before));
        assert_eq!(fs::read(&note).unwrap(), before);
        RawRecord::complete(&raw).unwrap();
        let listing = super::list(&drafts, &root).unwrap();
        assert_eq!(listing.versions.len(), 1);
        assert_eq!(listing.versions[0].bytes(), before);
        assert!(!listing.versions[0].protected);
    }
}

mod raw_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}
