# Own-window capture (#1502)

The shell library exports `own_window_capture::capture_own_window() ->
Result<PngBytes, Unsupported>`. Call synchronously from the UI thread while the
Reader is foreground, before opening an attachment picker. It returns PNG bytes
in memory, never a filename. It does not send feedback, touch the clipboard,
write files, or register permission grants. Feedback integration follows #1505;
preview/removal and explicit Send remain the caller's responsibility.

- macOS 15+: Unsupported (attach-only). The supported ScreenCaptureKit
  replacement requires **Screen Recording authorization**. Per this slice's
  least-permission boundary, the module never requests or uses that capability,
  enumerates other applications, or synchronously waits for async callbacks on
  the UI thread. It returns immediately, including when a grant already exists.
- macOS 11–14: optionally resolves the legacy CoreGraphics own-window API,
  which does not require Screen Recording for windows of this process. It
  captures only the current NSApp key window, including GPU surfaces. This API
  is never invoked on macOS 15+. No NSView bitmap-cache fallback is used.
- Windows: the foreground HWND must belong to the current process and calling
  UI thread. PrintWindow reads its client area with PW_RENDERFULLCONTENT.
  A failed or unpainted readback returns Unsupported; no desktop-DC fallback.
- Linux X11: XRes 1.2 supplies the focused window's real client PID. The module
  walks owned parents only and uses XGetImage on that window. XRes absence,
  foreign focus, unmapped windows and X errors fail to Unsupported. A short
  server grab protects the identity/readback sequence; encoding happens after
  releasing it. X11 does not promise useful pixels for obscured window regions.
- Wayland, including an XWayland DISPLAY inside a Wayland session: Unsupported,
  meaning attach-only. No desktop capture or crop.

Images are limited to 16 megapixels and 8 MiB encoded. This cap is for capture;
the eventual Feedback caller must still enforce its aggregate attachment budget.

## Evidence

Linux hosted CI runs an isolated Xvfb positive control with a known-color native
window and rejects the server-owned root. Windows hosted CI verifies real
PrintWindow pixels and rejects the desktop HWND. Shared tests decode PNG pixels
and reject malformed/oversized dimensions. macOS hosted CI runs a main-thread
NSWindow with a Metal-rendered color fixture. If capture is available, its center
pixel must match that GPU frame. An explicit permission/unavailable result records
attach-only, **not verified native capture**. No capability is inferred from GPUI's
test-only render_to_image. Native availability on an operator's machine is not
inferred from another platform's CI.
