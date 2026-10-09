//! Ephemeral exact-archive downloads. These handles never restore execution.
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use okilum_core::export::{ExportManifest, ExportReceipt};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const EXPORT_CHUNK_BYTES: usize = 1024 * 1024;
const LIFETIME: Duration = Duration::from_secs(30 * 60);
#[derive(Serialize)]
pub struct DownloadReady {
    pub export_id: String,
    pub bytes: u64,
    pub revision: String,
    pub manifest: ExportManifest,
}
#[derive(Serialize)]
pub struct DownloadChunk {
    pub export_id: String,
    pub offset: u64,
    pub next_offset: u64,
    pub eof: bool,
    pub content_base64: String,
}
struct Download {
    id: String,
    _directory: tempfile::TempDir,
    file: File,
    bytes: u64,
    created: Instant,
}
/// Only one retained download per backend. A second preparation explicitly
/// replaces its predecessor; expiry and backend exit remove all staging bytes.
#[derive(Default)]
pub struct ExportDownloads {
    current: Option<Download>,
}
impl ExportDownloads {
    pub fn prepare(
        &mut self,
        export: impl FnOnce(&Path) -> Result<ExportReceipt>,
    ) -> Result<DownloadReady> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("brain.tar");
        let receipt = export(&path)?;
        let mut file = File::open(path)?;
        let bytes = file.metadata()?.len();
        let mut hash = Sha256::new();
        let mut buffer = vec![0; EXPORT_CHUNK_BYTES];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        let revision = format!("sha256:{:x}", hash.finalize());
        let id = Uuid::new_v4().to_string();
        self.current = Some(Download {
            id: id.clone(),
            _directory: directory,
            file,
            bytes,
            created: Instant::now(),
        });
        Ok(DownloadReady {
            export_id: id,
            bytes,
            revision,
            manifest: receipt.manifest,
        })
    }
    pub fn chunk(&mut self, export_id: &str, offset: u64) -> Result<DownloadChunk> {
        if self
            .current
            .as_ref()
            .is_some_and(|d| d.created.elapsed() >= LIFETIME)
        {
            self.current = None;
        }
        let download = self
            .current
            .as_mut()
            .filter(|d| d.id == export_id)
            .context("export expired or unavailable; prepare a new archive")?;
        ensure!(
            offset <= download.bytes,
            "export offset exceeds archive size"
        );
        download.file.seek(SeekFrom::Start(offset))?;
        let wanted = (download.bytes - offset).min(EXPORT_CHUNK_BYTES as u64) as usize;
        let mut buffer = vec![0; wanted];
        download.file.read_exact(&mut buffer)?;
        let next_offset = offset + wanted as u64;
        Ok(DownloadChunk {
            export_id: export_id.into(),
            offset,
            next_offset,
            eof: next_offset == download.bytes,
            content_base64: STANDARD.encode(buffer),
        })
    }
    pub fn release(&mut self, export_id: &str) -> Result<()> {
        ensure!(
            self.current.as_ref().is_none_or(|d| d.id == export_id),
            "export identity does not match retained download"
        );
        self.current = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn prepare(downloads: &mut ExportDownloads, content: &[u8]) -> DownloadReady {
        downloads
            .prepare(|path| {
                fs::write(path, content)?;
                Ok(ExportReceipt {
                    destination: path.display().to_string(),
                    manifest: ExportManifest {
                        schema: okilum_core::export::EXPORT_SCHEMA.into(),
                        archive_root: "brain".into(),
                        execution_restored: false,
                        files: vec![],
                        exclusions: vec![],
                        dependencies: vec![],
                    },
                })
            })
            .unwrap()
    }
    #[test]
    fn bounded_repeatable_chunks_reassemble_exact_bytes_and_release_removes_staging() {
        let bytes: Vec<_> = (0..EXPORT_CHUNK_BYTES * 2 + 17)
            .map(|i| (i % 251) as u8)
            .collect();
        let mut downloads = ExportDownloads::default();
        let ready = prepare(&mut downloads, &bytes);
        let staging = downloads
            .current
            .as_ref()
            .unwrap()
            ._directory
            .path()
            .to_owned();
        assert_eq!(ready.bytes as usize, bytes.len());
        assert_eq!(
            ready.revision,
            format!("sha256:{:x}", Sha256::digest(&bytes))
        );
        let first = downloads.chunk(&ready.export_id, 0).unwrap();
        assert_eq!(
            first.content_base64,
            downloads.chunk(&ready.export_id, 0).unwrap().content_base64
        );
        assert!(downloads.chunk("wrong", 0).is_err());
        assert!(downloads.chunk(&ready.export_id, ready.bytes + 1).is_err());
        let mut reconstructed = Vec::new();
        let mut offset = 0;
        loop {
            let chunk = downloads.chunk(&ready.export_id, offset).unwrap();
            let decoded = STANDARD.decode(chunk.content_base64).unwrap();
            assert!(decoded.len() <= EXPORT_CHUNK_BYTES);
            assert_eq!(chunk.offset, offset);
            reconstructed.extend(decoded);
            offset = chunk.next_offset;
            if chunk.eof {
                break;
            }
        }
        assert_eq!(reconstructed, bytes);
        assert!(staging.exists());
        downloads.release(&ready.export_id).unwrap();
        assert!(!staging.exists());
        assert!(downloads.chunk(&ready.export_id, 0).is_err());
    }
    #[test]
    fn replacement_expiry_and_drop_remove_staging_without_changing_knowledge() {
        let mut downloads = ExportDownloads::default();
        let first = prepare(&mut downloads, b"first");
        let old = downloads
            .current
            .as_ref()
            .unwrap()
            ._directory
            .path()
            .to_owned();
        let next = prepare(&mut downloads, b"next");
        assert!(!old.exists());
        assert!(downloads.chunk(&first.export_id, 0).is_err());
        assert!(downloads.release(&first.export_id).is_err());
        let path = downloads
            .current
            .as_ref()
            .unwrap()
            ._directory
            .path()
            .to_owned();
        downloads.current.as_mut().unwrap().created = Instant::now() - LIFETIME;
        assert!(downloads.chunk(&next.export_id, 0).is_err());
        assert!(!path.exists());
        prepare(&mut downloads, b"drop");
        let path = downloads
            .current
            .as_ref()
            .unwrap()
            ._directory
            .path()
            .to_owned();
        drop(downloads);
        assert!(!path.exists());
    }
}
