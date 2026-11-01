# Connect a second computer

## Before you begin

This guide covers the Linux preview with an existing, configured Syncthing runtime on both computers. The first computer's source vault must already be registered in that runtime. Keep both computers on the same LAN and prepare an empty destination folder on the second computer.

Fresh-host bootstrap is not ready. This guide does not install or start a runtime. If no supported connection is available, stop here; Help cannot prepare one. The demonstrated flow used synthetic test data. Keep a separate backup: Sync can propagate edits and deletions.

## 1. First computer: choose the source

Enable Sync in Settings → Features if needed, then open Settings → Sync. If more than one connection appears under Sync connection, select the intended existing runtime. Enter a recognizable name under This computer.

Select Choose existing synced vault and choose the source vault already configured in that runtime. Check the displayed folder, device and Syncthing connection. Select Use this connection only for the intended vault.

## 2. First computer: invite the second

Select Add computer, then Copy pairing URL. Transfer the invitation privately to the second computer. Keep this Settings window open. The invitation expires after 10 minutes; do not post the complete pairing URL publicly.

## 3. Second computer: request the connection

Open Settings → Sync. Choose the intended existing runtime if prompted. Enter this computer's own recognizable name under This computer, paste the invitation into Paste pairing URL, then select Connect.

## 4. First computer: confirm the identity

Compare the displayed name and computer identity with the second computer. Select Confirm computer only for the expected request; decline unexpected requests. The authorized vault catalog then appears on the second computer.

## 5. Second computer: choose an empty destination

Under Available vaults, find the intended vault and select Choose local folder. In the folder picker, choose an empty local folder and confirm. Unknown nonempty folders require a separate lossless import flow; do not delete existing files to force this step.

## 6. Wait for the first transfer

Wait for Bidirectional Sync active. Open the received vault and check a note and an attachment. Do not treat a paired-computer entry alone as proof that the first transfer finished. If the runtime or other computer is offline, restore that same connection and refresh; do not create a second daemon or replace its identity.

## Check both directions

Open a note, enter the editor, add a recognizable line and save with Ctrl+S. Open the same note on the other computer and confirm the line appears. Edit and save there to check the return direction. Switching Source and Live Preview is not a Save-button click.

## About the local video

The optional bundled video shows an already-connected pair. It does not record first-time pairing, folder selection or initial transfer. It is a silent 18.4-second v6 edit with English descriptions and a full transcript. A labelled cut removes 6.3 seconds of waiting; this is not real-time latency evidence or Oleg acceptance.

Opening Help changes no vault, pairing or service settings. Opening the local video guide writes only bundled Help files into Okilum's application state and opens their local page in your default browser. It needs no hosted page or media URL.
