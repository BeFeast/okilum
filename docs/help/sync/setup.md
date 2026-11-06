# Connect a second computer

## Before you begin

This guide covers Linux, including two fresh installations. Install Okilum on both computers, open the vault containing your notes on the first, and keep both computers on the same LAN for first pairing. Okilum sets up its bundled Syncthing automatically when no existing connection needs attention. No account or server is required. Keep a separate backup: Sync can propagate edits and deletions.

macOS is coming in [#2489](https://git.oklabs.uk/BeFeast/okilum/issues/2489); Windows is coming in [#2490](https://git.oklabs.uk/BeFeast/okilum/issues/2490).

## 1. First computer: share your vault

Open the app menu → Share with another computer…. In Settings → Sync, find your vault under Local vaults or Vaults and select Share with my computers. It becomes Shared with approved computers. Use a recognizable name for This computer when creating the invitation.

If Sync finds an existing Syncthing, explicitly select Use connection. For a vault already synced by that runtime, select Choose existing synced vault, check the inspected folder and device, then select Use this connection. Okilum reuses that connection without starting a second daemon or taking over its lifecycle or network options.

## 2. First computer: invite the second

Select Add computer, enter This computer’s name, then Create pairing link and Copy link. Transfer the invitation privately to the second computer. Keep Okilum open here. The invitation pairs one computer and expires after one hour; do not post the complete pairing URL publicly. If it expires, create a new one.

## 3. Second computer: request the connection

On the welcome screen, select Get notes from another computer. This opens Settings → Sync and enables the Sync feature. Select I have a link from another computer, enter this computer's own recognizable name under This computer, paste the invitation, then select Request connection. If you already have a vault open, you can also use Settings → Sync. Choose Use connection if prompted about an existing runtime.

## 4. First computer: confirm the identity

Compare the displayed name and computer identities with the second computer. Select Approve only for the expected request; decline unexpected requests. The authorized vault catalog then appears on the second computer. Further authorized vault selections between this pair need no new invitation.

## 5. Second computer: choose the vault and local folder

Under Available vaults, find the intended vault and select Choose folder. In the native folder dialog, choose or create an empty local folder. After selecting the wanted vaults, select Start first sync. A non-empty folder is refused until the lossless preview and merge in [#2043](https://git.oklabs.uk/BeFeast/okilum/issues/2043); do not delete existing files to force this step.

## 6. Wait for the first transfer

The vault begins receiving automatically. Follow the first transfer until First sync complete; the received vault opens when it is ready. There is no extra Enable or daemon setup step. Do not treat a paired-computer entry alone as proof that the first transfer finished. If the runtime or other computer is offline, restore that same connection; do not create a second daemon or replace its identity.

## Check both directions

Open a note, enter the editor, add a recognizable line and save with Ctrl+S. Open the same note on the other computer and confirm the line appears. Edit and save there to check the return direction. Switching Source and Live Preview is not a Save-button click. Later saved edits and deletions sync automatically in both directions.

## Network and conflicts

Managed Linux Sync keeps explicit Syncthing peer addresses and uses public discovery and encrypted relays by default. Settings → Sync → Only local network and VPN turns off public discovery, relays and router traversal for every vault on the selected managed connection on this computer. Direct addresses must still be reachable over your LAN or VPN; first pairing always needs the same LAN. Change an external Syncthing's network options in Syncthing itself.

If an incoming change reaches a note with unsaved edits, Okilum keeps your text and shows a notice. Explicit Save or Keep both versions preserves your local text in a separate `.okilum-conflict-<time>-<random>` file, keeping the note extension and leaving the incoming file unchanged. Reading and clean editing views update when a changed file arrives. Sync conflict review offers Keep this, Keep that and Keep both (#2044). Managed folders retain previous remote versions for 30 days by default; use Note history to restore an overwritten note and Recover notes for a deleted one (#2045). This is not a backup of every local edit.

## Common questions

**Do I need an always-on server?** No. An always-on Linux computer running Okilum can receive a copy using these steps and act as another trusted peer. A Linux server can run `okilum sync serve` without a window. Pair through the running service with `okilum sync invite`. On a plain Syncthing server, share the vault explicitly, then edit the trusted sharing device → Advanced → Introducer. Leave Skip Introduction Removals disabled. New computers sharing that vault can then be introduced without repeated server configuration. Manually added devices still need manual removal; copied files remain on the removed device. Settings → Sync → Remove stops sharing with that computer and propagates through authenticated paired/introducer routes. Confirm its device ID first; disconnected computers follow when reconnected (#2042).

**Does disconnecting erase the other computer's copy?** No. Remove in Sync settings leaves that computer’s own files in place. Manually configured Syncthing relationships outside the introducer still need manual removal, and copied files cannot be recalled. Closing Okilum or disabling its optional Sync UI is not remote revocation.

**Do I sign in with a passkey?** No. The former service/hub/passkey desktop enrollment is gone (#2049). Pair directly with the URL and confirmation above.

## About the local video

The optional bundled tutorial shows first connection between two fresh Linux installations, folder selection, the initial transfer and edits in both directions. It is 1 minute 18 seconds long, with English narration, captions and a full transcript. Recorded live on beta 7538 (source 3a57fdfc) with fresh test users and firewalls on, it cuts waiting and speeds some steps up to 3×. One folder-dialog line is masked to hide the recording file name. The ElevenLabs Alice voice reads the caption text. This edited tutorial is not real-time latency evidence or Oleg acceptance.

Opening Help does not connect computers or change Sync settings. Opening the local video guide writes only bundled Help files into Okilum's application state and opens their local page in your default browser. It needs no hosted page or media URL.
