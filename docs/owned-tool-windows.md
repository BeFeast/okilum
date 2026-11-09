# Owned non-modal tools (#1582)

Settings and Feedback remain usable alongside the main window. A tool belongs to
the main window that opened it, including commands invoked from another tool.
Closing a tool leaves the main window usable; closing that main window closes its
tools. An unrelated main window remains open.

Settings retains one window per main window and its selected pane on ordinary
reopen. Feedback retains one window/draft per main window. Reopening focuses the
existing window; Esc closes it. The existing explicit About route still selects
About. Settings keeps its original owner while that tool remains open. Another main
window opens its own Settings/About instead of activating the first owner.

New tools use their owner's display and open centered over its bounds, constrained
to that display's visible work area. They cannot be minimized independently.

- Windows: an explicit native HWND owner and `WS_EX_TOOLWINDOW`, without
  `WS_EX_APPWINDOW`, keep the tool above its owner and out of the taskbar/Alt+Tab.
  Ownership is installed before the first show. Windows handles owner
  minimize/restore; GPUI also removes the child when the owner closes. The owner
  is never disabled and no modal session is started.
- macOS: a native child-window relationship, at the owner's normal level, keeps
  the tool above its owner without application-wide floating/topmost behavior.
  It is excluded from the Window menu; the app retains its single Dock identity.
  Settings/About use one transparent client caption with standard native traffic
  lights and the explicit Close action (#2755/#2770). A left press on blank
  caption content synchronously starts AppKit movement with the original event;
  a double press delegates the system preference. Reader and Feedback retain
  their existing chrome; owner lookup and lifecycle are unchanged.
- Linux: GPUI Floating requests `WM_TRANSIENT_FOR` on X11 or an xdg-toplevel
  parent on Wayland. GPUI chooses the focused window as the transient parent;
  when invoked from another tool that may be a sibling in the same family.
  There is no public API to reparent a Wayland toplevel explicitly. Window
  managers/compositors control transient stacking, minimize/restore and taskbar
  presentation. Wayland cannot guarantee an application's requested absolute
  screen position, so exact centering cannot be guaranteed there. GPUI lifecycle
  cleanup, Esc and singleton behavior apply on every platform.

Settings uses the `okilum-settings` app id and an opaque native background on
every page (#2615). On Hyprland with the Lua API, the app installs an idempotent
runtime opacity rule for that app id only. Omarchy's default 0.985 active / 0.96
inactive compositor opacity otherwise reveals the owner document even through
opaque GPUI surfaces and modal dialogs. No user config file is edited; Reader
windows retain their compositor settings. Other compositors use the ordinary
opaque surface; older Hyprland versions without Lua log the unsupported request.

No GPUI core or vendor changes are required. Regression tests cover real GPUI
Settings/Feedback windows and native Windows ownership, styles and lifecycle.
Native desktop presentation remains subject to the project's normal human/batch QA.

## Settings caption regression (#2755)

`39caa6e39856d1095c7f7c295162e17227263bcb` moved Close from the footer
into the custom caption and intercepted left-button presses on its entire
full-height row, including blank padding. Those pixels cannot start a title-bar
drag. The custom title bar introduced by `7c73ea4f7` also starts the native move
only on a subsequent mouse-move event over the bar; macOS hands that current
event to `performWindowDragWithEvent:`. A short caption therefore depends on
where the next pointer event lands instead of handing the original press to
AppKit. The main Reader uses the same custom handler, with a taller header and
different controls, so its working drag does not establish Settings correctness.

PR2756 used an opaque native caption above a separate client Close toolbar.
The tester rejected that grey strip and reported continued jumping in #2770 on
a build containing the change. Historical manual evidence included ten drags,
but two reverse WindowServer samples and no inactive-first-press coverage;
it did not establish smooth native motion.

Settings/About now use a single 34-pixel client caption over a transparent native
titlebar. Native traffic lights retain their standard positions. Blank content
and padding start native movement synchronously on left mouse-down, including
an inactive first press. No flag waits for another mouse-move, no synthetic
position or frame updates implement movement, and GPUI core remains unpatched.
The pinned GPUI macOS backend synchronously dispatches this input and its public
`start_window_move` hands `NSApplication.currentEvent` to
`performWindowDragWithEvent:`; invoking it inside the press callback preserves
the original event. A second press delegates `titlebar_double_click` once.

Only the exact Close-button hit region stops the press from bubbling to the
caption. The surrounding padding remains draggable. Close stays in the first
34 client pixels that Dialog reserves for window actions, including the Sync
wizard backdrop; it still uses the normal blur/commit/close path. The original
per-Reader native child relationship, ownership routing, About reuse, Esc and
other platforms' chrome stay intact.

Regression checks assert transparent/client-owned caption options, immediate
move dispatch before any mouse-move, inactive-first-press routing, blank padding
and tight Close hit regions, a real Close click with no move dispatch, single
double-click delegation and actual Settings/About/Sync modal Close/ownership
behavior. TestPlatform has no AppKit window: a test-only recorder at the native
dispatch boundary observes requests instead of calling its unimplemented move
method. These controls do not certify native motion or reproduce a jump.

Actual containing-beta Mac acceptance must record repeated slow/fast real-input
drags across caption positions, including inactive-first-press, simultaneous
pointer/window-frame traces and scoped screenshots/video. Check native traffic
lights, client Close/Esc, modal typed-field commit, About reuse and owner closure.
A static screenshot, hosted request-routing controls or asynchronous geometry
sampling alone cannot establish smooth movement; retain any observed reversals
or untested paths explicitly until the reported behavior is verified.
