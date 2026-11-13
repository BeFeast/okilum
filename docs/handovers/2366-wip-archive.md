# WIP archive of the stopped 02.11 fleet (#2366)

Disposition of the worktrees #2366 kept after the 02.11 fleet cleanup,
recorded 2026-11-13 against main `81a5c442b1b38afecda3a62841584cf1b0340ac7`.

## Where the work is

Every kept path that held work not on a fetched remote is preserved on Forgejo
as a ref under `refs/wip-2366/<worktree-name>`: the worktree HEAD plus any
uncommitted tracked and untracked changes (untracked files over 2 MB, all of
them QA source tarballs rebuildable from Git, are left out). Snapshots were
built with a temporary index, so no worktree, index or HEAD changed. 136 refs
in total; the six other kept worktrees had nothing beyond what main or a
remote branch already holds.

```bash
git fetch origin refs/wip-2366/<name>
git log -p FETCH_HEAD --not origin/main
```

None of these refs was ever merged as-is. A row is a starting point; compare it
with current main before reusing it.

## Dispositions

### Linked issue still open (19)

The issue keeps its user-visible scope as written; each open issue has a comment pointing to its ref(s).

| Ref `refs/wip-2366/…` | Branch | Commit | Issues | PRs |
|---|---|---|---|---|
| `t3-1dc0f8e2` | `feat/ok-11-cursor-reminder` OK-11 | `955a37a041ef` + uncommitted | #724 (open) | — |
| `t3-29d8f6f9` | `feat/ok16-offline-scene` OK-16 | `4883c1360b61` + uncommitted | #478 (open) | — |
| `t3-4cf1f7d3` | `t3/ok-48-board-help` OK-48 | `cb57c1ad6049` | #2357 (open) | #2361 closed |
| `t3-52d87ff7` | `t3/ok-49` OK-49 | `861a20997265` + uncommitted | #2356 (open) | #2362 closed |
| `t3-7433f9ae` | `impl/ok26-nightly` OK-26 | `2b8204ab09d3` + uncommitted | #1789 (open) | — |
| `t3-94acc3e3` | `t3/ok-37` OK-37 | `89d5bc47273e` + uncommitted | #2091 (open) | — |
| `t3-9b0c223f` | `t3/ok-39` OK-39 | `1114d46e0b59` + uncommitted | #2091 (open) | — |
| `t3-b63c4e95` | `ok-19-manager-role` OK-19 | `f806addeaf50` | #1473 (open) | — |
| `t3-b926b09c` | `ok22-gapless-reconnect` OK-22 | `a7b8c0d952fc` + uncommitted | #1390 (open) | — |
| `t3-c565c008` | `ok21-executor-tree` OK-21 | `cc33462e46e5` + uncommitted | #1419 (open) | — |
| `t3-cccf1e13` | `t3/implement-ok-50` OK-50 | `7887db220837` + uncommitted | #2207 (open) | — |
| `t3-d24889b1` | `t3/ok-41` OK-41 | `2e93185d3dad` + uncommitted | #1593 (open) | — |
| `t3-e711bc48` | `impl/ok-35-doc-contract` OK-35 | `5df4ba582513` + uncommitted | #1943 (open) | #2350 closed |
| `t3-e99bcfe3` | `implement/ok13-durable-untitled` OK-13 | `877454e1db42` + uncommitted | #1528 (open) | — |
| `t3-f3369741` | `ok44-search-bounded` OK-44 | `00c0b49d70ef` | #2094 (open) | #2360 closed |
| `integration-compat-1440` | `compat/1440-integration-log-checks` | `06d53e6f8b44` | #1440 (open) | — |
| `t3-9d509920` | `t3/ok-42` OK-42 | `7c9bfa30b76a` | #2094 (open) | — |
| `t3-d478fef4` | `t3/ok-34` OK-34 | `4aea9ccbbf1c` | #2344 (open) | — |
| `t3-eef767ad` | `t3/split-pane-behavior` | `ea4a2d78838c` | #773 (open) | — |

