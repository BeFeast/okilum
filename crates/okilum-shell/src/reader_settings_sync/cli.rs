//! Headless `okilum sync …` (#2589): pair computers and share vaults from a
//! shell script. Every step runs the same executor and controller calls as the
//! Settings → Sync buttons, before any window or GPUI initialization.
use super::*;
use okilum_sync_controller::direct::{Peer, Pending};
use std::{collections::HashSet, io::Write as _, time::Instant};

const USAGE: &str = "\
okilum sync — pair computers and share vaults without opening a window

USAGE:
    okilum sync status
    okilum sync invite [--name NAME] [--approve-first | --approve NAME] [--wait SECONDS]
    okilum sync connect <URL> [--name NAME] [--wait SECONDS]
    okilum sync cancel
    okilum sync remove <COMPUTER>
    okilum sync serve [--wait SECONDS]
    okilum sync share <VAULT>
    okilum sync choose <COMPUTER> <VAULT> <DESTINATION> [--wait SECONDS]
    okilum sync select <SYNCTHING-CONFIG.xml>       (Linux)

invite prints only the pairing URL on its first stdout line, then keeps this
process listening. --approve-first approves the first computer that redeems the
URL; --approve NAME approves only a computer with that name; without either,
invite asks on stdin. It exits after the other computer has received the
approval and prints a JSON result line.
connect redeems the URL and waits until the other computer approves.
serve keeps answering paired computers (catalog, vault transfers); without
--wait it runs until interrupted.
choose receives a vault (id or name, from the computer's catalog) into
DESTINATION: a new folder is created, an existing one must be empty.
--name defaults to this computer's host name.

One process owns this computer's Sync state. While `okilum sync serve` or the
app owns it, the other commands run through that process and it keeps running.
The app owns Sync once Sync has started in it; until then these commands refuse
while it is open. --quit-app quits it first instead (Linux: SIGTERM, which saves
and exits; macOS: the AppleScript quit command).

Exit codes: 0 success, 1 failure or timeout, 2 usage, 3 Okilum is running.
";

const INVITE_WAIT: Duration = Duration::from_secs(900);
const CONNECT_WAIT: Duration = Duration::from_secs(300);
const CHOOSE_WAIT: Duration = Duration::from_secs(600);

#[derive(Debug, PartialEq)]
enum Approval {
    Ask,
    First,
    Named(String),
}
#[derive(Debug, PartialEq)]
enum Verb {
    Remove(String),
    Status,
    Invite {
        name: Option<String>,
        approval: Approval,
        wait: Duration,
    },
    Connect {
        url: String,
        name: Option<String>,
        wait: Duration,
    },
    Cancel,
    Serve {
        wait: Option<Duration>,
    },
    Share(PathBuf),
    Choose {
        peer: String,
        vault: String,
        destination: PathBuf,
        wait: Duration,
    },
    Select(PathBuf),
}
#[derive(Debug, PartialEq)]
struct Request {
    verb: Verb,
    quit_app: bool,
}

/// `Some(exit code)` when argv is `okilum sync …`; `None` leaves every other
/// command line to the ordinary launcher.
pub(crate) fn entry() -> Option<i32> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("sync")) {
        return None;
    }
    let Ok(arguments) = arguments
        .map(std::ffi::OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        diagnostic_error!("okilum sync: arguments must be UTF-8");
        return Some(2);
    };
    Some(run(&arguments))
}

