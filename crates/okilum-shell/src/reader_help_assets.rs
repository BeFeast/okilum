//! Immutable offline Help bundles shared by contextual guides.
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

fn fingerprint(files: &[(&str, &[u8])]) -> String {
    let mut digest = Sha256::new();
    for (name, bytes) in files {
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}
fn real_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Help directory replaced"
    );
    Ok(())
}
fn verify_local(root: &Path, files: &[(&str, &[u8])]) -> anyhow::Result<PathBuf> {
    real_directory(root)?;
    real_directory(&root.join("assets"))?;
    for (name, bytes) in files {
        let path = root.join(name);
        let metadata = fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() == bytes.len() as u64,
            "Help asset replaced"
        );
        anyhow::ensure!(fs::read(path)? == *bytes, "Help asset changed");
    }
    Ok(root.join("index.html"))
}
/// Publish only immutable bundled assets outside vaults/indexes. Stage a complete
/// generation before making it visible; never overwrite a changed existing copy.
pub(crate) fn publish(
    state: &Path,
    topic: &str,
    files: &[(&str, &[u8])],
) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(state)?;
    real_directory(state)?;
    let help = state.join("help");
    match fs::create_dir(&help) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    real_directory(&help)?;
    let destination = help.join(format!("{topic}-{}", fingerprint(files)));
    match fs::symlink_metadata(&destination) {
        Ok(_) => return verify_local(&destination, files),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let temporary = tempfile::Builder::new()
        .prefix(".help-")
        .tempdir_in(&help)?;
    fs::create_dir(temporary.path().join("assets"))?;
    for (name, bytes) in files {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temporary.path().join(name))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        fs::File::open(temporary.path().join("assets"))?.sync_all()?;
        fs::File::open(temporary.path())?.sync_all()?;
    }
    if let Err(error) = fs::rename(temporary.path(), &destination) {
        // Another Help window may have published this exact generation first.
        // Only a byte-verified copy can win that race.
        if verify_local(&destination, files).is_err() {
            return Err(error.into());
        }
    }
    #[cfg(unix)]
    fs::File::open(help)?.sync_all()?;
    verify_local(&destination, files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    const FILES: &[(&str, &[u8])] = &[("index.html", b"guide"), ("assets/video.mp4", b"video")];

    #[test]
    fn immutable_help_reuses_only_verified_bytes_and_separates_topics() {
        let state = tempfile::tempdir().unwrap();
        let first = publish(state.path(), "board", FILES).unwrap();
        assert_eq!(publish(state.path(), "board", FILES).unwrap(), first);
        assert_ne!(publish(state.path(), "sync", FILES).unwrap(), first);
        fs::write(&first, b"other").unwrap();
        assert!(publish(state.path(), "board", FILES).is_err());
        assert_eq!(fs::read(first).unwrap(), b"other");
    }

    #[cfg(unix)]
    #[test]
    fn immutable_help_refuses_a_replaced_asset_directory() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let page = publish(state.path(), "board", FILES).unwrap();
        let assets = page.parent().unwrap().join("assets");
        fs::remove_file(assets.join("video.mp4")).unwrap();
        fs::remove_dir(&assets).unwrap();
        std::os::unix::fs::symlink(outside.path(), assets).unwrap();
        assert!(publish(state.path(), "board", FILES).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