### Landed through a merged PR (11)

The ref keeps local follow-up commits or QA files only.

| Ref `refs/wip-2366/…` | Branch | Commit | Issues | PRs |
|---|---|---|---|---|
| `t3-0b418fd5` | `t3/ok-25` OK-25 | `cd8cb5489df5` + uncommitted | #1493 (closed) | #2349 merged |
| `t3-0c9b5ff2` | `ok06-table-grid` OK-06 | `6bf41f7313d5` + uncommitted | #1624 (closed) | #2345 merged |
| `t3-e5176b01` | `t3/ok-05-ledger-task` OK-05 | `c2f8bd877ecb` + uncommitted | #2347 (closed) | #2348 merged |
| `direct-reader-open-327` | `feat/327-direct-reader-open` | `1cf1b9d93442` + uncommitted | #327 (closed) | #332 merged |
| `goal-context-brief-160` | `feat/goal-context-brief-160` | `35497694e250` + uncommitted | — | #164 merged |
| `insert-note-link271` | `feat/insert-note-link271` | `4f111180e2b2` + uncommitted | #271 (closed) | #272 merged |
| `maestro-operation-dispositions-156` | `feat/maestro-operation-dispositions-156` | `98ef52a9e708` + uncommitted | #152 (closed) | #159 merged |
| `maintenance162-matrix` | `test/maintenance-capability-matrix` | `7e2f0e450fd0` + uncommitted | #162 (closed) | #177 merged |
| `retrieval204-selection` | `experiment/retrieval-selection-204` | `7a283d87a318` + uncommitted | #204 (closed) | #205 merged |
| `t3-5f3ed6b0` | `t3/prune-timeline-group-cache` | `ada21ec06833` | #2188 (closed) | #2261 merged |
| `t3-8ff0b0f5` | `t3/switch-vault-single-file-mode` | `d4279099d1c3` | #1527 (closed) | #2272 merged |

### Linked issue closed (26)

The issue was closed after this checkpoint (landed by other work or superseded); a closed PR's handover comment lists what it left out.

