use anyhow::{ensure, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};
use uuid::Uuid;

pub(crate) fn directory(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir()
            && !m.file_type().is_symlink()
            && m.uid() == rustix::process::geteuid().as_raw()
            && m.mode() & 0o077 == 0,
        "private owned directory required"
    );
    Ok(())
}
pub(crate) fn read(path: &Path) -> Result<Vec<u8>> {
    read_file(path, true)
}
pub(crate) fn public_file(path: &Path) -> Result<Vec<u8>> {
    read_file(path, false)
}
fn read_file(path: &Path, secret: bool) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    let m = f.metadata()?;
    ensure!(
        m.is_file()
            && m.uid() == rustix::process::geteuid().as_raw()
            && (!secret || m.mode() & 0o077 == 0)
            && m.len() <= 4 * 1024 * 1024,
        "private regular file required"
    );
    let mut data = Vec::new();
    f.read_to_end(&mut data)?;
    Ok(data)
}
pub(crate) fn write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing parent"))?;
    let temporary = parent.join(format!(".sync-{}.tmp", Uuid::new_v4()));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    f.write_all(data)?;
    f.sync_all()?;
    fs::rename(temporary, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
pub(crate) fn lock(path: &Path) -> Result<File> {
    directory(path)?;
    let f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path.join("prepare.lock"))?;
    let m = f.metadata()?;
    ensure!(
        m.is_file()
            && m.uid() == rustix::process::geteuid().as_raw()
            && m.permissions().mode() & 0o077 == 0,
        "private owned lock required"
    );
    f.try_lock()?;
    Ok(f)
}
