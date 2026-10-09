use super::*;
use std::{fs, path::Path};

// A disposable real process tree driven by the production argv. The root starts one
// child (same group) and then waits; marker files in the data directory choose the
// variant. No shell, no Syncthing, no personal state.
const FIXTURE: &str = r#"
use std::os::unix::process::CommandExt;
extern "C" { fn setsid() -> i32; }
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("child") {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < until {
            if std::path::Path::new(&args[2]).join("release-child").exists() { break; }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        return;
    }
    let data = args.windows(2).find(|a| a[0] == "--data").unwrap()[1].clone();
    let dir = std::path::Path::new(&data);
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.arg("child").arg(&data);
    if dir.join("leave-group").exists() {
        unsafe { command.pre_exec(|| { setsid(); Ok(()) }); }
    }
    let mut child = command.spawn().unwrap();
    std::fs::write(dir.join("child.pid"), child.id().to_string()).unwrap();
    if dir.join("exit-parent").exists() { return; }
    let _ = child.wait();
}
"#;

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
/// Alive and not a zombie (an orphaned zombie is dead for these purposes).
fn alive(pid: u32) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&out.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}
struct Fixture {
    root: tempfile::TempDir,
    executable: String,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fixture.rs");
        let executable = root.path().join("fixture");
        fs::write(&source, FIXTURE).unwrap();
        let compiled = Command::new("rustc")
            .arg("--edition=2021")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        Self {
            executable: fs::canonicalize(&executable)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            root,
        }
    }
    fn launch(&self, name: &str, markers: &[&str]) -> (Launch, std::path::PathBuf) {
        let data = self.root.path().join(name).join("data");
        let config = self.root.path().join(name).join("config");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&config).unwrap();
        for marker in markers {
            fs::write(data.join(marker), b"").unwrap();
        }
        let launch = Launch {
            executable: self.executable.clone(),
            config: fs::canonicalize(&config)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            data: fs::canonicalize(&data)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        };
        (launch, data)
    }
    /// Tree plus the pid of the descendant the fixture started.
    fn tree(
        &self,
        name: &str,
        markers: &[&str],
        budget: Duration,
    ) -> (ProcessGroupTree, u32, std::path::PathBuf) {
        let (launch, data) = self.launch(name, markers);
        let tree = ProcessGroupTree::spawn(&launch, budget).unwrap();
        let pid_file = data.join("child.pid");
        wait_until("descendant pid", || {
            pid_file.exists() && !fs::read_to_string(&pid_file).unwrap().is_empty()
        });
        let pid: u32 = fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        assert!(alive(pid), "positive control: the descendant is alive");
        (tree, pid, data)
    }
}
fn release(data: &Path) {
    fs::write(data.join("release-child"), b"").unwrap();
}

#[test]
fn unix_arguments_are_fixed_and_paths_must_be_plain_and_absolute() {
    let launch = Launch {
        executable: "/runtime/syncthing".into(),
        config: "/state/config".into(),
        data: "/state/data".into(),
    };
    assert_eq!(
        launch.unix_arguments().unwrap(),
        [
            "serve",
            "--no-browser",
            "--no-restart",
            "--no-upgrade",
            "--config",
            "/state/config",
            "--data",
            "/state/data"
        ]
    );
    for bad in ["relative/x", "/a/../b", "/a/./b", "/a/\nb", "/a/\0b", ""] {
        for field in 0..3 {
            let mut broken = launch.clone();
            match field {
                0 => broken.executable = bad.into(),
                1 => broken.config = bad.into(),
                _ => broken.data = bad.into(),
            }
            assert!(broken.unix_arguments().is_err(), "{bad:?} in field {field}");
        }
    }
    let mut same = launch.clone();
    same.data = same.config.clone();
    assert!(same.unix_arguments().is_err());
}

#[test]
fn stop_terminates_the_group_reaps_the_root_and_is_idempotent() {
    let fixture = Fixture::new();
    let (mut tree, child, _data) = fixture.tree("plain", &[], Duration::from_secs(10));
    assert_eq!(tree.status().unwrap(), Status::Running);
    assert!(!tree.root_exited().unwrap());
    assert_eq!(tree.stop().unwrap(), Status::Stopped);
    wait_until("descendant to disappear", || !alive(child));
    assert_eq!(tree.status().unwrap(), Status::Stopped);
    assert_eq!(
        tree.stop().unwrap(),
        Status::Stopped,
        "a repeat stays Stopped"
    );
}

