use super::*;
use std::fs;

const NAME: &str = if cfg!(windows) {
    "okilum-sync-supervisor.exe"
} else {
    "okilum-sync-supervisor"
};

struct Fixture {
    /// Plays the Velopack layout: `<app>/current/<helper>` and a private root beside it.
    _dir: tempfile::TempDir,
    payload: PathBuf,
    root: PathBuf,
}
fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn private_root(path: &Path) {
    #[cfg(windows)]
    drop(
        super::super::windows::private::PrivateDirectory::prepare(path.to_str().unwrap()).unwrap(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}
impl Fixture {
    fn new(content: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Windows canonicalizes to `\\?\C:\...`, which the private-directory check
        // (rightly) refuses; the product passes plain paths and only compares canonical ones.
        let base = PathBuf::from(
            fs::canonicalize(dir.path())
                .unwrap()
                .to_string_lossy()
                .trim_start_matches(r"\\?\")
                .to_string(),
        );
        let current = base.join("app").join("current");
        fs::create_dir_all(&current).unwrap();
        let payload = current.join(NAME);
        fs::write(&payload, content).unwrap();
        let root = base.join("state");
        private_root(&root);
        Self {
            _dir: dir,
            payload,
            root,
        }
    }
}

#[test]
fn the_helper_is_copied_outside_the_application_folder_with_its_digest() {
    let f = Fixture::new(b"the supervisor image");
    let staged = stage_supervisor(&f.payload, &f.root, "0.1.5001").unwrap();
    assert_eq!(
        staged.path,
        f.root.join("supervisor").join("0.1.5001").join(NAME)
    );
    assert_eq!(staged.digest, digest(b"the supervisor image"));
    assert_eq!(fs::read(&staged.path).unwrap(), b"the supervisor image");
    assert!(fs::metadata(&staged.path).unwrap().permissions().readonly());
    assert!(!staged.path.starts_with(f.payload.parent().unwrap()));
    // The source is untouched and nothing else was left behind.
    assert_eq!(fs::read(&f.payload).unwrap(), b"the supervisor image");
    let names: Vec<_> = fs::read_dir(staged.path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [NAME]);
}

#[test]
fn staging_again_is_idempotent_and_never_replaces_a_different_file() {
    let f = Fixture::new(b"image one");
    let first = stage_supervisor(&f.payload, &f.root, "0.1.1").unwrap();
    assert_eq!(
        stage_supervisor(&f.payload, &f.root, "0.1.1").unwrap(),
        first
    );

    // The app was updated in place under the same version label: refuse, leave the
    // staged file (it may be the running supervisor) exactly as it was.
    fs::remove_file(&f.payload).unwrap();
    fs::write(&f.payload, b"image two").unwrap();
    let error = stage_supervisor(&f.payload, &f.root, "0.1.1")
        .unwrap_err()
        .to_string();
    assert!(error.contains("different supervisor"), "{error}");
    assert_eq!(fs::read(&first.path).unwrap(), b"image one");
    let leftovers = fs::read_dir(first.path.parent().unwrap()).unwrap().count();
    assert_eq!(leftovers, 1, "a temporary copy was left behind");

    // A new version stages beside the old one; the old one stays.
    let second = stage_supervisor(&f.payload, &f.root, "0.1.2").unwrap();
    assert_ne!(second.path, first.path);
    assert_eq!(second.digest, digest(b"image two"));
    assert_eq!(fs::read(&first.path).unwrap(), b"image one");
}

#[test]
fn the_root_must_be_private_absolute_and_outside_the_application_folder() {
    let f = Fixture::new(b"image");
    let version = "0.1.1";
    assert!(stage_supervisor(&f.payload, &f.root, version).is_ok()); // control

    // Inside the folder the updater replaces, directly or by name.
    let inside = f.payload.parent().unwrap().join("state");
    private_root(&inside);
    assert!(stage_supervisor(&f.payload, &inside, version).is_err());
    let named = f._dir.path().join("current").join("state");
    fs::create_dir_all(&named).unwrap();
    private_root(&named.join("inner"));
    let error = stage_supervisor(&f.payload, &named.join("inner"), version)
        .unwrap_err()
        .to_string();
    assert!(error.contains("current"), "{error}");

    assert!(stage_supervisor(&f.payload, Path::new("relative"), version).is_err());
    assert!(stage_supervisor(&f.payload, &f.root.join("missing"), version).is_err());

    // A directory other users could read.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let shared = f._dir.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(stage_supervisor(&f.payload, &shared, version).is_err());
    }
    #[cfg(windows)]
    {
        let shared = f._dir.path().join("shared");
        fs::create_dir(&shared).unwrap(); // inherits the temp folder's broad DACL
        assert!(stage_supervisor(&f.payload, &shared, version).is_err());
    }
}

#[test]
fn bad_versions_and_a_missing_source_stage_nothing() {
    let f = Fixture::new(b"image");
    for version in ["", ".", "..", "a/b", "a\\b", "a b", "é", &"v".repeat(65)] {
        assert!(
            stage_supervisor(&f.payload, &f.root, version).is_err(),
            "{version:?}"
        );
    }
    assert!(
        !f.root.join("supervisor").exists(),
        "a bad request created folders"
    );
    let missing = f.payload.with_file_name("missing-helper");
    assert!(stage_supervisor(&missing, &f.root, "0.1.1").is_err());
    assert!(!f.root.join("supervisor").exists());
}

#[test]
fn racing_stagings_of_different_content_cannot_overwrite_each_other() {
    use std::sync::{Arc, Barrier};
    let f = Fixture::new(b"unused");
    let base = f
        .payload
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let payloads: Vec<(PathBuf, Vec<u8>)> = ["a", "b"]
        .iter()
        .map(|side| {
            let current = base.join(format!("app-{side}")).join("current");
            fs::create_dir_all(&current).unwrap();
            let path = current.join(NAME);
            let content = format!("image {side}").into_bytes();
            fs::write(&path, &content).unwrap();
            (path, content)
        })
        .collect();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|n| {
            let (payload, content) = payloads[n % 2].clone();
            let (root, barrier) = (f.root.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                (content, stage_supervisor(&payload, &root, "0.1.9"))
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let staged = f.root.join("supervisor").join("0.1.9").join(NAME);
    let winner = fs::read(&staged).unwrap();
    assert!(winner == b"image a" || winner == b"image b");
    for (content, result) in &results {
        match result {
            Ok(done) => {
                assert_eq!(content, &winner, "a loser was reported as staged");
                assert_eq!(done.digest, digest(&winner));
                assert_eq!(done.path, staged);
            }
            Err(error) => {
                assert_ne!(content, &winner);
                assert!(
                    error.to_string().contains("different supervisor"),
                    "{error}"
                );
            }
        }
    }
    assert_eq!(results.iter().filter(|(_, r)| r.is_ok()).count(), 4);
    let names: Vec<_> = fs::read_dir(staged.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [NAME], "temporary copies were left behind");
}