fn run(arguments: &[String]) -> i32 {
    #[cfg(target_os = "macos")]
    if arguments == ["qa-stop"] {
        return match native_signing_policy()
            .and_then(okilum_sync_controller::sidecar::macos::bootstrap::stop_isolated_current_app)
        {
            Ok(()) => {
                diagnostic_println!("Isolated Sync helper stopped and its exit verified.");
                0
            }
            Err(error) => {
                diagnostic_error!("okilum sync qa-stop: {error:#}");
                1
            }
        };
    }
    let request = match parse(arguments) {
        Ok(Some(request)) => request,
        Ok(None) => {
            print!("{USAGE}");
            return 0;
        }
        Err(error) => {
            diagnostic_error!("okilum sync: {error:#}\n\n{USAGE}");
            return 2;
        }
    };
    let result = (|| -> anyhow::Result<i32> {
        use anyhow::Context as _;
        let directory = reader_history::state_directory()?;
        let app = running_app(&directory)?;
        // #2589: `okilum sync serve` or the app owns Sync here; it runs this
        // command, so neither has to stop. --quit-app still quits the app.
        if !(request.quit_app && app.is_some()) && !matches!(request.verb, Verb::Serve { .. }) {
            if let Some(owner) = state_location()
                .ok()
                .map(|state| control::Client::find(&state))
                .transpose()?
                .flatten()
            {
                return execute(request.verb, Session::Owner(owner));
            }
        }
        if let Some(pid) = app {
            if !request.quit_app {
                diagnostic_error!("okilum sync: {}", running_message(pid));
                return Ok(3);
            }
            quit_app(&directory, pid)?;
        }
        if !matches!(request.verb, Verb::Status | Verb::Cancel) {
            // Like an opened pairing link: the next app start keeps serving.
            reader_features::enable_saved(&directory, reader_features::Feature::Sync)
                .context("turn on Sync in Settings → Features")?;
        }
        execute(request.verb, Session::default())
    })();
    result.unwrap_or_else(|error| {
        diagnostic_error!("okilum sync: {error:#}");
        if format!("{error:#}").contains("would block") {
            // Controller::open found the profile's Sync lock taken.
            diagnostic_error!(
                "okilum sync: another `okilum sync` command is using this Sync state"
            );
        }
        1
    })
}

fn parse(arguments: &[String]) -> anyhow::Result<Option<Request>> {
    use anyhow::{bail, ensure, Context as _};
    let mut positional = Vec::new();
    let (mut name, mut wait, mut approve, mut approve_first, mut quit_app) =
        (None, None, None, false, false);
    let mut options_done = false;
    let mut iter = arguments.iter();
    while let Some(argument) = iter.next() {
        if options_done || !argument.starts_with('-') {
            positional.push(argument.clone());
            continue;
        }
        let mut value = |option: &str| {
            iter.next()
                .cloned()
                .with_context(|| format!("{option} needs a value"))
        };
        match argument.as_str() {
            "--" => options_done = true,
            "-h" | "--help" => return Ok(None),
            "--quit-app" => quit_app = true,
            "--approve-first" => approve_first = true,
            "--name" => name = Some(value("--name")?),
            "--approve" => approve = Some(value("--approve")?),
            "--wait" => {
                let seconds: u64 = value("--wait")?
                    .parse()
                    .context("--wait takes a whole number of seconds")?;
                ensure!(seconds > 0, "--wait must be at least 1 second");
                wait = Some(Duration::from_secs(seconds));
            }
            other => bail!("unknown option {other}"),
        }
    }
    let Some((verb, rest)) = positional.split_first() else {
        bail!("missing command");
    };
    let arity = |count: usize| -> anyhow::Result<()> {
        ensure!(
            rest.len() == count,
            "{verb} takes {count} argument(s), got {}",
            rest.len()
        );
        Ok(())
    };
    let only = |allowed: &[&str]| -> anyhow::Result<()> {
        for (set, option) in [
            (name.is_some(), "--name"),
            (wait.is_some(), "--wait"),
            (approve.is_some(), "--approve"),
            (approve_first, "--approve-first"),
        ] {
            ensure!(
                !set || allowed.contains(&option),
                "{option} does not apply to {verb}"
            );
        }
        Ok(())
    };
    let verb = match verb.as_str() {
        "status" | "cancel" => {
            arity(0)?;
            only(&[])?;
            if verb == "status" {
                Verb::Status
            } else {
                Verb::Cancel
            }
        }
        "invite" => {
            arity(0)?;
            only(&["--name", "--wait", "--approve", "--approve-first"])?;
            ensure!(
                !(approve_first && approve.is_some()),
                "use either --approve-first or --approve NAME"
            );
            Verb::Invite {
                name,
                approval: match (approve_first, approve) {
                    (true, _) => Approval::First,
                    (false, Some(name)) => Approval::Named(name),
                    (false, None) => Approval::Ask,
                },
                wait: wait.unwrap_or(INVITE_WAIT),
            }
        }
        "connect" => {
            arity(1)?;
            only(&["--name", "--wait"])?;
            Verb::Connect {
                url: rest[0].clone(),
                name,
                wait: wait.unwrap_or(CONNECT_WAIT),
            }
        }
        "serve" => {
            arity(0)?;
            only(&["--wait"])?;
            Verb::Serve { wait }
        }
        "remove" => {
            arity(1)?;
            only(&[])?;
            Verb::Remove(rest[0].clone())
        }
        "share" | "select" => {
            arity(1)?;
            only(&[])?;
            let path = PathBuf::from(&rest[0]);
            if verb == "share" {
                Verb::Share(path)
            } else {
                Verb::Select(path)
            }
        }
        "choose" => {
            arity(3)?;
            only(&["--wait"])?;
            Verb::Choose {
                peer: rest[0].clone(),
                vault: rest[1].clone(),
                destination: PathBuf::from(&rest[2]),
                wait: wait.unwrap_or(CHOOSE_WAIT),
            }
        }
        other => bail!("unknown command {other}"),
    };
    Ok(Some(Request { verb, quit_app }))
}

