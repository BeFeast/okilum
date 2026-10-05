# Clickable prototype demo

Open [prototype/index.html](prototype/index.html). Shared browser links and final evidence live in [handoff.md](archive/handoff.md). The prototype uses synthetic Halenote engineering content and mocked state only: it does not connect to CLIProxyAPI, Todoist or T3, save canonical project files, or export a real brain. Demo controls deliberately advance simulated states. Reload/reset starts a fresh demonstration; no desktop-crash recovery guarantee is implied.

The revised shell uses the supplied 1Password reference: neutral sidebar, persistent item list and roomy detail, with search/New thought in the toolbar. First inspect this structure at laptop width before opening secondary Prototype tools.

## Main journey (about five minutes)

1. Start in **Attention**. Identify what needs a decision, the project label and current goal. Open toolbar **New thought** (or N outside a text field), enter “Prepare the launch-readiness checklist”, and capture it. Check the title is visible on the resulting goal.
2. In **Discussion**, inspect the selected `README.md` and `engineering/launch-readiness.md` sources. Send a message about preparing a checklist from those documents. Observe the mock reply. No actual model call occurs.
3. If authentication is missing, open **Connections** and use **Save and connect** to restore the mock connection, then return to the same goal. Choose **Create Todoist task**, then **Prepare T3 stage**. Inspect the prepared goal/context/next step. Confirm **Prepare stage**, then choose **Start T3 stage** separately. The mock task and thread represent linked identities, not live provider resources.
4. Open **Execution**. Open **Prototype tools** at the bottom of the sidebar and use its execution controls to simulate a blocker. The stage may already be running; the offered path recovers/inspects the existing attempt. It must not suggest blindly starting another stage. Simulate a returned result.
5. Open **Outcome**. Read **Engine succeeded** alongside the unmet goal review requirement. Open evidence and prior-stage details. Record review explicitly; the completed state replaces the review action. Returning to this goal must not request that same acceptance again.
6. Prepare a follow-up. Inspect its predecessor result in history. Switch to **Document the offline capture boundary** and back; source/conversation/result selection must remain associated with the original goal.

## Source and conflict recovery (about three minutes)

1. Open **Project brain**, select `engineering/launch-readiness.md`, and edit Markdown. Inspect the draft preview. Navigate away while dirty: demonstrate Stay and explicit Save/Discard behavior without silent loss. Return to the goal via its source context.
2. Edit again, use the demo **external change** control, then Save (Ctrl/Cmd+S in source). Inspect your preserved draft, current saved version and genuine fixture base.
3. Edit a proposed resolution. Simulate a **newer incoming version**. Confirm submission is blocked pending a review of the latest comparison, with your draft intact. Review latest, then deliberately save the resolved draft. This models manual revision-aware resolution, not automatic merge.

## Recovery and export inspection

In **Connections**, demonstrate missing credentials and restore the simulated saved connection. Source reading and past results remain accessible. Inspect interrupted discussion: preserved partial text is labeled and reconnect does not resend it. In **Export brain**, read what is included and excluded, including unsaved drafts and execution state; demonstrate mock success and failure independently. The fixture .tar contains synthetic saved files and a SHA-256 manifest. Its download demonstrates portable fixture packaging; it does not call the production exporter or export a real workspace.

## Layout and keyboard review

Open **Prototype tools** for theme, density, reset and simulation controls. Use default/compact and light/dark. Inspect **State library** for empty, loading, error/offline and trust-state examples. Inspect a long title and long note at 1280×800 and 1024×768. At 200% browser zoom, reach the next action without app-wide horizontal scrolling. Tab from navigation through goal tabs, fields and actions; use Ctrl/Cmd+K, N outside inputs, Ctrl/Cmd+S in the editor, and Escape to close a transient surface. Check focus returns to the invoker and no editor keystroke activates capture. These are review instructions; completed checks are reported in handoff evidence, not assumed here.

**Mobile concept** is future capture/attention scope. It does not demonstrate a working mobile service, full editor, offline queue, sync, ok-gobot connector or approval channel.

## Mock boundary and review outcomes

All task/thread IDs, timestamps, messages, results, evidence, conflicts, connector states and archive phases are synthetic. A “Record review” click changes only the fixture; it is not Oleg's acceptance of the real product or a real goal. UI navigation/typing/state transitions are implemented prototype interactions; storage, network, dispatch, exact-byte conflict recovery and native keyboard/accessibility are not proven by them.

A useful review records which next action was unclear, whether source/goal identity was ever lost, and whether outcome status could be confused with verification. File native follow-ups using the [implementation map](implementation-map.md). Do not use the demo to operate or automate the live acceptance desktop.
