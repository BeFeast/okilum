# Linux Manager audit (#2122, 2 November 2026)

Initial baseline: main `715a971e43ea01dfb8a8552a975c229f398e7b0a`.
The final implementation is rebased onto `c608353a8`, including #2410 and
the H=github agent-check workflow (#2406). Native evidence remains explicitly
pinned to the compiled source below; the four changed runtime files have
identical SHA256 fingerprints after the rebase.
This audit covers the issue body and its model/title/composer/history comment.
The whole issue remains open: the thread-navigation scope changed in #2203,
and implementation, automated regressions and native/human acceptance are
separate results.

## Desktop handoff

The external protocol is `t3code://threads/<environmentId>/<threadId>`.
`app/#/...` is the renderer's internal route, not a parsed external thread link.
Okilum already uses the correct producer in main, including authenticated
`server.getConfig` discovery rather than the configured local alias. The GPUI
regression `open_in_t3_loads_native_identity_and_only_dispatches_the_exact_desktop_thread`
exercises the real Manager button and verifies that URI.

Independent Linux acceptance uses a copied, unmodified T3 AppImage
`0.1.7-nightly.20261102.4066` (SHA256
`3a57c1b92e6ce63f22181dd1edd9df893bdfab68d168f1bd63a7e1ad133c710c`).
Its packaged parser (`main.cjs:160165`) accepts `threads/<env>/<thread>`;
`DesktopClerk` forwards startup arguments and `second-instance` arguments to
`DesktopWebLinks`. The packaged `.desktop` has `MimeType=x-scheme-handler/t3code`
and `Exec=AppRun %U`. The runtime registration in the disposable profile instead
points `%U` at its isolated launcher, preserving the exact URI arguments.

Tests run under the whole-session GUI lock on muninn in a private headless sway
compositor, D-Bus session, XDG directories and `T3CODE_HOME`. Only two empty QA
threads are created in its local server. No installed Okilum or working T3
session is opened, stopped or modified. Screenshots contain one owned window.
An additional owned foot window supplies both a focus control and a positive
control for the window detector.

- Warm protocol handoff navigates from `qa2122-before` to `qa2122-target` in the
  same process/window without another desktop or browser window.
- Cold handoff first terminates only the disposable T3, proving its process and
  window are absent while the control remains. `xdg-open` starts T3 at the exact
  target despite a different thread being selected before shutdown. The new
  window receives focus; the control remains, with no extra window.
- Upstream Wayland limitation: on warm handoff, the route changes but sway keeps
  focus on the control window. Packaged `ElectronWindow.reveal`
  (`main.cjs:95248`) calls `window.focus()` on Linux; its additional foreground
  handling is Windows/macOS specific. This is a focus limitation, not a thread
  parser or single-instance routing failure.

Okilum supplements URI dispatch with off-thread sway IPC. It focuses only one
window whose `app_id` exactly matches `com.t3tools.T3Code`, including floating
windows. It does not choose by title or pick between multiple windows/profiles.
Each IPC command has a one-second deadline. Other compositors keep their normal
registered-handler activation. The registered launcher must still select the
intended T3 profile. The fallback neither launches T3 nor creates a browser
window; the existing registered URI handler owns startup and navigation.

## Native acceptance

The compiled Okilum QA artifact is source
`fa810be1cfda44af42b62af51611de22c0d8c22a`, SHA256
`9ad112c24416af2796fb009beb38da9ce49d9d6c99cae9a5629a3c1fd6f32488`.
A private Wayland virtual pointer was positively controlled by clicking T3's
other thread. No host input device or installed application was controlled.

The actual Manager button was tested twice against the **same** running T3
PID 2145275 / sway window 9, on the same compositor and machine:

- [Fallback disabled](https://git.oklabs.uk/attachments/ab084edc-4b7e-4703-bb5f-9be69e19af1d):
  the correct thread appears in the main pane, but focus stays with Okilum.
  Only `SWAYSOCK` was omitted from the QA app's environment; GPUI's own Wayland
  activation-token path remains intact. GPUI does not read `SWAYSOCK`.
- [Fallback enabled](https://git.oklabs.uk/attachments/d86816bf-6ba5-4d95-918c-8088a51cb7a6):
  the actual click navigates and focuses that same T3 window. There are three
  owned windows before and after; no extra desktop or browser window appears.
  [Settled T3 frame](https://git.oklabs.uk/attachments/75cf8927-27ac-4d6c-9735-151eb46784f8).

The [actual-button cold test](https://git.oklabs.uk/attachments/d73c86e5-43fd-4542-b6a3-a866b9887909)
first selects a different T3 thread, stops only the QA T3 and proves its process
and window absent. Clicking Manager's button starts PID 2159402 / window 12
with the requested thread in the main pane and focus. Exactly one expected T3
window is created, with zero extra windows. No T3 window existed for the sway
fallback to select at dispatch. [Cold frame](https://git.oklabs.uk/attachments/a91fa142-24a1-4842-b77b-163d872308ec).

A synthetic PNG was uploaded to the isolated server (HTTP 204); a subsequent
signed download returns HTTP 200 and the same 926 bytes / SHA256
`7fe1d295da6e0f85e23abf025ae759383d9dfccfdf5ef78701c01776e211ae2a`.
A durable uploaded draft, without a runtime local source, restores its
[thumbnail after restarting Okilum](https://git.oklabs.uk/attachments/2277eb5e-12ea-4d92-bd44-caf4d24a5d75)
and opens the real [full image viewer](https://git.oklabs.uk/attachments/a0a37b21-9842-4223-adff-7c7ff244d182).
The exact asset reference and text remain unchanged across both launches.
This does not claim chooser/drop/paste/Send or provider-byte acceptance.

Both QA threads finish with zero runs, messages and pending requests.
[Cleanup](https://git.oklabs.uk/attachments/686615cf-d14a-4710-89e9-2feff61305cb)
stops owned GUI/input processes, removes the private credential/profile/signing
keys and retains the user's original scheme registration. The GUI lock is
released. Evidence is retained under
`~/work/qa-evidence/issue-2122-20261102/` on maestro and muninn.

## Per-item parity

| Requested item | Current implementation / disposition |
| --- | --- |
| Markdown messages: code, lists, links | Shared Reader Markdown in `project/feed/content.rs`; selectable text, code Copy, scoped local links and external links. Feed tests exercise actual Copy/link actions. |
| Collapsed tool-call activity | `project/feed/groups.rs`, `feed/native.rs` and `feed/details.rs`: collapsed activity plus source/revision-aware detail loading. Missing/truncated detail remains explicit. |
| Answer questions / approvals in panel | `project/native/actions`: question options/custom answers, exact approval decisions, pending-response recovery and connection/thread guards. Two existing question-click scenarios still fail in the full Project run; this is not a complete parity PASS. |
| Stop / interrupt | Native exact-run interrupt in `project/native/actions.rs`, visible only for an active run/request. |
| New thread | Deliberately removed from the embedded Manager by #2203. Panel is bound to the configured Manager. Original #2122 parity remains unmet under that original scope. |
| Thread switching with search | Same #2203 override. Project switching still changes the configured Manager; other threads/search live in T3. Reintroducing in-panel navigation requires resolving that scope decision. |
| Image attachments | Current main has Attach/paste/drop, binary upload, draft tray and history preview. This PR restores the missing uploaded-draft thumbnail/viewer after restart, keeps exact references and adds explicit download-only Retry preview. Lost local sources remain unsendable until reselected or removed. |
| T3-like typography, no technical strings | Current `project/design.rs`, header, feed and composer share a compact scale; operational details are behind Diagnostics. The mounted-header alignment regression still fails; whole-panel visual acceptance remains open. |
| Model selector failure / perpetual Loading | Bounded thread/catalog reads, lifecycle guards, explicit Apply and confirmed readback already in main. `models/tests.rs` covers refresh independently of send readiness. |
| Duplicate thread title | #2203 removed the thread dropdown; the header says Manager and the conversation title has one location. |
| Single-line input | Current composer grows with content; Enter sends, Shift+Enter adds a newline and IME confirmation does not send. Empty one-line appearance is intentional after #2236. |
| Scrolling hides previous messages | Virtualized retained feed, anchored older-page prepend, follow-only-at-bottom and Jump to latest already in main. Actual wheel/page regressions cover both scrolling and arrival of new messages. |

The restored-preview tests download a confirmed synthetic asset, open the actual
thumbnail/viewer, retain the exact send reference on download failure, retry
only the download, reject cached pixels after row/environment/client changes,
and keep a lost local source unsendable. Transport and provider delivery are not
inferred from a thumbnail or a generated design reference.

## Automated checks

On source `fa810be1cfda44af42b62af51611de22c0d8c22a`, build-slot Linux checks
pass all six attachment regressions, both desktop protocol/focus regressions,
and the native Manager button/server-identity regression. The restored-preview
fixtures wait for the actual durable draft lifecycle; previews also refuse to
start or publish before that draft is ready.

The wider Project suite is **not green**: 271 pass, five fail. The remaining
failures are in unchanged paths: durable prompt recall, two current-question
click scenarios, mounted-header alignment and a retained send/retry test-scheduler mismatch. Earlier CI
run 20438 (`shared-local`, source `35602880ce7628049784b9af21f63858b5eba771`)
already reports the recall and question-option failures without this patch.
These results do not establish every failure's root cause, and are not relabelled
as native or whole-issue acceptance. The launch/executor failures from the first
run no longer reproduce after the rebase including #2410.

The slot builds the native QA artifact successfully (no OOM). Targeted strict
clippy is blocked by existing lints in `okilum-sync-controller` (unused trusted
preparation helper, `as_ref`/`as_mut` trait-name ambiguity) and `okilum-core`'s
remote-log fake (`manual_is_multiple_of`). The hosted Linux run
[41537049472](https://github.com/BeFeast/okilum/actions/runs/41537049472)
independently fails at those remote-log lints before the Rust suites. No lint
allowance, test skip or CI threshold was added by this PR. The normal required
PR checks and the source-pinned H=github agent checks remain separate statuses.

## Remaining work proposed to the owner

- Resolve #2203 against #2122, then restore scoped New thread and searchable thread switching if the original embedded-navigation requirement is retained.
- Repair native question-click/option regressions and verify the full questions/approvals loop against an isolated provider.
- Investigate durable prompt-recall persistence and correct the retained send/retry test’s mixed real-I/O scheduling, then verify restart/reconnect without duplicate dispatch.
- Finish native attachment acceptance: real chooser/paste/drop, Send/provider byte identity, retry and restart on the shipping Linux build.
- Repair mounted-header alignment and accept the complete native Manager composition and live interaction loop on a shipping build: Markdown/activity, questions/approvals, Stop, model selection and retained scroll at normal/narrow widths.
- T3 upstream: carry Linux Wayland activation through the single-instance URI handoff so Electron reveal transfers compositor focus without an application-specific sway fallback.
