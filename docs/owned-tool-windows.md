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
  Settings/About use an opaque native AppKit title bar with standard traffic
  lights, native dragging and the system double-click preference (#2755).
  Their explicit Close button is in a footer, outside the caption. Reader and
  Feedback retain their existing chrome; owner lookup and lifecycle are unchanged.
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

On macOS Settings now delegates the whole native caption to AppKit without a
GPUI overlay or mouse-move handler. Linux and Windows keep their original title
bar and Close placement. The owner-scoping commits in #1957 (`2e94124f0`,
`718e0f01b`, `ce6ac797b`, `56f3fe6b4`) change routing/reuse, not caption input;
the native macOS child attachment predates them (`a7b1a9c50`). These relationships
are retained, including the Windows HWND owner from #1917.

Regression checks cover the macOS window options, absence of a custom caption,
Close via pointer/Esc/shortcut/action, per-Reader About reuse, and owner closure.
TestPlatform cannot simulate AppKit window dragging. Manual Mac verification must
still cover repeated slow/fast drags across the full caption (active/inactive
window, every Settings pane and About), native close, footer Close, and movement
of the owning Reader. Source inspection identifies the dead input strip above;
it does not substitute for reproducing the reported jumps on the tester's Mac.
