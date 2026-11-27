# Own-window capture for Feedback (#1502)

Scope (Oleg, 18.11): Windows only. macOS and Linux keep the #1469
attachments (picker, paste, drop); nothing captures a desktop there.

## Windows

The Feedback window shows **Capture Okilum window**. It reads the window
Feedback was opened from, never the Feedback window itself, the desktop or
another application:

- The HWND comes from that GPUI window. It must belong to this process and the
  calling UI thread; this is checked again right before the readback.
- `PrintWindow(PW_CLIENTONLY | PW_RENDERFULLCONTENT)` draws the window's own
  composed content into a memory DIB. GPUI 0.3.3 renders through a
  premultiplied flip-sequential swap chain on a DirectComposition target, with
  `WS_EX_NOREDIRECTIONBITMAP`; PW_RENDERFULLCONTENT is what reads that content.
  Occluding windows, the Feedback window included, do not appear.
- No permission prompt, clipboard, file or network use. The pixels go into the
  existing screenshot slot: preview, Remove, replacement by paste/drop, the
  8 MB / attachment-limit checks and the explicit Send all stay as they were.
- Failures are visible and attach nothing: minimized owner ("Restore the
  Okilum window"), a closed window, a too-large window, or a readback that left
  the buffer unpainted or entirely black (a failed composition read) — the
  message points to Win+Shift+S and paste.

`crates/okilum-shell-platform/src/own_window_capture.rs`.

## Evidence

Windows tests in `forgejo-native-shell.yml` (step "Run own-window capture
regressions"):

- GDI positive control: a known colour read back from a real HWND; the
  desktop, the shell tray, a null handle and another thread's call are refused;
  a minimized window is refused.
- DirectComposition positive control: the same swap chain and target setup as
  GPUI (D3D11 hardware, WARP on hosted runners), GDI drawing nothing; the
  capture must contain the swap-chain colour.
- `feedback1502_capture_targets_only_the_owner_window`: the button targets the
  Reader window and a failed capture attaches nothing.

Pixel conversion and the unpainted/black/size rejections are tested on every
platform. GPUI's test-only `render_to_image` is not used anywhere.
