//! Read-only macOS Sync status. Opening Settings cannot register, launch, repair,
//! adopt or stop anything. Missing authority/peer is visible recovery pending.
use super::*;
use okilum_sync_controller::sidecar::macos::recovery::{ManagedStatus, RecoveryDetail};

pub(super) fn shared(_: &mut Window, cx: &mut App) -> Entity<SyncSettings> {
    cx.new(|cx| {
        let mut view = SyncSettings {
            status: ManagedStatus::RecoveryPending,
            detail: RecoveryDetail::StateUnavailable,
            busy: false,
        };
        view.refresh(cx);
        view
    })
}
pub(super) struct SyncSettings {
    status: ManagedStatus,
    detail: RecoveryDetail,
    busy: bool,
}
impl SyncSettings {
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if !reader_features::enabled(reader_features::Feature::Sync, cx) {
            return;
        }
        if self.busy {
            return;
        }
        self.busy = true;
        // Do not retain a prior Running claim while a new observation is pending.
        self.status = ManagedStatus::RecoveryPending;
        self.detail = RecoveryDetail::StateUnavailable;
        cx.notify();
        cx.spawn({
            let module_access =
                crate::reader_features::access(crate::reader_features::Feature::Sync, cx);
            async move |this, cx| {
                okilum_t3::scope_network(module_access, async move {
                    let status = cx
                        .background_executor()
                        .spawn(async {
                            read_status().unwrap_or((
                                ManagedStatus::RecoveryPending,
                                RecoveryDetail::StateUnavailable,
                            ))
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.status = status.0;
                        this.detail = status.1;
                        this.busy = false;
                        cx.notify();
                    });
                })
                .await
            }
        })
        .detach();
    }
}
fn read_status() -> anyhow::Result<(ManagedStatus, RecoveryDetail)> {
    anyhow::ensure!(
        okilum_t3::NetworkAccess::current().allowed(),
        "Sync is disabled"
    );
    use anyhow::Context as _;
    use okilum_sync_controller::sidecar::{
        macos::{account_state_directory, connection::NativeConnection},
        store::UnixStore,
        supervisor::ipc::code_requirement::CodeRequirement,
    };
    let budget = Duration::from_secs(5);
    let state = account_state_directory()?;
    let store = UnixStore::open_existing(&state)?;
    let saved = {
        let tx = store.begin(std::time::Instant::now() + budget)?;
        tx.current()?.context("missing journal")?.clone()
    };
    let binding = saved.binding().clone();
    anyhow::ensure!(
        Path::new(&binding.state_directory) == state,
        "non-native state root"
    );
    let team = option_env!("OKILUM_SIGNING_TEAM_ID").context("no signing policy")?;
    let identifier = option_env!("OKILUM_BUNDLE_ID").context("no bundle policy")?;
    let own = std::env::current_exe()?;
    let executable = own
        .file_name()
        .and_then(|name| name.to_str())
        .context("no app executable")?;
    let app = own
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("no app bundle")?;
    let connection = NativeConnection::open(
        app,
        executable,
        binding,
        CodeRequirement::team_and_identifier(team, identifier)?,
        CodeRequirement::team_and_identifier(team, &format!("{identifier}.sync"))?,
        budget,
    )?;
    let status = connection.managed_status();
    Ok((status, RecoveryDetail::observe(&saved, status)))
}
impl Render for SyncSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = brand::palette(cx);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_base().child(if self.busy {
                "Checking Sync…"
            } else {
                self.status.label()
            }))
            .child(
                div()
                    .text_sm()
                    .text_color(p.text_muted)
                    .child(self.detail.description()),
            )
            .child(
                Button::new("refresh-macos-sync-status")
                    .label("Refresh status")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    }
}
