//! Linux mountinfo classification, independent of filesystem I/O for testing.
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

fn mount_path(field: &[u8]) -> Option<PathBuf> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\' {
            let escaped = field.get(i + 1..i + 4)?;
            decoded.push(match escaped {
                b"040" => b' ',
                b"011" => b'\t',
                b"012" => b'\n',
                b"134" => b'\\',
                _ => return None,
            });
            i += 4;
        } else {
            decoded.push(field[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(decoded)))
}

pub(super) fn remote_fuse_mount(root: &Path, info: &[u8]) -> bool {
    let mut selected: Option<(usize, bool)> = None;
    for line in info.split(|b| *b == b'\n') {
        let Some(separator) = line.windows(3).position(|s| s == b" - ") else {
            continue;
        };
        let Some(field) = line[..separator].split(|b| *b == b' ').nth(4) else {
            continue;
        };
        let Some(mount) = mount_path(field).filter(|p| p.is_absolute()) else {
            continue;
        };
        if !root.starts_with(&mount) {
            continue;
        }
        let fs_type = line[separator + 3..]
            .split(|b| *b == b' ')
            .next()
            .unwrap_or_default();
        let depth = mount.components().count();
        if selected.is_none_or(|(old_depth, _)| depth >= old_depth) {
            selected = Some((depth, matches!(fs_type, b"fuse.sshfs" | b"fuse.rclone")));
        }
    }
    selected.is_some_and(|(_, remote)| remote)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_subtypes_use_longest_component_prefix_and_decode_mount_names() {
        let info = b"1 0 8:1 / / rw - ext4 /dev/root rw\n2 1 0:1 / /home/cloud\\040vault rw shared:1 - fuse.sshfs host:/ rw\n3 2 0:2 / /home/cloud\\040vault/local rw - fuse.gocryptfs local rw\n4 1 0:3 / /other rw - fuse.rclone remote: rw\n";
        assert!(remote_fuse_mount(
            Path::new("/home/cloud vault/nested/vault"),
            info
        ));
        assert!(remote_fuse_mount(Path::new("/other"), info));
        assert!(!remote_fuse_mount(
            Path::new("/home/cloud vault/local/vault"),
            info
        ));
        assert!(!remote_fuse_mount(
            Path::new("/home/cloud vault-other"),
            info
        ));
        assert!(!remote_fuse_mount(Path::new("/local"), info));
    }

    #[test]
    fn local_unknown_and_malformed_fuse_entries_do_not_disable_watching() {
        for fs in [
            "fuseblk",
            "fuse.gocryptfs",
            "fuse.mergerfs",
            "fuse.unknown",
            "fuse",
        ] {
            let info = format!("1 0 0:1 / /vault rw - {fs} source rw\n");
            assert!(!remote_fuse_mount(Path::new("/vault/sub"), info.as_bytes()));
        }
        for info in [
            b"invalid".as_slice(),
            b"1 0 0:1 / /vault\\999 rw - fuse.sshfs source rw",
            b"1 0 0:1 / /vault\\ rw - fuse.sshfs source rw",
        ] {
            assert!(!remote_fuse_mount(Path::new("/vault"), info));
        }
        assert_eq!(
            mount_path(b"/a\\011b\\012c\\134d").unwrap(),
            Path::new("/a\tb\nc\\d")
        );
    }
}
