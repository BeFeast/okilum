# GUI interaction and state specification

Proposed native behavior; the [prototype](prototype/index.html) simulates the selected subset described in the [demo script](demo-script.md). Existing API truth always wins over optimistic UI state. Do not infer persistence from an animation or elapsed timer.

## Presentation after the reference-driven revision

Navigation is a neutral sidebar → persistent goal/source master list → selected detail. The global toolbar contains search and New thought; simulation controls sit under secondary Prototype tools. A row opens its detail without replacing the master list. Switching goal tabs keeps row selection and the item heading stable. Use quiet section dividers rather than a dashboard of competing cards. Primary blue is reserved for the current action; warning/error colors communicate actual state. These presentation changes preserve all trust rules below.

## Selection and transitions

Selection identity is `(workspace, goal, stage, source path, conversation)`. Async replies update their originating entity only. A late reply must not move the visible selection or attach a result to another goal. Opening a source from a goal preserves return context. Switching among retained workspace surfaces preserves editor entities.

The primary action follows the server projection: Capture → Discuss/create or link task → Prepare stage → Start prepared stage → Inspect running work → Review saved result → Read completed result / prepare explicit follow-up. A pending task command or indeterminate stage replaces the normal action with recovery. One stage at a time per goal; a follow-up requires a terminal predecessor and its retained result.

## Trust-state matrix

| State | Visible copy and retained content | Permitted action / transition |
|---|---|---|
| Empty project / no goals | “Capture your first thought”; no invented activity counts. | Capture. Missing connection is shown separately. |
| Loading | Named operation, stable goal/title, loading text; keep known saved content. | Navigate only where ownership and drafts are safe; no blank-state replacement. |
| Backend unavailable / offline | “Backend unavailable. Last observed …”; state freshness is explicit. | Retry connection, inspect retained content if available. No claim of durable offline capture or current remote status. |
| Operation error | Concrete action failed, brief reason, preserved draft/result. | Appropriate retry only if safe; technical details expandable. |
| Missing credential | “Authentication required” on the affected connector. | Open Connections/reconnect reference. Other sources and historical results remain accessible. |
| Identity mismatch | “This connection belongs to a different workspace/account.” | Restore original connection; never silently adopt it for pending work. |
| Dirty source + navigation | Dialog identifies the source and unsaved changes. | Stay; Save and continue after acknowledged save; Discard draft and continue. Failed save/conflict keeps the dialog's intended destination pending, never discards. |
| Interrupted discussion | Saved partial visibly separated from completed messages; “Reply interrupted. Nothing was resent.” | Read partial; explicitly send a new message. Reconnection, focus or reopening never sends. |
| Discussion already running | Show saved/streaming output and busy composer state. | Inspect; do not submit a second message to the same running conversation. |
| Stage running / desktop reconnect | Same stage/thread shown with observation freshness. | Open exact thread, inspect details; no second Start. |
| Stage start uncertain | “Start not confirmed. Work may already be running.” | Reconcile original operation; retry only on proven not-started or provider-proven idempotent replay. Timeout/process absence is not proof. |
| Engine succeeded / goal incomplete | Separate engine outcome and unmet criterion list; result opens immediately. | Inspect evidence, evaluate criterion, record required human review, or prepare follow-up. Never auto-pass criteria. |
| Prior result retained | Prior stage heading and immutable receipt, clearly historical. | Open result/context. Does not replace successor attention or accept a newer result. |
| Goal completed | Saved result plus recorded review provenance; no repeated acceptance CTA. | Inspect/export; explicit follow-up. Todoist task status stays independently visible. |
| Export preparing/transferring | Named phase; file destination once selected; draft exclusion visible. | Do not show success until full length/hash verification and local publication. |
| Export succeeded | “Archive saved”; file location and manifest counts. | Show file; inspect exclusions/unresolved references. No “execution backed up” claim. |
| Export failed | “Archive not saved”; reason (source changed, destination exists, transfer failed). | Retry a new export. Failed partial is not offered as completed archive. |

## Source editing and conflict resolution

Source and preview are two views of the same draft. Preview is never the byte source for saving. Keep unchanged frontmatter, BOM, line endings and wikilinks intact. Draft preview is labeled as unsaved. Ambiguous links show candidates with parent paths; unresolved links explain the missing target rather than guessing. Missing assets have a labeled placeholder.

On stale save, keep the draft editable and show **Your draft**, **Current saved version**, and **Base version** read-only when a real base exists. If base is unavailable, say so. Changed-line indicators supplement full readable versions; they do not claim automatic merge. “Save resolved draft” submits against the exact current version displayed. The action explains that it replaces that current saved version with the edited resolution, while retaining conflict evidence.

