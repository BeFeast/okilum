//! Explicit local capture into an existing, user-selected vault folder.
//! Operational records belong outside the disposable index. No reader protocol IO.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receipt {
    pub operation_id: String,
    pub path: PathBuf,
}

/// One authored capture. Keep this value on failure: a new attempt must not
/// silently allocate another destination after publication became uncertain.
#[derive(Debug, Serialize, Deserialize)]
pub struct Capture {
    receipt: Receipt,
    text: String,
}

impl Capture {
    pub fn new(folder: &Path, text: String) -> Result<Self> {
        if text.trim().is_empty() {
            bail!("Enter capture text before saving");
        }
        if folder
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            bail!("Choose a capture folder inside the vault");
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        let path = folder.join(format!("Capture-{operation_id}.md"));
        if crate::vault::service_path(&path) {
            bail!("Choose a capture folder outside service files");
        }
        Ok(Self {
            receipt: Receipt { operation_id, path },
            text,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The caller supplies durable per-vault operational storage, never an index
    /// directory. A retained intent blocks blind retries following an IO error.
    pub fn publish(&self, root: &Path, drafts: &Path, records: &Path) -> Result<Receipt> {
        let _reservation = crate::file_editor::FileEditor::reserve_destination(
            &root.join(&self.receipt.path),
            drafts,
        )?;
        std::fs::create_dir_all(records)?;
        persist_new(records, &format!("{}.intent.json", self.receipt.operation_id), self)
            .context("Capture intent already exists or cannot be saved; retain the draft and recover this attempt")?;
        crate::note_files::create_with_source(root, &self.receipt.path, self.text.as_bytes())
            .context("Capture was not acknowledged; retain the draft and inspect the saved intent before retrying")?;
        persist_new(records, &format!("{}.receipt.json", self.receipt.operation_id), &self.receipt)
            .context("Note exists, but its receipt could not be acknowledged; retain the draft and recover this attempt")?;
        Ok(self.receipt.clone())
    }
}

fn persist_new(directory: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    use std::io::Write;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&serde_json::to_vec(value)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(directory.join(name))?;
    #[cfg(unix)]
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_text_receipt_and_no_overwrite() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::create_dir(vault.path().join("Inbox")).unwrap();
        let capture = Capture::new(Path::new("Inbox"), "\u{feff}Original\r\ntext".into()).unwrap();
        let receipt = capture
            .publish(
                vault.path(),
                &state.path().join("drafts"),
                &state.path().join("captures"),
            )
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(vault.path().join(&receipt.path)).unwrap(),
            capture.text()
        );
        let saved: Receipt = serde_json::from_slice(
            &std::fs::read(
                state
                    .path()
                    .join("captures")
                    .join(format!("{}.receipt.json", receipt.operation_id)),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(saved, receipt);
        std::fs::write(vault.path().join(&receipt.path), "external edit").unwrap();
        assert!(capture
            .publish(
                vault.path(),
                &state.path().join("drafts"),
                &state.path().join("captures")
            )
            .is_err());
        assert_eq!(
            std::fs::read_to_string(vault.path().join(receipt.path)).unwrap(),
            "external edit"
        );
        assert_eq!(capture.text(), "\u{feff}Original\r\ntext");
    }

    #[test]
    fn chosen_folder_only_and_collision_preserves_both_sources() {
        let vault = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        assert!(Capture::new(Path::new("../outside"), "text".into()).is_err());
        assert!(Capture::new(Path::new(""), " \n".into()).is_err());
        let capture = Capture::new(Path::new(""), "authored".into()).unwrap();
        std::fs::write(vault.path().join(&capture.receipt.path), "existing").unwrap();
        assert!(capture
            .publish(
                vault.path(),
                &state.path().join("drafts"),
                &state.path().join("captures")
            )
            .is_err());
        assert_eq!(
            std::fs::read_to_string(vault.path().join(&capture.receipt.path)).unwrap(),
            "existing"
        );
        assert_eq!(capture.text(), "authored");
    }
}
