# Compact read-only Reader (#321)

The document is the default surface. Notes and Backlinks are explicit, mutually
exclusive panels and start closed. At Reader widths of at least 1000px a panel docks beside the document; narrower
windows overlay that panel instead of squeezing the document to zero width.
Both panels have a draggable toolkit resize handle and independent preferred
widths (initially 280px). Docking reserves at least 480px for the document.
An overlay leaves 48px exposed. Width clamps use measured Reader bounds and
never overwrite a preference merely because the window was resized. Close/Escape restores focus to the live TextView. Selecting a note or
backlink keeps its panel open, docked or overlay (#363); focus moves to the
document so Back/Forward stay immediate. Panels close only on an explicit
toggle, close button or Escape. Ctrl+K
reveals and focuses note search; Ctrl+F (Cmd+F on macOS) remains find-in-note, including from focused search Input.

Panel state owns no content, selection or navigation state. The document child and
TextView entity stay mounted when panels toggle or the window crosses the dock
threshold. Existing typography, rendering, history and heading landing remain in
use. Tests exercise real GPUI focus/content/scroll/history preservation as well as
the panel state/placement rules; native evidence is still a separate gate.

A local-only workspace uses compact Workspace/Appearance controls instead of the
permanent disabled Brain navigation and New thought action. Managed workspace
navigation is unchanged. Native TitleBar traffic-light clearance is retained.

#327 owns file/folder open actions, root selection, constructor/cache/index behavior
and OS open events. Its Open file/Open folder controls and inspectable chosen root
must survive integration. #321 does not introduce a second onboarding or open flow.
Derived-cache invariants are accepted with #327, never inferred from the old base.
Native acceptance covers 640x720 and 1366x768 on an explicitly leased desktop with
disposable canonical notes hashed before and after, then restoration of user state.
No Mac GUI acceptance is implied by Linux tests or artifact creation.

`Reader::embedded_in_workspace()` changes presentation only: Workspace supplies
the title bar and Open controls; Reader retains one document/control row. The
document title exposes the full chosen root in its tooltip. Standalone Reader
retains its title bar and Open controls. Entry Open buttons use normal primary
styling; toolbar Open buttons remain compact, with the same picker handlers.

Widths are saved on drag completion to application config `tessera/reader-layout.json`,
not canonical notes or their derived index. A configured path inside the selected
vault is refused. Tests drive actual mouse down/move/up through the toolkit handle
and verify persisted widths plus retained TextView/history. Native acceptance must
prove drag, resize and restart restoration on a fresh pinned build; c9b0b53 fixed
width native acceptance was stopped and is not a passing composition.

Window resize must use current viewport geometry on its first layout frame. The
Reader pairs measured body bounds with their viewport width and applies the
viewport delta before choosing dock/overlay widths. Reusing the previous width
can temporarily lay out a docked document across the entire window; a short
remeasure then clamps ListState to the top before the correct panel layout returns.
This repair prevents the invalid intermediate layout. It does not create a second
scroll store, dispatch navigation, replace content, or modify the toolkit. A
1600 → 600 → 1600 regression checks a nonzero block/offset on the same entity,
root, document and history while the preferred widths exceed the compact clamp.