When a newer incoming version appears, preserve all draft text, mark the current comparison stale, show “Review latest version”, and disable submission until refreshed comparison is reviewed. A further race during submission creates another conflict. An indeterminate save keeps its frozen operation and payload for reconciliation; it never fabricates a new write to dismiss uncertainty. Deleted or non-UTF-8 current versions remain recoverable conflicts; this text editor does not recreate or normalize them automatically.

**Discard draft** is the destructive in-session choice: it states which unsaved text is lost and receives no default focus. The saved source is not deleted. **Keep editing** is the initial safe action. Choosing “use current” must explain that it replaces the local draft only, and require an explicit discard choice. A future conflict-history browser must not be suggested as an existing alpha feature.

## Connections and export

Connector cards distinguish unconfigured, configured/untested, reachable, disconnected, authentication-required and identity mismatch. Check fetches discovery/account evidence without starting a task or thread. Save applies non-secret configuration; Reconnect rereads saved credential references. No secret value is displayed or included in exports. Provider account/target routing remains pinned even with completed history; another target requires a separate managed workspace. Model/mode changes must respect all goals' nonterminal work.

Export states saved-file scope before download. With a dirty source offer Save first or Export saved files; the latter explicitly excludes the draft. Preserve the draft through export. Destination collision fails rather than overwrites. The exact archive carries relative paths/revisions and canonical records; exclusions and unresolved/external dependencies are manifest facts, not proof every linked item is included. Export never resumes execution in an extracted copy.

## Keyboard and focus contract

| Input | Intended behavior |
|---|---|
| Tab / Shift+Tab | Logical visible order: global toolbar → sidebar → master list → selected detail controls → content → next action. No hidden/offscreen control receives focus. |
| Enter / Space | Activate focused button. Enter in multiline source/composer inserts a newline. No destructive operation through an unmodified text-editor key. |
| Ctrl/Cmd+K | Open search; initial focus in query; Escape returns to invoking control. Fixture-only search in prototype. |
| N outside editable controls | Open Capture and focus thought input. Does not steal a typed letter from source, composer or search. |
| Ctrl/Cmd+S in source | Save exact draft, or open/retain conflict workflow. Prevent browser Save dialog in prototype. |
| Escape | Close transient drawer/dialog safely; restore invoker. Never discard an unsaved draft or cancel backend work. |
| Tabs / radio groups | Native arrow-key semantics where represented as such; selected and focus state separately visible. |

Modal entry focuses its heading or first safe field; background is inert, focus cycles inside, and closing restores the invoker. Inline errors are programmatically associated with fields. Result arrival uses a polite announcement without stealing focus or scrolling a reading user to the bottom. Blocking failure may use an alert once. Streaming text must not announce every token. A source conflict focuses its summary on arrival from an explicit save; background polling does not repeatedly steal focus.

## Button system

Use a consistent control height, horizontal padding and modest radius across each density. The current main action within the detail is blue; global New thought is a separate capture action. Secondary actions use a quiet neutral fill or text treatment, and toolbar actions pair an icon with a readable label. Avoid wrapping every action in a dark border. Distinguish hover from selected and focused; focus must remain visible on both filled and quiet controls. Disabled/busy controls retain their label and explain why an operation is unavailable. Destructive actions use explicit wording and an intentional danger treatment, separated from the primary path; a generic red button is not an explanation of conflict consequences.

Verify button relationships on the actual toolbar, goal, connector, export and conflict screens in both themes: sizing and spacing should be consistent, and the primary action should be identifiable without reading every control. This is a required visual review, not established by the earlier token contrast audit.

## Component library and accessibility targets

Reusable components: global toolbar/search; workspace/nav item; master-list row with selection and subtitle; detail heading/section divider; attention row with reason/action; goal header and status pair; source chip with path; criteria/evidence row; conversation message/partial; stage timeline; next-action panel; source/preview split; conflict version panel; connector status card; archive receipt; confirmation dialog and inline error. Every component has normal, focus, disabled/loading and applicable error states in the State library. Status uses words and icon/shape as well as color.

Targets for both density modes and themes: body text contrast at least 4.5:1; large text and essential UI boundaries/focus at least 3:1; visible 2px focus ring with clearance; default targets approximately 40px, compact minimum 32px and never below 24px; readable body sizing preserved in compact. Keyboard-only use, 200% zoom, reduced motion and long text are required review cases. Reduced motion removes animated repositioning; status information remains available as text.

These are design acceptance targets, not claims that native GPUI exposes a complete accessibility tree. HTML semantics and keyboard evidence do not prove native screen-reader support. Record actual prototype checks separately; native implementation requires its own user-visible evidence under existing quality gates.
