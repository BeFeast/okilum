# Retired pairing UI (#2049)

The 30 October decision in #2039 leaves one computer pairing flow: direct
Okilum pairing. Linux and macOS Settings share `reader_settings_sync.rs`.
The Settings entry, vault share menu, welcome screen and bundled Help use that
flow. Windows has no Sync Settings section or welcome pairing action. External
desktop URLs navigate notes; they cannot open enrollment or service Settings.

Inbox no longer mounts its retained folder pairing module, exposes its menu or
renders its approval dialog. Old `#sync` approval URLs display instructions to
use Okilum Settings instead. Inbox account passkeys/device invitations are a
separate feature and remain available. The service API, pairing implementation
and existing tests remain compiled and available for future uses.

The former Linux desktop persisted service setup at `sync/setup.json`, beside
the current `sync/direct` state. Active, unreadable or corrupt old setup blocks
direct controller opening and automatic daemon preparation, with this message:

> An older Sync connection is saved on this computer. Your files and connection
> settings have been kept. This version cannot finish or manage that connection.
> Existing syncing can continue while Syncthing is running. If notes are not
> arriving, check this connection in Syncthing on both computers before setting
> up a new connection.

This read-only boundary does not revoke enrollment, stop an existing daemon,
change its config, migrate files or claim that a pending enrollment completed.
Uninstall diagnostics preserve their refusal and no longer redirect users to
the retired Settings removal action. Bundled Help describes direct Sync settings.
An absent setup or a retired setup (completed old removal) permits the ordinary
direct flow. Do not delete a saved setup to bypass the warning: migration of
existing connections is separate work. macOS's former Settings showed native
runtime status, not service enrollment; its managed runtime continues to be
reused by the shared direct UI.

Regression coverage includes a real readable HubReady enrollment, unchanged
journal/config/note bytes across attempted desktop operations, missing/retired
and uncertain setup controls, the visible Settings warning, rejected external
enrollment URLs, and removal of Inbox's rendered and URL-based pairing entrypoints.
Existing direct welcome/offer/confirmation tests and service tests stay enabled.
