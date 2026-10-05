# Tessera GUI information architecture

Design proposal, 2026-09-06. Synthetic content only. This reorganizes existing alpha capabilities; it does not certify native implementation or change the [approved scope](../../docs/ai-brain-alpha.md). The prototype is a disposable interaction model, not a framework decision.

## One workspace, two ways into the same work

The everyday entry is **Attention**: decisions, blockers, and saved outcomes. **Project brain** opens canonical notes. **All goals** opens the work those notes support. A source opened from a goal keeps a “Back to goal” path; a goal always exposes its selected sources and saved result as ordinary Markdown. Knowledge and execution share selection, provenance and navigation rather than separate home screens.

The persistent shell follows the user-supplied 1Password reference: a neutral sidebar, a persistent goal/source master list, and a roomy detail pane. A global toolbar holds search and New thought; the sidebar holds the project identity and navigation. Connections and Export brain are secondary destinations. Prototype tools are tucked into a secondary utility entry, outside the everyday task hierarchy. Root/host identity is available in workspace details; it is never an editable path field on a goal. A backend root is not a desktop filesystem path. The legacy read-only Reader remains an explicit separate workspace mode.

| Destination | Primary question / hierarchy | Objects and exits |
|---|---|---|
| Attention | What needs my decision now? Group blockers, review requests, final outcomes; routine progress belongs in history. | Goal title, reason, one concrete action. Selecting a row opens its exact goal/stage, not whichever goal was previously selected. Empty attention is distinct from an unavailable backend. |
| Project brain | What do I know, and where did it come from? Note list → selected source → source/preview. | Exact relative path, title, links, source bytes; Back to goal retains the owning goal. Ambiguous links open explicit choices. |
| All goals | Which outcome am I pursuing? Goal list → goal header → Discussion / Execution / Outcome. | Criteria, selected sources, linked Todoist task and next action remain visible together. Details disclose IDs, frozen revisions and historical events. |
| Discussion | What should we do with these sources? | Full saved conversation, selected source chips, composer. AI output is a proposal until a user-authorized operation occurs; sending does not also start T3. |
| Execution | What was delegated and what is happening? | Prepared context and next step, Start stage, exact T3 thread, stage state, current blocker, previous stages. Closing the desktop does not cancel backend work. |
| Outcome | What was produced and what is verified? | Saved result, cited sources, criterion-by-criterion evidence and review, previous results, explicit follow-up. Engine outcome and goal verification are separate facts. |
| Connections | What is unavailable and how do I restore it? | CLIProxyAPI / Todoist / T3 status, non-secret settings and credential reference; Check, Save and connect, Reconnect. No token entry in project content. |
| Export brain | Can I inspect these files independently? | Saved-file scope, draft exclusion, manifest diagnostics, archive download and verified transfer status. |

## Connected synthetic journey

The prototype's **Halenote · engineering copy** is invented demonstration data, not a view of a health project or an actual acceptance fixture.

1. Capture “Prepare the launch-readiness checklist”. Define an observable outcome: a source-grounded checklist, with a human review of its applicability. A second goal, “Document the offline capture boundary”, demonstrates separate ownership.
2. Open `README.md` and `engineering/launch-readiness.md` from Project brain; return to the selected goal and include them in Discussion. A long title wraps without hiding which source is selected.
3. Discuss the scope through a simulated CLIProxyAPI conversation. Create a simulated Todoist task, or link an existing task in the native implementation. Todoist owns task status; accepting a checklist does not complete a real launch.
4. Prepare the T3 stage. The context contains goal, criteria, selected source snapshots, decisions and next step. Start is a separate explicit action. Freeze context; subsequent source edits apply to future work.
5. Inspect active execution and its thread. A simulated transport uncertainty becomes an actionable blocker: recover the existing attempt. Never turn uncertainty into a fresh start.
6. Save the synthetic engine result automatically. Show **Engine succeeded · Goal needs review**. Read retained evidence, record required review, then show **Goal completed** without repeating the same confirmation.
7. Prepare a follow-up from the saved predecessor; prior stages and their results remain inspectable. Current attention belongs to the current stage, not a predecessor.
8. Open a source, edit exact Markdown, encounter a managed second writer's conflict, inspect current/base, and deliberately resolve. A newer incoming version invalidates the displayed resolution basis and requires another review.
9. Export saved Markdown and attachments as a knowledge archive. Explain any unsaved draft before exporting; extraction does not restore execution or credentials.

## Scope legend

| Current alpha contract and code surfaces | Proposed arrangement over alpha | Future vision, visibly labeled |
|---|---|---|
| One managed project; reusable goals and sequential stages; source and rendered preview; manual conflicts; saved connectors; results/evidence/history; exact archive. | Attention-first shell, combined goal tabs, source-return context, human-readable next actions, density controls and the illustrated component styling. These visuals are not installed in GPUI. | Cross-project/life-wide inbox; semantic retrieval; Live Preview; automatic independent-edit merge; AI context export; ok-gobot/mobile capture and attention; Maestro stages; Todoist replacement; general sync and full mobile editing. |

Search in this prototype filters fixture content. Native Reader search exists, but a unified remote goal/source search needs a separate implementation contract. A mobile concept may demonstrate capture/attention/reply only, not working offline synchronization. Alpha does not promise automatic recovery of unsaved desktop drafts after a crash.

## Layout and content priorities

The first proposal used too many colored panels and competed with its own content. The user's 1Password reference supersedes that presentation: use a quiet neutral sidebar, a separated master list and a broad white detail surface in light mode. Reserve a modest blue accent for the primary action, links and selection. Dark mode preserves the same hierarchy with neutral dark surfaces. Keep Noto Sans and Cascadia Code; this is an interface layout/surface correction, not a competing logo or typography direction.

The master list persists while the selected goal's Discussion / Execution / Outcome content changes. Project brain uses the same list/detail structure for notes. Selection belongs to the row and is clearly distinct from keyboard focus. A narrow row subtitle communicates state or parent path; the detail pane carries explanation, criteria and source content. Replace card grids and oversized introductory text with aligned sections, restrained dividers and a clear item heading. Evidence/details remain adjacent to the selected item, not a fourth always-open column.

At laptop width, reduce navigation/list widths before sacrificing the main reading area; at narrow or zoomed widths, use an explicit list/detail switch that retains selection and return focus. At 1024 × 768 no action requires horizontal scrolling of the whole app; Markdown code/table regions may scroll locally. At 1280 × 800 the goal identity, next action and verification status should be visible together. At 200% zoom all actions remain operable.

Default density favors reading; compact density reduces row spacing and chrome without shrinking reading text or focus targets. Long titles wrap in details and expose the full title on list focus; parent paths distinguish colliding filenames. Brand tokens stay immutable; GUI-owned interface aliases implement the user-directed neutral surfaces and restrained blue. See [brand requests](brand-requests.md) for the reason and verification boundary.

## Authority and open implementation choices

Sources are [PRD](../../docs/PRD.md), [foundation contracts](../../docs/ai-brain-contracts.md), [application API](../../docs/ai-brain-application-api.md), [workspace](../../docs/ai-brain-workspace.md), [connectors](../../docs/ai-brain-connectors.md), [export](../../docs/ai-brain-export.md), and the vault AI Brain vision loaded during activation. The vision supplies destination intent; the repository contracts govern alpha guarantees.

Native accessibility exposure, unified remote search, post-restart draft listing, and external writer exclusion remain engineering work or later decisions. No global provider-account switch or cancel action is implied: routing is pinned, and cancellation requires advertised engine support. These do not block reviewing the proposed layout.
