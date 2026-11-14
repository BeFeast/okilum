//! #1610 against real OpenSSH: a user-mode sshd on 127.0.0.1, an unknown host
//! key and a passphrase-protected client key with no agent. Ignored by
//! default (needs `sshd`, `ssh`, `ssh-keygen`); run with
//! `cargo test -p okilum-core --test ssh_askpass_e2e -- --ignored`.
#![cfg(unix)]
use okilum_core::{
    log::sftp::OpenSsh,
    remote_markdown::{self, Source},
    ssh_askpass::{self, Kind},
};
use std::{
    collections::VecDeque,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const PASSPHRASE: &str = "fixture passphrase 1610";

struct Sshd(Child);
impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn which(program: &str, extra: &[&str]) -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .chain(extra.iter().map(PathBuf::from))
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("{program} is required for this test"))
}
fn run(command: &mut Command) {
    let status = command.status().unwrap();
    assert!(status.success(), "{command:?}");
}
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

#[derive(Default)]
struct Script {
    answers: VecDeque<Option<String>>,
    asked: Vec<(Kind, String)>,
    /// Keep requests open, as a dialog the person has not answered yet.
    hold: bool,
    held: Vec<ssh_askpass::Request>,
}

#[test]
#[ignore]
fn real_openssh_asks_for_unknown_host_and_passphrase_through_the_bridge() {
    let sshd = which("sshd", &["/usr/sbin", "/usr/local/sbin"]);
    let ssh = which("ssh", &[]);
    let keygen = which("ssh-keygen", &[]);
    // `cargo build -p okilum-core --example ssh_askpass_helper` first; the
    // test binary may sit deeper than the profile directory.
    let exe = std::env::current_exe().unwrap();
    let helper = exe
        .ancestors()
        .map(|dir| dir.join("examples/ssh_askpass_helper"))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("build the ssh_askpass_helper example for {}", exe.display()));
    assert!(helper.is_file(), "missing {}", helper.display());

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = |name: &str| root.join(name);
    run(Command::new(&keygen)
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path("host")));
    run(Command::new(&keygen)
        .args(["-q", "-t", "ed25519", "-N", PASSPHRASE, "-f"])
        .arg(path("id")));
    std::fs::copy(path("id.pub"), path("authorized_keys")).unwrap();
    write(&path("known_hosts"), "");
    let port = free_port();
    let user = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_owned();
    write(
        &path("sshd_config"),
        &format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {host}\nPidFile {pid}\n\
             AuthorizedKeysFile {keys}\nUsePAM no\nStrictModes no\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\n\
             PubkeyAuthentication yes\nSubsystem sftp internal-sftp\nLogLevel ERROR\n",
            host = path("host").display(),
            pid = path("sshd.pid").display(),
            keys = path("authorized_keys").display(),
        ),
    );
    let identity = format!(
        "  IdentityFile {id}\n  IdentitiesOnly yes\n  IdentityAgent none\n  \
         UserKnownHostsFile {known}\n  GlobalKnownHostsFile /dev/null\n  \
         PreferredAuthentications publickey\n  ControlMaster no\n  ControlPath none\n",
        id = path("id").display(),
        known = path("known_hosts").display(),
    );
    write(
        &path("ssh_config"),
        &format!(
            "Host fixture\n  HostName 127.0.0.1\n  Port {port}\n  User {user}\n{identity}\
             Host 127.0.0.1\n{identity}"
        ),
    );
    // Product commands run `ssh` from PATH; this wrapper only adds the
    // fixture config, standing in for the person's ~/.ssh/config.
    std::fs::create_dir(path("bin")).unwrap();
    write(
        &path("bin/ssh"),
        &format!(
            "#!/bin/sh\nexec '{}' -F '{}' \"$@\"\n",
            ssh.display(),
            path("ssh_config").display()
        ),
    );
    std::fs::set_permissions(path("bin/ssh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let search = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![path("bin")];
    paths.extend(std::env::split_paths(&search));
    // The only test in this binary; no other thread reads the env yet.
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());

    let _sshd = Sshd(
        Command::new(&sshd)
            .args(["-D", "-e", "-f"])
            .arg(path("sshd_config"))
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "sshd did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let script = Arc::new(Mutex::new(Script::default()));
    let handler = script.clone();
    ssh_askpass::install(helper, move |request| {
        let mut script = handler.lock().unwrap();
        script.asked.push((request.kind, request.prompt.clone()));
        if script.hold {
            script.held.push(request);
            return;
        }
        let answer = script.answers.pop_front().flatten();
        drop(script);
        request.answer(answer);
    })
    .unwrap();
    let answer = |answers: Vec<Option<&str>>| {
        let mut script = script.lock().unwrap();
        script.asked.clear();
        script.answers = answers.into_iter().map(|a| a.map(str::to_owned)).collect();
    };
    let asked = || script.lock().unwrap().asked.clone();
    write(&path("note.md"), "# Remote\n\nvia OpenSSH\n");
    let note = path("note.md").display().to_string();

    // 1. Unknown host: OpenSSH asks for trust, then for the key passphrase.
    answer(vec![Some("yes"), Some(PASSPHRASE)]);
    let source = Source::parse(&format!("ssh://fixture{note}")).unwrap();
    let text = remote_markdown::read(&source, &mut OpenSsh::default(), || false).unwrap();
    assert_eq!(text, "# Remote\n\nvia OpenSSH\n");
    let prompts = asked();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_eq!(prompts[0].0, Kind::Confirm, "{prompts:?}");
    assert!(prompts[0].1.contains("authenticity of host"), "{prompts:?}");
    assert_eq!(prompts[1].0, Kind::Secret, "{prompts:?}");
    assert!(prompts[1].1.contains("passphrase"), "{prompts:?}");
    assert!(std::fs::read_to_string(path("known_hosts"))
        .unwrap()
        .contains("ssh-ed25519"));

    // 2. user@host:port from the link; the host is now known: passphrase only.
    answer(vec![Some(PASSPHRASE)]);
    let source = Source::parse(&format!("ssh://{user}@127.0.0.1:{port}{note}")).unwrap();
    let text = remote_markdown::read(&source, &mut OpenSsh::default(), || false).unwrap();
    assert_eq!(text, "# Remote\n\nvia OpenSSH\n");
    let prompts = asked();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_eq!(prompts[0].0, Kind::Secret);

    // 3. Cancel: the read fails, and the connection is not asked again.
    answer(vec![None, Some(PASSPHRASE), Some(PASSPHRASE)]);
    let source = Source::parse(&format!("ssh://fixture{note}")).unwrap();
    assert!(remote_markdown::read(&source, &mut OpenSsh::default(), || false).is_err());
    assert_eq!(asked().len(), 1, "{:?}", asked());

    // 4. Stop while the passphrase dialog is open: the read ends at once,
    //    ssh is killed, the dialog is told to close, and later prompts work.
    script.lock().unwrap().hold = true;
    answer(vec![]);
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker = {
        let cancel = cancel.clone();
        let source = source.clone();
        std::thread::spawn(move || {
            let result =
                remote_markdown::read(&source, &mut OpenSsh::cancellable(cancel), || false);
            (result, Instant::now())
        })
    };
    let started = Instant::now();
    while script.lock().unwrap().held.is_empty() {
        assert!(started.elapsed() < Duration::from_secs(20), "no dialog");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(!script.lock().unwrap().held[0].is_cancelled());
    let stopped = Instant::now();
    cancel.store(true, std::sync::atomic::Ordering::Release);
    let (result, ended) = worker.join().unwrap();
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
    assert!(
        ended - stopped < Duration::from_secs(2),
        "not a 15 s deadline"
    );
    assert!(
        script.lock().unwrap().held[0].is_cancelled(),
        "dialog closes"
    );
    {
        let mut script = script.lock().unwrap();
        script.hold = false;
        script.held.clear();
    }
    answer(vec![Some(PASSPHRASE)]);
    let text = remote_markdown::read(&source, &mut OpenSsh::default(), || false).unwrap();
    assert_eq!(text, "# Remote\n\nvia OpenSSH\n");
    assert_eq!(asked().len(), 1, "{:?}", asked());

    // 5. A wrong host key is still refused by OpenSSH, with no dialog.
    write(
        &path("known_hosts"),
        &format!(
            "[127.0.0.1]:{port} {}",
            std::fs::read_to_string(path("id.pub")).unwrap()
        ),
    );
    answer(vec![Some("yes"), Some(PASSPHRASE)]);
    assert!(remote_markdown::read(&source, &mut OpenSsh::default(), || false).is_err());
    assert!(asked().is_empty(), "{:?}", asked());
}
