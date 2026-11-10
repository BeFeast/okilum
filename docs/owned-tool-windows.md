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
  caption content synchronously starts AppKit movement with the original event,
  with the macOS 27.2 mitigation described below. A double press delegates the
  system preference. Reader and Feedback retain
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
an inactive first press. GPUI core remains unpatched.
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

### macOS 27.2 drag handoff mitigation

Installed beta30561 verified the integrated caption and lifecycle on the tester's
macOS 27.2, but a fast input control retained one reverse WindowServer frame
sample (453 to438 points while the actual pointer moved843 to858). AX remained
stationary. Sixteen slower cases and an inactive-window control did not reverse.
These observations do not establish continuous perceptual smoothness or attribute
the sample to a specific cause. The original fast labels were not trusted: input
guards made them about1.6 seconds; separate controls measured about0.3 seconds.

A separate [Warp report](https://github.com/warpdotdev/warp/issues/16508) describes
intermittent `performWindowDragWithEvent:` failure on macOS27.2 beta4, including
A/B data for native handoff versus application movement. This is supporting
evidence, not proof of Okilum's exact failure. Apple documents that this handoff
returns immediately; the pinned backend's brief lock scope is not evidence of a
blocking native event loop.

On macOS27.2 only, Settings uses an app-local native event monitor for a drag
that began in its own blank caption. The original press records the native
window frame and screen point. Subsequent events use that fixed anchor plus
the latest absolute Cocoa screen pointer to set the same window's origin.
Window-local coordinates and accumulated deltas are never used. Native callbacks
run before GPUI event dispatch, avoiding native geometry callbacks while the
caption entity is borrowed. Close and other controls cannot start this path;
double-click still delegates the system preference. Other OS versions retain
the native WindowServer handoff.

Each monitor belongs to one live Settings window, uses a weak Rust capture and
is removed on teardown. A new press, release, wrong event window,
hidden target, lost left button, changed window size, missing display or invalid
geometry retires the drag. Movement uses the display under the pointer, keeps
the caption below its menu bar and retains a reachable caption segment at the
other edges. It changes no global monitor, input permission, preference, Reader,
Feedback, ownership relationship or live vault.

The scoped workaround does not participate in WindowServer edge tiling or
dragging through Mission Control/Spaces. Native double-click zoom and explicit
window actions remain available; ordinary movement across display coordinates
uses the pointer's target display. These limits must remain visible in delivery
notes until a native handoff fix allows removal of the workaround. Fresh beta
motion/video acceptance is required; coordinate and cancellation regressions
do not themselves establish runtime smoothness.

### First mouse during application activation

An original press accepted by this window can precede its key-window or app
activation transition. The Settings-local monitor therefore checks the native
event's window identity and visibility, rather than requiring `isKeyWindow`.
Keeping a key-window prerequisite would retire an accepted first drag before
activation settles. The pressed button and geometry/teardown controls still
apply; no activation request or foreground override is added.

Installed30605 recorded a complete different-process inactive input trajectory
with zero Settings movement, while same-process inactive movement worked. This
is the runtime regression control for the correction. It does not by itself
prove which native callback rejected the event or equate injected and physical
input. Its original trace and the failed30561 fast sample remain evidence.
Eight ordinary30605 traces also ended before mouse-up; they cannot establish
whole-drag behavior. Retesting must start recording before mouse-down, continue
through mouse-up and settling, and confirm the actual sampled pointer and both
geometry endpoints. Current Close/Esc/About/owner checks must not be inherited
from older builds after a native run stops early.
