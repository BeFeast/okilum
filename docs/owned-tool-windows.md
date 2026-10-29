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
- Linux: GPUI Floating requests `WM_TRANSIENT_FOR` on X11 or an xdg-toplevel
  parent on Wayland. GPUI chooses the focused window as the transient parent;
  when invoked from another tool that may be a sibling in the same family.
  There is no public API to reparent a Wayland toplevel explicitly. Window
  managers/compositors control transient stacking, minimize/restore and taskbar
  presentation. Wayland cannot guarantee an application's requested absolute
  screen position, so exact centering cannot be guaranteed there. GPUI lifecycle
  cleanup, Esc and singleton behavior apply on every platform.

No GPUI core or vendor changes are required. Regression tests cover real GPUI
Settings/Feedback windows and native Windows ownership, styles and lifecycle.
Native desktop presentation remains subject to the project's normal human/batch QA.
