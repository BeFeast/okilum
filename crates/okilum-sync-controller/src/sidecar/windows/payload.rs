//! Pin executable sections independently of the Authenticode certificate table.
//! Re-signing the exact upstream v2.1.6 PE cannot substitute another signed app.
use super::{
    super::supervisor::ipc::windows_discovery::ImagePolicy, signature::AuthenticodePolicy,
};
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{fs::OpenOptions, io::Read, os::windows::fs::OpenOptionsExt, path::Path};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
const BODY_SIZE: usize = 27_432_960;
const BODY_HASH: &str = "3e348ea195ff81fc3b23f6a261afcada8a8e007992ddbab88629344273ae89fc";
pub const ARCHIVE_HASH: &str = "589098269cabb9a5a1703a934305b4153609d7623e978e1c94f0f49d77ae5e9f";
pub const UPSTREAM_HASH: &str = "e33d934db704c9a3c7c46488d34ba655d676ac17234f01f3025740ffa4655dae";

pub fn verify(path: &Path, policy: &AuthenticodePolicy) -> Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    okilum_write_authority::filesystem_identity::from_file(&file)?;
    ensure!(
        (BODY_SIZE as u64..=100_000_000).contains(&file.metadata()?.len()),
        "unsupported Windows Sync payload size"
    );
    policy.verify_image(path)?;
    let mut bytes = Vec::new();
    (&mut file).take(100_000_001).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 100_000_000 && bytes.starts_with(b"MZ"),
        "unsupported Windows Sync payload"
    );
    let u16_at = |offset: usize| -> Result<u16> {
        Ok(u16::from_le_bytes(
            bytes
                .get(offset..offset + 2)
                .context("truncated PE header")?
                .try_into()?,
        ))
    };
    let u32_at = |offset: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(
            bytes
                .get(offset..offset + 4)
                .context("truncated PE header")?
                .try_into()?,
        ))
    };
    let pe = u32_at(0x3c)? as usize;
    let optional = pe.checked_add(24).context("invalid PE offset")?;
    ensure!(
        pe <= BODY_SIZE - 264
            && bytes.get(pe..pe + 4) == Some(b"PE\0\0")
            && u16_at(pe + 4)? == 0x8664
            && u16_at(pe + 20)? >= 240
            && u16_at(optional)? == 0x20b,
        "unsupported Windows Sync PE architecture"
    );
    let certificate = u32_at(optional + 144)? as usize;
    let certificate_size = u32_at(optional + 148)? as usize;
    ensure!(
        certificate == BODY_SIZE
            && certificate_size >= 8
            && certificate.checked_add(certificate_size) == Some(bytes.len()),
        "unsupported Windows Sync PE certificate layout"
    );
    let packaged_hash = format!("{:x}", Sha256::digest(&bytes));
    bytes.truncate(BODY_SIZE);
    bytes[optional + 64..optional + 68].fill(0);
    bytes[optional + 144..optional + 152].fill(0);
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == BODY_HASH,
        "Windows Sync runtime differs from the pinned upstream executable body"
    );
    Ok(packaged_hash)
}
