//! Portable vault identity for external links (#1414, docs/deep-links.md).
//!
//! The same synced vault can have a different folder name on each machine, so
//! a copied `okilum://v/…` link also carries `vault_id`, read from
//! `.okilum/link-identity.json`. It is separate from `.okilum/vault.json` (the
//! display name), so renaming can never replace an identity.
//!
//! An explicit Copy is the only action that creates the file: opening a menu,
//! building one, resolving a link or reading the vault never writes. Existing
//! bytes (unknown fields, broken JSON, a directory in the way) are never
//! repaired, migrated or rewritten.
use anyhow::{bail, Context, Result};
use std::{io::Write, path::Path};

/// Vault-relative location of the identity file.
pub const FILE: &str = ".okilum/link-identity.json";

/// The vault's identity, `None` when it has none yet. Read-only.
pub fn read(root: &Path) -> Result<Option<String>> {
    let bytes = match std::fs::read(root.join(FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("The vault's link identity cannot be read"),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .context("The vault's link identity is damaged; it was left unchanged")?;
    let id = value
        .get("id")
        .and_then(|v| v.as_str())
        .context("The vault's link identity has no id; it was left unchanged")?;
    Ok(Some(
        uuid::Uuid::parse_str(id)
            .context("The vault's link identity is not valid; it was left unchanged")?
            .to_string(),
    ))
}

/// The identity for a Copy: the existing one, or a new one written once,
/// atomically and without replacing anything. Two Copies on this machine at
/// once agree on the winner. Two machines copying before the metadata syncs
/// can create competing ids; resolution then refuses the losing id and never
/// rebinds (decision 26, 2026-10-18).
pub fn for_copy(root: &Path) -> Result<String> {
    if let Some(id) = read(root)? {
        return Ok(id);
    }
    let destination = root.join(FILE);
    let directory = destination.parent().context("No metadata folder")?;
    let created_directory = match std::fs::create_dir(directory) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error).context("Cannot create the vault's metadata folder"),
    };
    let written = (|| -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let mut temporary = tempfile::NamedTempFile::new_in(directory)
            .context("Cannot save the vault's link identity")?;
        temporary.write_all(
            serde_json::to_string_pretty(&serde_json::json!({"version": 1, "id": id}))?.as_bytes(),
        )?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&destination) {
            Ok(_) => {
                #[cfg(unix)]
                std::fs::File::open(directory)?.sync_all()?;
                Ok(id)
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                read(root)?.context("The vault's link identity changed during Copy; try again")
            }
            Err(error) => bail!("Cannot save the vault's link identity: {}", error.error),
        }
    })();
    if written.is_err() && created_directory {
        // Leave the vault as it was: only the folder this attempt made, if empty.
        let _ = std::fs::remove_dir(directory);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_creates_once_and_preserves_all_metadata_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        assert_eq!(read(root).unwrap(), None);
        assert!(!root.join(".okilum").exists(), "reading never creates");
        let id = for_copy(root).unwrap();
        assert_eq!(read(root).unwrap().as_deref(), Some(id.as_str()));
        // Only the identity file, no temporary leftovers.
        let entries: Vec<_> = std::fs::read_dir(root.join(".okilum"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            entries,
            vec![std::ffi::OsString::from("link-identity.json")]
        );
        // Unknown fields written by a newer Okilum survive later Copies.
        let bytes = format!(r#"{{"id":"{id}","future":{{"color":"teal"}},"version":99}}"#);
        std::fs::write(root.join(FILE), &bytes).unwrap();
        assert_eq!(for_copy(root).unwrap(), id);
        assert_eq!(std::fs::read(root.join(FILE)).unwrap(), bytes.as_bytes());
        // The display-name metadata is a different file and stays untouched.
        std::fs::write(root.join(".okilum/vault.json"), r#"{"name":"Work"}"#).unwrap();
        assert_eq!(for_copy(root).unwrap(), id);
        assert_eq!(
            std::fs::read_to_string(root.join(".okilum/vault.json")).unwrap(),
            r#"{"name":"Work"}"#
        );
    }

    #[test]
    fn broken_unreadable_and_failed_creation_are_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir(root.join(".okilum")).unwrap();
        for bytes in ["", "broken", "[]", "{}", r#"{"id":"invalid","future":7}"#] {
            std::fs::write(root.join(FILE), bytes).unwrap();
            assert!(read(root).is_err(), "{bytes:?}");
            assert!(for_copy(root).is_err(), "{bytes:?}");
            assert_eq!(std::fs::read(root.join(FILE)).unwrap(), bytes.as_bytes());
        }
        // A directory where the file belongs.
        std::fs::remove_file(root.join(FILE)).unwrap();
        std::fs::create_dir(root.join(FILE)).unwrap();
        std::fs::write(root.join(FILE).join("keep"), "keep").unwrap();
        assert!(for_copy(root).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join(FILE).join("keep")).unwrap(),
            "keep"
        );
        // A file where the metadata folder belongs: refused, left as it was.
        std::fs::remove_dir_all(root.join(".okilum")).unwrap();
        std::fs::write(root.join(".okilum"), "blocked").unwrap();
        assert!(for_copy(root).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join(".okilum")).unwrap(),
            "blocked"
        );
        std::fs::remove_file(root.join(".okilum")).unwrap();
        assert!(
            for_copy(root).is_ok(),
            "positive control: nothing in the way"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_write_leaves_no_metadata_folder() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root ignores permissions; the control below still runs.
        let denied = std::fs::create_dir(root.join("probe")).is_err();
        if denied {
            assert!(for_copy(&root).is_err());
            assert!(!root.join(".okilum").exists());
        } else {
            std::fs::remove_dir(root.join("probe")).unwrap();
        }
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(for_copy(&root).is_ok(), "positive control: writable");
    }
}