| Ref `refs/wip-2366/…` | Branch | Commit | Issues | PRs |
|---|---|---|---|---|
| `t3-0354e0d4` | `t3/ok-24-implementation` OK-24 | `4e1ee9e939b9` + uncommitted | #1622 (closed) | — |
| `t3-107cd14e` | `t3/ok-40` OK-40 | `711d390631a0` + uncommitted | #1643 (closed) | — |
| `t3-2b1860a6` | `t3/ok-45` OK-45 | `549c6c428226` + uncommitted | #1470 (closed) | — |
| `t3-31cbfd21` | `t3/ok-31-network-policy` OK-31 | `81ccdb323fa6` + uncommitted | #2047 (closed) | — |
| `t3-3c8da07c` | `t3/ok17` OK-17 | `ec69de5da4c6` + uncommitted | #1587 (closed) | — |
| `t3-832f53f2` | `impl/ok08-revision-layout` OK-08 | `9661b2a92899` + uncommitted | #1679 (closed) | #2341 closed |
| `t3-8464bd66` | `ok32/headless-session` OK-32 | `aa29967a5893` | #2048 (closed) | — |
| `t3-8b52e184` | `ok07-completion-ranking` OK-07 | `2326dceed5ae` + uncommitted | #1638 (closed) | #2343 closed |
| `t3-bc2cc944` | `t3/ok-18-implementation` OK-18 | `2056341a1aca` + uncommitted | #2161 (closed) | — |
| `t3-c6aa3cf5` | `ok09-piecewise-live` OK-09 | `01eff9d6769d` + uncommitted | #1621 (closed) | #2352 closed |
| `t3-cdbc9ad3` | `t3/ok-20` OK-20 | `f6263345489a` + uncommitted | #1478 (closed) | — |
| `t3-df1f5569` | `fix/ok-14-deleted-note-confirmation` OK-14 | `b1041bdb8012` + uncommitted | #1986 (closed) | #2342 closed |
| `t3-e95cec87` | `ok38-block-entry-20261102` OK-38 | `d0d512995e5c` + uncommitted | #1642 (closed) | #2354 closed |
| `t3-f18da739` | `t3/ok-46` OK-46 | `62cb28150fd7` + uncommitted | #1645 (closed) | — |
| `t3-ffeb4d26` | `ok30-ignore-policy` OK-30 | `27b654360ca3` + uncommitted | #2046 (closed) | #2351 closed |
| `context-review-export` | `feat/context-review-export` | `11b4c602201d` | #116 (closed) | — |
| `t3-299ba455` | `t3/express-session-ui` | `45c97a624c45` | #1493 (closed) | — |
| `t3-37cf9fff` | `t3/persistent-editable-table-grid` | `826cf9db7fe4` | #1624 (closed) | — |
| `t3-5c615813` | `t3/implement-ok-29` OK-29 | `1795fa098330` | #2045 (closed) | — |
| `t3-63c19118` | `t3/outline-navigation-incremental-update` | `bfabb2f36854` | #1320 (closed) | — |
| `t3-74e211a5` | `t3/chat-message-projection` | `ce1ed704d1e5` | #1784 (closed) | — |
| `t3-8ce175f5` | `t3/agent-rules-stack` | `16294ad6b618` | #1590 (closed) | — |
| `t3-e5b143f6` | `t3/rtl-punctuation-reveal-copy` | `20d5f1a9e2ad` | #1644 (closed) | — |
| `t3-eaade2b4` | `t3/agent-import-resolution` | `f2c35a918955` | #1588 (closed) | — |
| `t3-f3c10e26` | `t3/versioning-restore-retention` | `3ff24aba704c` | #2045 (closed) | #2291 closed |
| `t3-f784d7cb` | `t3/exact-target-draft-handoff` | `a7a156776283` | #1784 (closed) | — |

### No linked issue (80)

Checkpoints from earlier fleet branches with no issue or PR. Nothing user-visible is tracked for them; reopen work only through a new issue.