#[test]
fn an_exited_root_with_a_live_member_is_still_running_until_stop_flushes_it() {
    let fixture = Fixture::new();
    let (mut tree, child, data) =
        fixture.tree("crashed", &["exit-parent"], Duration::from_secs(10));
    wait_until("root exit", || tree.root_exited().unwrap());
    assert!(alive(child), "the member outlives its leader");
    assert_eq!(
        tree.status().unwrap(),
        Status::Running,
        "never done while a member lives"
    );
    assert_eq!(tree.stop().unwrap(), Status::Stopped);
    wait_until("member to disappear", || !alive(child));
    release(&data);
}

#[test]
fn an_expired_budget_signals_nothing_and_a_real_budget_then_stops_the_same_tree() {
    let fixture = Fixture::new();
    let (mut tree, child, _data) = fixture.tree("budget", &[], Duration::ZERO);
    assert_eq!(tree.stop().unwrap(), Status::Stopping);
    assert_eq!(tree.status().unwrap(), Status::Running);
    assert!(alive(child), "an expired budget terminated the descendant");
    assert!(
        !tree.root_exited().unwrap(),
        "an expired budget terminated the root"
    );
    tree.budget = Duration::from_secs(10); // positive control on the same tree
    assert_eq!(tree.stop().unwrap(), Status::Stopped);
    wait_until("descendant to disappear", || !alive(child));
}

#[test]
fn a_descendant_that_left_the_group_is_not_owned_so_stop_is_never_stopped() {
    let fixture = Fixture::new();
    let (mut tree, child, data) =
        fixture.tree("escaped", &["leave-group"], Duration::from_millis(600));
    assert_eq!(tree.stop().unwrap(), Status::Stopping);
    assert!(
        alive(child),
        "the escaped copy is outside the group and untouched"
    );
    assert_eq!(tree.status().unwrap(), Status::Running);
    // Not killed, not adopted: once it ends by itself the answer becomes Stopped.
    release(&data);
    wait_until("escaped copy to exit", || !alive(child));
    tree.budget = Duration::from_secs(10);
    assert_eq!(tree.stop().unwrap(), Status::Stopped);
}

#[test]
fn unrelated_processes_never_block_a_stop() {
    let fixture = Fixture::new();
    // The same image started BEFORE the tree, another image, both same user.
    let (older_launch, older_data) = fixture.launch("older", &[]);
    let older = Command::new(&older_launch.executable)
        .arg("child")
        .arg(&older_launch.data)
        .spawn()
        .unwrap();
    let other = Command::new("sleep").arg("30").spawn().unwrap();
    std::thread::sleep(Duration::from_millis(150)); // distinct start ticks
    let (mut tree, _child, _data) = fixture.tree("unrelated", &[], Duration::from_secs(10));
    assert_eq!(tree.stop().unwrap(), Status::Stopped);
    assert!(
        alive(older.id()) && alive(other.id()),
        "bystanders were touched"
    );
    // Positive control for the scan itself: it does see that older copy when asked
    // to look from before it started.
    let image = fs::canonicalize(&older_launch.executable).unwrap();
    let since = scan::started(older.id()).unwrap();
    let seen = scan::live_copies(&image, rustix::process::geteuid().as_raw(), since).unwrap();
    assert!(
        seen.contains(&older.id()),
        "the scan must see the live copy"
    );
    release(&older_data);
    let mut older = older;
    let mut other = other;
    other.kill().unwrap();
    other.wait().unwrap();
    older.wait().unwrap();
}

#[test]
fn dropping_the_tree_does_not_leave_the_runtime_behind() {
    let fixture = Fixture::new();
    let (tree, child, _data) = fixture.tree("dropped", &[], Duration::from_secs(10));
    drop(tree);
    wait_until("descendant to disappear", || !alive(child));
}