/// The running app's process id while it holds the single-instance lock. The
/// app keeps one Sync controller for its whole run; a second owner is refused.
fn running_app(directory: &Path) -> anyhow::Result<Option<u32>> {
    let lock = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("reader-instance.lock"))
    {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    match lock.try_lock() {
        Ok(()) => Ok(None),
        Err(std::fs::TryLockError::WouldBlock) => {
            let pid = std::fs::read(directory.join("reader-instance.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|value| value["pid"].as_u64())
                .and_then(|pid| u32::try_from(pid).ok())
                .unwrap_or(0);
            Ok(Some(pid))
        }
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}
fn quit_command(pid: u32) -> String {
    if cfg!(target_os = "macos") {
        "osascript -e 'quit app \"Okilum\"'".into()
    } else if pid == 0 {
        "quit Okilum from its window".into()
    } else {
        format!("kill {pid}")
    }
}
fn running_message(pid: u32) -> String {
    format!(
        "Okilum is running and owns this computer's Sync state. Quit it first, \
         or add --quit-app:\n    {}",
        quit_command(pid)
    )
}
fn quit_app(directory: &Path, pid: u32) -> anyhow::Result<()> {
    diagnostic_error!("okilum sync: quitting Okilum: {}", quit_command(pid));
    #[cfg(target_os = "macos")]
    {
        let _ = pid;
        let status = std::process::Command::new("osascript")
            .args(["-e", "quit app \"Okilum\""])
            .status()?;
        anyhow::ensure!(status.success(), "osascript could not quit Okilum");
    }
    #[cfg(target_os = "linux")]
    {
        anyhow::ensure!(pid != 0, "cannot find the running Okilum process");
        let pid = libc::pid_t::try_from(pid)?;
        // SAFETY: kill sends a signal and touches no memory of this process.
        anyhow::ensure!(
            unsafe { libc::kill(pid, libc::SIGTERM) } == 0,
            "cannot signal Okilum: {}",
            std::io::Error::last_os_error()
        );
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while running_app(directory)?.is_some() {
        anyhow::ensure!(
            Instant::now() < deadline,
            "Okilum is still running after 30 seconds; check it for an open dialog"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

/// Where the command's Sync steps run.
enum Session {
    /// One controller for the whole command, like the Settings view's cached one.
    Local {
        controller: Option<Controller>,
        epoch: Epoch,
    },
    /// The process that owns this computer's Sync state runs them (#2589).
    Owner(control::Client),
}
impl Default for Session {
    fn default() -> Self {
        Self::Local {
            controller: None,
            epoch: Epoch::default(),
        }
    }
}
impl Session {
    fn perform(&mut self, operation: Operation) -> anyhow::Result<Step> {
        match self {
            Self::Local { controller, epoch } => {
                let output = perform_scoped(controller.clone(), operation, &epoch.ticket())?;
                *controller = Some(output.controller.clone());
                Ok(Step::from(output))
            }
            Self::Owner(owner) => match owner.call(control::Call::Perform(Box::new(operation)))? {
                control::Answer::Step(step) => Ok(*step),
                _ => anyhow::bail!("unexpected answer from the process that owns Sync"),
            },
        }
    }
    fn controller(&self) -> anyhow::Result<&Controller> {
        use anyhow::Context as _;
        match self {
            Self::Local { controller, .. } => controller.as_ref().context("Sync is not open"),
            Self::Owner(_) => anyhow::bail!("Sync is owned by another process"),
        }
    }
    fn pairing(
        &self,
        call: control::Call,
        local: impl FnOnce(&Controller) -> anyhow::Result<Snapshot>,
    ) -> anyhow::Result<Snapshot> {
        let Self::Owner(owner) = self else {
            return local(self.controller()?);
        };
        match owner.call(call)? {
            control::Answer::Snapshot(snapshot) => Ok(*snapshot),
            _ => anyhow::bail!("unexpected answer from the process that owns Sync"),
        }
    }
    fn refresh_pairing(&self) -> anyhow::Result<Snapshot> {
        self.pairing(control::Call::RefreshPairing, Controller::refresh_pairing)
    }
    fn approve(&self, request: uuid::Uuid) -> anyhow::Result<Snapshot> {
        self.pairing(control::Call::Approve(request), |c| c.approve(request))
    }
    fn reject(&self, request: uuid::Uuid) -> anyhow::Result<Snapshot> {
        self.pairing(control::Call::Reject(request), |c| c.reject(request))
    }
    fn approval_delivered(&self) -> anyhow::Result<bool> {
        let Self::Owner(owner) = self else {
            return self.controller()?.approval_delivered();
        };
        match owner.call(control::Call::ApprovalDelivered)? {
            control::Answer::Delivered(delivered) => Ok(delivered),
            _ => anyhow::bail!("unexpected answer from the process that owns Sync"),
        }
    }
}

/// One step's result as the command follows and prints it, wherever it ran.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Step {
    snapshot: Snapshot,
    /// `okilum sync status` JSON, made beside the controller.
    status: serde_json::Value,
    inspected: Option<ExistingVault>,
    selected_destination: Option<PathBuf>,
}
impl From<Output> for Step {
    fn from(output: Output) -> Self {
        Self {
            status: status(&output),
            snapshot: output.snapshot,
            inspected: output.inspected,
            selected_destination: output.selected_destination,
        }
    }
}

fn execute(verb: Verb, mut session: Session) -> anyhow::Result<i32> {
    use anyhow::{bail, Context as _};
    match verb {
        Verb::Remove(computer) => {
            let output = session.perform(Operation::Read)?;
            let matches: Vec<_> = output
                .snapshot
                .computers
                .iter()
                .filter(|c| c.device_id == computer || c.name == computer)
                .collect();
            anyhow::ensure!(matches.len() == 1,"Choose one exact computer device ID from sync status; the name is missing or ambiguous");
            let output = session.perform(Operation::Remove(matches[0].device_id.clone()))?;
            print_json(&output.status);
        }
        Verb::Status => {
            let output = session.perform(Operation::Read)?;
            print_json(&output.status);
        }
        Verb::Invite {
            name,
            approval,
            wait,
        } => return invite(&mut session, name, approval, wait),
        Verb::Connect { url, name, wait } => return connect(&mut session, url, name, wait),
        Verb::Cancel => {
            let output = session.perform(Operation::Cancel)?;
            print_json(&output.status);
        }
        Verb::Serve { wait } => {
            let deadline = wait.map(|wait| Instant::now() + wait);
            let mut output = session.perform(Operation::Read)?;
            // #2589: other `okilum sync` commands of this user run through
            // this process, so pairing another computer keeps it serving.
            let controller = session.controller()?.clone();
            let _control = state_location()
                .and_then(|state| {
                    control::Host::start(&state, move |call| {
                        control::answer(&controller, call, control::perform_owned)
                    })
                })
                .map_err(|error| {
                    diagnostic_error!(
                        "okilum sync: other commands cannot reach this process: {error:#}"
                    )
                })
                .ok();
            diagnostic_error!(
                "okilum sync: serving {} paired computer(s)",
                output.snapshot.peers.len()
            );
            while deadline.is_none_or(|deadline| Instant::now() < deadline) {
                std::thread::sleep(Duration::from_secs(2));
                match session.perform(Operation::Read) {
                    Ok(next) => output = next,
                    Err(error) => diagnostic_error!("okilum sync: refresh failed: {error:#}"),
                }
            }
            print_json(&output.status);
        }
        Verb::Share(path) => {
            // Paths are this shell's; the process that owns Sync may run elsewhere.
            let path = std::path::absolute(path)?;
            let mut output = session.perform(Operation::Share(path))?;
            // Share on an existing Syncthing folder becomes inspection; the
            // explicit command is the consent the Settings view asks for.
            if let Some(inspected) = output.inspected.take() {
                output = session.perform(Operation::Consent(Box::new(inspected)))?;
            }
            print_json(&output.status);
        }
        #[cfg(target_os = "linux")]
        Verb::Select(path) => {
            let path = std::path::absolute(path)?;
            let output = session.perform(Operation::SelectConnection(path))?;
            print_json(&output.status);
        }
        #[cfg(not(target_os = "linux"))]
        Verb::Select(_) => bail!("this command is available on Linux"),
        Verb::Choose {
            peer,
            vault,
            destination,
            wait,
        } => {
            let destination = std::path::absolute(destination)?;
            let output = session.perform(Operation::Read)?;
            let peer = find_peer(&output.snapshot.peers, &peer)?;
            let catalog = session.perform(Operation::Catalog(peer))?.snapshot.catalog;
            let matches: Vec<_> = catalog
                .iter()
                .filter(|v| v.id == vault || v.label == vault)
                .collect();
            let [chosen] = matches.as_slice() else {
                bail!(
                    "vault {vault:?} is {} in this computer's catalog: {}",
                    if matches.is_empty() {
                        "not"
                    } else {
                        "ambiguous"
                    },
                    serde_json::to_string(&catalog)?
                );
            };
            let operation = if destination.exists() {
                Operation::Attach(peer, chosen.id.clone(), destination)
            } else {
                Operation::AttachNew(peer, chosen.id.clone(), destination)
            };
            let mut output = session.perform(operation)?;
            let path = output
                .selected_destination
                .clone()
                .context("destination unavailable")?;
            let deadline = Instant::now() + wait;
            // The Settings view opens the vault once this is true.
            while output.snapshot.preparing
                || !output.snapshot.attached.iter().any(|v| v.path == path)
            {
                if Instant::now() >= deadline {
                    print_json(&output.status);
                    bail!(
                        "{} is still being received; keep `okilum sync serve` running on the \
                         other computer and run this again",
                        path.display()
                    );
                }
                std::thread::sleep(Duration::from_secs(2));
                match session.perform(Operation::Read) {
                    Ok(next) => output = next,
                    Err(error) => diagnostic_error!("okilum sync: refresh failed: {error:#}"),
                }
            }
            print_json(&output.status);
        }
    }
    Ok(0)
}

fn invite(
    session: &mut Session,
    name: Option<String>,
    approval: Approval,
    wait: Duration,
) -> anyhow::Result<i32> {
    use anyhow::{bail, Context as _};
    let output = session.perform(Operation::Begin(name.unwrap_or_else(default_computer_name)))?;
    let url = output
        .snapshot
        .offer_url
        .clone()
        .context("Sync did not create a pairing URL")?;
    diagnostic_println!("{url}");
    std::io::stdout().flush()?;
    diagnostic_error!(
        "okilum sync: waiting up to {}s for the other computer",
        wait.as_secs()
    );
    let deadline = Instant::now() + wait;
    let mut approved: Option<(Pending, Option<Peer>)> = None;
    loop {
        if let Some((pending, peer)) = &approved {
            if session.approval_delivered()? {
                // The listener has written the approval; let the joiner read it.
                std::thread::sleep(Duration::from_secs(1));
                print_json(&serde_json::json!({
                    "paired": true,
                    "peer": peer.as_ref().map(peer_json),
                    "request": pending_json(pending),
                }));
                return Ok(0);
            }
        } else {
            // The Settings view's pairing poll on its cached controller.
            let snapshot = session.refresh_pairing()?;
            if let Some(pending) = snapshot.pending.clone().filter(|p| p.incoming) {
                diagnostic_error!(
                    "okilum sync: request from {:?}, fingerprint {}",
                    pending.name,
                    pending.fingerprint
                );
                if !decide(&approval, &pending)? {
                    session.reject(pending.request_id)?;
                    bail!("declined the request from {:?}", pending.name);
                }
                let before: HashSet<_> = snapshot.peers.iter().map(|p| p.id).collect();
                let after = session.approve(pending.request_id)?;
                let peer = after.peers.into_iter().find(|p| !before.contains(&p.id));
                diagnostic_error!("okilum sync: approved; waiting until it receives the approval");
                approved = Some((pending, peer));
                continue;
            }
            if snapshot.offer_url.is_none() {
                bail!("the invitation ended: {}", snapshot.status);
            }
        }
        if Instant::now() >= deadline {
            print_json(&serde_json::json!({ "paired": false, "reason": "timeout" }));
            if approved.is_some() {
                diagnostic_error!(
                    "okilum sync: approved, but the other computer did not collect the \
                     approval in time. {}",
                    if matches!(session, Session::Owner(_)) {
                        "This computer keeps serving; connect again from the other computer."
                    } else {
                        "Run `okilum sync serve` here while it connects again."
                    }
                );
            }
            return Ok(1);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn decide(approval: &Approval, pending: &Pending) -> anyhow::Result<bool> {
    Ok(match approval {
        Approval::First => true,
        Approval::Named(name) => *name == pending.name,
        Approval::Ask => {
            eprint!(
                "Approve {:?} (fingerprint {})? [y/N] ",
                pending.name, pending.fingerprint
            );
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            matches!(answer.trim(), "y" | "Y" | "yes")
        }
    })
}

fn connect(
    session: &mut Session,
    url: String,
    name: Option<String>,
    wait: Duration,
) -> anyhow::Result<i32> {
    let name = name.unwrap_or_else(default_computer_name);
    direct::validate_pairing_input(&url, &name)?;
    // Settings enters pairing before Connect; that also tells which computers
    // were already paired. Its failure (an unreachable earlier request, say)
    // is reported but does not stop Connect; `okilum sync cancel` clears it.
    let before: Option<HashSet<_>> = match session.perform(Operation::EnterPairing) {
        Ok(output) => Some(output.snapshot.peers.iter().map(|p| p.id).collect()),
        Err(error) => {
            diagnostic_error!("okilum sync: {error:#}");
            None
        }
    };
    // A first Connect also prepares and starts this computer's Sync; on a Mac its
    // first launch can take a few minutes (#2589).
    diagnostic_error!("okilum sync: connecting (a first Connect also starts Sync here)");
    let output = session.perform(Operation::Redeem(url, name))?;
    let mut snapshot = output.snapshot;
    let deadline = Instant::now() + wait;
    diagnostic_error!("okilum sync: waiting for approval on the other computer");
    loop {
        if snapshot.pending.is_none() {
            let peer = snapshot
                .peers
                .iter()
                .rev()
                .find(|p| before.as_ref().is_none_or(|b| !b.contains(&p.id)));
            let Some(peer) = peer else {
                anyhow::bail!("pairing did not complete: {}", snapshot.status);
            };
            print_json(&serde_json::json!({
                "paired": true,
                "peer": peer_json(peer),
                "catalog": snapshot.catalog,
            }));
            return Ok(0);
        }
        if Instant::now() >= deadline {
            print_json(&serde_json::json!({ "paired": false, "reason": "timeout" }));
            return Ok(1);
        }
        std::thread::sleep(Duration::from_secs(1));
        match session.refresh_pairing() {
            Ok(next) => snapshot = next,
            Err(error) if pairing_ended(&error) => return Err(error),
            Err(error) => diagnostic_error!("okilum sync: still waiting ({error:#})"),
        }
    }
}
/// The Settings view clears the pairing step on these; anything else retries.
fn pairing_ended(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_lowercase();
    ["expired", "reject", "cancel", "changed", "claimed"]
        .iter()
        .any(|word| message.contains(word))
}

fn find_peer(peers: &[Peer], wanted: &str) -> anyhow::Result<uuid::Uuid> {
    let matches: Vec<_> = peers
        .iter()
        .filter(|p| p.id.to_string() == wanted || p.name == wanted)
        .collect();
    match matches.as_slice() {
        [peer] => Ok(peer.id),
        _ => anyhow::bail!(
            "computer {wanted:?} is {} among paired computers: {}",
            if matches.is_empty() {
                "not found"
            } else {
                "ambiguous"
            },
            serde_json::to_string(&peers.iter().map(peer_json).collect::<Vec<_>>())?
        ),
    }
}

fn peer_json(peer: &Peer) -> serde_json::Value {
    serde_json::json!({ "id": peer.id, "name": peer.name, "fingerprint": peer.device_id })
}
fn pending_json(pending: &Pending) -> serde_json::Value {
    serde_json::json!({
        "request_id": pending.request_id,
        "name": pending.name,
        "fingerprint": pending.fingerprint,
        "device_id": pending.device_id,
        "direction": if pending.incoming { "incoming" } else { "outgoing" },
    })
}
/// The pairing URL carries a secret key, so status reports only that one is open.
fn status(output: &Output) -> serde_json::Value {
    let snapshot = &output.snapshot;
    let name = output
        .controller
        .computer_name()
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(default_computer_name);
    // How this computer's Syncthing reaches each paired computer, e.g. offline
    // but its vaults syncing through another computer (#2589).
    let links = output.controller.peer_links().unwrap_or_default();
    let peers = snapshot.peers.iter().map(|peer| {
        let mut value = peer_json(peer);
        if let Some(link) = links.get(&peer.id) {
            value["connection"] = serde_json::json!(link);
        }
        value
    });
    #[allow(unused_mut)]
    let mut value = serde_json::json!({
        "computers": output.snapshot.computers,
        "pending_removals": output.snapshot.pending_removals,
        "computer": name,
        "status": snapshot.status,
        "invitation_open": snapshot.offer_url.is_some(),
        "pending": snapshot.pending.as_ref().map(pending_json),
        "peers": peers.collect::<Vec<_>>(),
        "catalog": snapshot.catalog,
        "attached": snapshot.attached.iter().map(|v| serde_json::json!({
            "path": v.path,
            "label": v.label,
            "folder_id": v.folder_id,
            "folder_type": v.folder_type,
            "paused": v.paused,
        })).collect::<Vec<_>>(),
        "preparing": snapshot.preparing,
        "connection_error": output.connection_error,
    });
    #[cfg(target_os = "linux")]
    {
        value["shared"] = serde_json::json!(snapshot.shared);
        value["connections"] = output
            .runtimes
            .iter()
            .map(|choice| {
                serde_json::json!({
                    "config": choice.identity.config_file,
                    "device_id": choice.identity.device_id,
                    "managed": choice.managed,
                    "selected": choice.selected,
                    "folders": choice.folders,
                })
            })
            .collect();
    }
    value
}
fn print_json(value: &serde_json::Value) {
    diagnostic_println!("{value}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn issue2589_sync_cli_parses_the_pairing_commands() {
        let invite = parse(&args("invite --approve-first --wait 60 --quit-app"))
            .unwrap()
            .unwrap();
        assert_eq!(
            invite,
            Request {
                verb: Verb::Invite {
                    name: None,
                    approval: Approval::First,
                    wait: Duration::from_secs(60)
                },
                quit_app: true,
            }
        );
        let connect = parse(&args(
            "connect okilum-sync://pair/10.0.0.2:4000#ab --name Mac",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(
            connect.verb,
            Verb::Connect {
                url: "okilum-sync://pair/10.0.0.2:4000#ab".into(),
                name: Some("Mac".into()),
                wait: CONNECT_WAIT,
            }
        );
        assert_eq!(
            parse(&args("invite --approve idunn"))
                .unwrap()
                .unwrap()
                .verb,
            Verb::Invite {
                name: None,
                approval: Approval::Named("idunn".into()),
                wait: INVITE_WAIT
            }
        );
        assert_eq!(
            parse(&args("choose idunn Notes -- -odd"))
                .unwrap()
                .unwrap()
                .verb,
            Verb::Choose {
                peer: "idunn".into(),
                vault: "Notes".into(),
                destination: "-odd".into(),
                wait: CHOOSE_WAIT
            }
        );
        assert_eq!(
            parse(&args("serve")).unwrap().unwrap().verb,
            Verb::Serve { wait: None }
        );
        assert_eq!(parse(&args("status --help")).unwrap(), None);
    }

    #[test]
    fn issue2589_sync_cli_rejects_ambiguous_or_misplaced_options() {
        for line in [
            "",
            "pair",
            "status extra",
            "connect",
            "invite --approve-first --approve Mac",
            "status --name Mac",
            "connect URL --approve-first",
            "invite --wait 0",
            "invite --wait soon",
            "invite --name",
            "invite --bogus",
        ] {
            assert!(parse(&args(line)).is_err(), "{line:?} must be refused");
        }
    }

    #[test]
    fn issue2589_sync_cli_detects_a_running_app_through_its_instance_lock() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            running_app(directory.path()).unwrap(),
            None,
            "never started"
        );
        let lock = std::fs::File::create(directory.path().join("reader-instance.lock")).unwrap();
        assert_eq!(
            running_app(directory.path()).unwrap(),
            None,
            "stale lock file"
        );
        lock.lock().unwrap();
        std::fs::write(
            directory.path().join("reader-instance.json"),
            br#"{"port":1,"pid":4242,"nonce":"n"}"#,
        )
        .unwrap();
        assert_eq!(running_app(directory.path()).unwrap(), Some(4242));
        assert!(
            running_message(4242).contains(if cfg!(target_os = "macos") {
                "osascript -e 'quit app \"Okilum\"'"
            } else {
                "kill 4242"
            })
        );
        drop(lock);
        assert_eq!(running_app(directory.path()).unwrap(), None, "app quit");
    }

    /// The hub's actions without this machine's Syncthing inventory: its
    /// invitation listens on loopback.
    fn loopback(controller: Controller, operation: Operation) -> anyhow::Result<Output> {
        let snapshot = match operation {
            Operation::Begin(name) => {
                controller.begin_pairing_at(&name, "127.0.0.1:0".parse()?, "127.0.0.1".parse()?)?
            }
            Operation::Read => controller.snapshot()?,
            _ => anyhow::bail!("not part of this pairing"),
        };
        Ok(Output {
            runtime_health: Default::default(),
            overview: Default::default(),
            connection_error: None,
            diagnostic: None,
            controller,
            snapshot,
            inspected: None,
            selected_destination: None,
            first_sync: Default::default(),
            #[cfg(target_os = "linux")]
            runtimes: vec![],
            vaults: vec![],
        })
    }

    #[test]
    fn issue2589_invite_approve_first_runs_through_the_serving_process() -> anyhow::Result<()> {
        use anyhow::Context as _;
        let root = tempfile::tempdir()?;
        // Profile A: `okilum sync serve` owns the hub's Sync state.
        let hub_state = root.path().join("hub");
        let hub = Controller::open(hub_state.clone(), vec![])?;
        let serving = hub.clone();
        let _serve = control::Host::start(&hub_state, move |call| {
            control::answer(&serving, call, loopback)
        })?;
        // A second command in profile A runs through it instead of refusing.
        let owner = control::Client::find(&hub_state)?.context("serve answers")?;
        let inviting = std::thread::spawn(move || {
            invite(
                &mut Session::Owner(owner),
                Some("Hub".into()),
                Approval::First,
                Duration::from_secs(60),
            )
        });
        // Profile B connects with the URL `invite` printed.
        let deadline = Instant::now() + Duration::from_secs(30);
        let url = loop {
            if let Some(url) = hub.snapshot()?.offer_url {
                break url;
            }
            anyhow::ensure!(Instant::now() < deadline, "no invitation");
            std::thread::sleep(Duration::from_millis(50));
        };
        let laptop = Controller::open(root.path().join("laptop"), vec![])?;
        let mut joined = laptop.redeem(&url, "Laptop")?;
        while joined.pending.is_some() {
            anyhow::ensure!(Instant::now() < deadline, "no approval");
            std::thread::sleep(Duration::from_millis(100));
            joined = laptop.refresh_pairing()?;
        }
        assert_eq!(inviting.join().unwrap()?, 0, "invite reports the pairing");
        let names = |peers: &[Peer]| peers.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&joined.peers), ["Hub"]);
        assert_eq!(names(&hub.snapshot()?.peers), ["Laptop"]);
        // `okilum sync status` from yet another command, through the hub.
        let owner = control::Client::find(&hub_state)?.context("still serving")?;
        let status = Session::Owner(owner).perform(Operation::Read)?.status;
        assert_eq!(status["computer"], "Hub");
        assert_eq!(status["peers"][0]["name"], "Laptop");
        assert_eq!(status["invitation_open"], false);
        Ok(())
    }

    #[test]
    fn issue2589_sync_cli_stops_waiting_only_when_pairing_ended() {
        for message in [
            "pairing request rejected",
            "pairing invitation expired",
            "pairing request cancelled or changed",
            "invitation already claimed",
        ] {
            assert!(pairing_ended(&anyhow::anyhow!("{message}")), "{message}");
        }
        assert!(!pairing_ended(&anyhow::anyhow!("connection refused")));
    }
}