| Ref `refs/wip-2366/…` | Branch | Commit | Issues | PRs |
|---|---|---|---|---|
| `t3-0a8ff7f5` | `detached` | `8d5ad753730d` | — | — |
| `t3-23252a1e` | `t3/kibana-live-catchup` | `100dc04c4534` + uncommitted | — | — |
| `t3-45e4b853` | `t3/ok-36` OK-36 | `5a4b58b7b137` + uncommitted | — | — |
| `t3-982b3afe` | `detached` | `93ba96b96c4a` | — | — |
| `t3-9c2416b6` | `detached` | `8ae2c35b9af3` | — | — |
| `t3-b9b70b4b` | `detached` | `63740cacf190` | — | — |
| `t3-bb71b4e2` | `detached` | `4597a421e076` | — | — |
| `t3-d4c1aa23` | `t3/vault-device-review-ui` | `27693ec3abaf` | — | — |
| `t3-dd725158` | `t3/mobile-core-binding` | `4e083f6204b7` | — | — |
| `t3-dee141d7` | `t3/hang-list-quote-markers` | `674b6555722c` + uncommitted | — | — |
| `t3-f3e5b46d` | `detached` | `072e388ac22b` | — | — |
| `t3-f7d504e3` | `t3/optional-attachment-upload` | `bc44ca361e7c` + uncommitted | — | — |
| `1679-incremental-projection` | `feat/ssh-connect-1610` | `1139cddb0a11` | — | — |
| `auto-update-333` | `integration/333-338-normal-next` | `d95c92a3f371` + uncommitted | — | #345 closed |
| `maestro-observation-context267-historical193812c` | `detached` | `16019dea92d1` + uncommitted | — | — |
| `open-link285-diagnosis` | `diagnose/open-link285` | `4d696871b9f0` + uncommitted | — | — |
| `ui-1787-actions` | `feat/rtl-paragraph-context-1034` | `848d09f55ea5` + uncommitted | — | — |
| `t3-3c29e139` | `fix/2347-folder-external-changes` | `783d22c984b6` | — | — |
| `t3-0130ff69` | `t3/rules-refresh-offer-draft` | `15b126ff4a5b` | — | — |
| `t3-05a75152` | `t3/canonical-inbox-capture` | `61fe27bcbcd8` | — | — |
| `t3-060ca264` | `t3/preparation-cancellation-recovery` | `dc4c26b1e3b0` | — | — |
| `t3-0e0ccb03` | `t3/bound-supervisor-status` | `08181ca50ceb` | — | — |
| `t3-0f119f76` | `t3/project-skill-discovery` | `99a6b1592c02` | — | — |
| `t3-0ff13580` | `t3/feedback-video-upload-cancellation` | `83f39c01208d` | — | — |
| `t3-14cb5cf3` | `t3/bounded-piecewise-classification` | `a872270cf3fe` | — | — |
| `t3-15909920` | `t3/windows-feedback-transport` | `e0bd7130f5a2` | — | — |
| `t3-19ae7033` | `t3/recent-visual-gallery` | `c444dbc81c05` | — | — |
| `t3-1be902d8` | `t3/excalidraw-edit-roundtrip` | `2af406f4161f` | — | — |
| `t3-1ccd7a59` | `t3/existing-vault-admission` | `d3f481bbcf46` | — | — |
| `t3-206f05d0` | `t3/validate-pairing-invitations` | `66d3087b2317` | — | — |
| `t3-320344e2` | `t3/reject-ambiguous-process-records` | `7e0069a1360a` | — | — |
| `t3-3896a03b` | `t3/package-build-identity` | `900051eda0b3` | — | — |
| `t3-3a1aba2f` | `t3/unix-state-write-defenses` | `3806c8460324` | — | — |
| `t3-436e599a` | `t3/local-project-t3-binding` | `c643dd10579d` | — | — |
| `t3-43808946` | `t3/sync-peer-removal-revocation` | `937b1831bb2d` | — | — |
| `t3-4440503d` | `t3/isolate-stable-cargo-target` | `488d65e853ee` | — | — |
| `t3-452663b8` | `t3/approval-lifecycle-fixes` | `2de779cc9b7c` | — | — |
| `t3-45394cda` | `t3/first-sync-merge-preview` | `43c668688246` | — | — |
| `t3-49407393` | `t3/receiver-config-rediscovery` | `5cef7d8b78ee` | — | — |
| `t3-52d84602` | `t3/prevent-partial-runtime-staging` | `5c58451bc523` | — | — |
| `t3-58a18aec` | `t3/sync-headless-cli` | `83a528b842b7` | — | — |
| `t3-5a6fdbbf` | `t3/fix-readiness-handshake` | `a2bf528d1220` | — | — |
| `t3-5fafb863` | `t3/agent-language-diagnostics` | `b4c0cddc7b03` | — | — |
| `t3-672f10b0` | `t3/tasks-dashboard-snooze-actions` | `6a31526036b1` | — | — |
| `t3-6bdb6037` | `t3/link-write-fallback` | `6290c6939832` | — | — |
| `t3-71e8d8f9` | `t3/maestro-partial-source-recovery` | `66280f7bf710` | — | — |
| `t3-71fd0ec2` | `t3/move-preview-noop-filter` | `17b88a733adb` | — | — |
| `t3-733e1d9f` | `t3/sync-device-pairing-ui` | `951f1801dcb5` | — | — |
| `t3-7a257402` | `t3/remote-markdown-ssh-quick-view` | `390a1068f352` | — | — |
| `t3-820337bc` | `t3/untitled-draft-recovery` | `2b095eaa4153` | — | — |
| `t3-84dc5e13` | `t3/project-rotation-preview-handoff` | `1b03778be6ba` | — | — |
| `t3-892a89be` | `t3/history-writer-lock-release` | `cc1580c91d09` | — | — |
| `t3-8be98df0` | `t3/restore-source-listener` | `5f80d6d39571` | — | — |
| `t3-8e9dd0d9` | `t3/linux-daemon-env-path` | `5f80994ff65e` | — | — |
| `t3-9194e376` | `t3/updater-windows-proxy` | `464695add22f` | — | — |
| `t3-96e58840` | `t3/t3-question-answer-composition` | `88d58daa4305` | — | — |
| `t3-99bbd669` | `t3/vault-ignore-policy-reconciliation` | `6d56729ea53f` | — | — |
| `t3-9d63af89` | `t3/bounded-reader-quick-look` | `6c5cdd786d1b` | — | — |
| `t3-a0952320` | `t3/sparkle-defaults-domain` | `bc66a6d886c0` | — | — |
| `t3-af444c1e` | `t3/rtl-highlight-geometry` | `bc4748c38f86` | — | — |
| `t3-b7b94e57` | `t3/incremental-block-layout` | `90194ac92059` | — | — |
| `t3-b7cb95da` | `t3/heif-viewing-route` | `353388359ccf` | — | — |
| `t3-ba2e4e77` | `t3/preserve-search-results` | `175121ad963f` | — | — |
| `t3-bc018982` | `t3/bound-pairing-frames` | `972cb0d452e9` | — | — |
| `t3-c196d5cf` | `t3/agent-search-filter` | `c02c5b80d971` | — | — |
| `t3-c838df73` | `t3/sync-conflict-resolution` | `236c2a71b6d9` | — | — |
| `t3-ca4747f7` | `t3/pairing-deep-link-validation` | `b8ff8381712e` | — | — |
| `t3-cd895750` | `t3/macos-sidecar-bootstrap` | `bc3ca07fbd84` | — | — |
| `t3-d5b3cdb8` | `t3/idempotent-macos-registration` | `2c5e535bbf1e` | — | — |
| `t3-de8c559c` | `t3/complete-popup-candidates` | `3e99c543fe5a` | — | — |
| `t3-deecf500` | `t3/task-checkbox-accessibility-action` | `be8ffb8cacb1` | — | — |
| `t3-e3f593b7` | `t3/sync-lifecycle-cancellation` | `c32a61cff233` | — | — |
| `t3-e46ef605` | `t3/source-safe-conflict-preview-apply` | `6fdcb2a9ae72` | — | #2292 closed |
| `t3-eacbf403` | `t3/sync-network-policy` | `fc69b3d32506` | — | #2288 closed |
| `t3-f1dec86f` | `t3/recover-interrupted-helper-update` | `ddadc9bdab44` | — | — |
| `t3-f44debb1` | `t3/guard-owned-process-group` | `b638df836ad0` | — | — |
| `t3-f5407a1d` | `t3/bidi-caret-selection` | `9e85f714701a` | — | — |
| `t3-f67ad582` | `t3/guard-upload-revision` | `3ae5a5b252d2` | — | — |
| `t3-f6b4f7fe` | `t3/device-pairing-protocol` | `b54123060c3f` | — | — |
| `t3-f957f4e8` | `t3/inbox-answer-ack-recovery` | `26b86d16469b` | — | — |

## Inherited open PRs

- #2602 (format fix for #2593): closed, main already carries the same formatting.
- #2584 (#2582 Linux managed launch): closed, its head is an ancestor of main
  (landed with the sync-lossless-first merge, 08.11). Review blocker 1 is fixed
  by `48f96f6c6`; the missing-folder remainder is recorded on #2582, which stays open.
- #2004 and GitHub mirror PR 20, the two owner-risk holds named in #2366, were
  closed before this pass.
