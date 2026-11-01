//! Pure identity gate between the system-network feed and a local pending package.
//! Transport, checksum verification and atomic publication remain owned by Velopack.

#[derive(Clone, Copy)]
pub(super) struct Package<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub filename: &'a str,
    pub size: u64,
    pub sha256: &'a str,
}

pub(super) fn matches(pending: Package<'_>, offered: Package<'_>) -> bool {
    pending.id == offered.id
        && pending.version == offered.version
        && pending.filename == offered.filename
        && pending.size == offered.size
        && pending.sha256.len() == 64
        && pending.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        && pending.sha256.eq_ignore_ascii_case(offered.sha256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_feed_identity_is_required_even_with_matching_content() {
        let hash = "abcdef0123456789".repeat(4);
        let offered = Package {
            id: "BeFeast.Okilum",
            version: "1.2.3",
            filename: "BeFeast.Okilum-1.2.3-full.nupkg",
            size: 123,
            sha256: &hash,
        };
        // Positive control includes the upper-case digest returned by the file hasher.
        let upper = hash.to_ascii_uppercase();
        assert!(matches(
            Package {
                sha256: &upper,
                ..offered
            },
            offered
        ));
        for pending in [
            Package {
                id: "Other.App",
                ..offered
            },
            Package {
                version: "1.2.4",
                ..offered
            },
            Package {
                filename: "renamed.nupkg",
                ..offered
            },
            Package {
                size: 122,
                ..offered
            },
            Package {
                sha256: "",
                ..offered
            },
            Package {
                sha256: &"0".repeat(64),
                ..offered
            },
        ] {
            assert!(!matches(pending, offered));
        }
        for invalid in ["", "abc", &"z".repeat(64)] {
            let malformed = Package {
                sha256: invalid,
                ..offered
            };
            assert!(!matches(malformed, malformed));
        }
    }
}
