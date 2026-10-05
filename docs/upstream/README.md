# Upstreaming the vendor patches (#4)

> **Opened 2026-09-04** against `longbridge/gpui-kit` (the project renamed itself from
> `gpui-component`). Branches live on the fork `kossoy/gpui-kit`, each rebased onto
> upstream `main` @ `928c3eb7` (2026-09-05), which is also our vendor pin since the
> second weekly bump (#44). The fork branches are the source of truth: each
> `scripts/patches/000N-*.diff` is `git diff main..<branch>` from that fork.
>
> | Patch | Upstream PR | Status |
> |---|---|---|
> | 0001 | https://github.com/longbridge/gpui-kit/pull/2945 | landed upstream as `c3937a3` (2026-09-04), patch dropped at #44 |
> | 0002 | https://github.com/longbridge/gpui-kit/pull/2946 | landed upstream as `65db562` (2026-09-04), patch dropped at #44 |
> | 0003 | https://github.com/longbridge/gpui-kit/pull/2947 | landed upstream as `b0a1836` (2026-09-04), patch dropped at #44 |
> | 0004 | https://github.com/longbridge/gpui-kit/pull/2948 | open; reworked after review, fork branch `fix/autoscroll-notify-participant` @ `bee77ea` ([rework notes](https://github.com/longbridge/gpui-kit/pull/2948#issuecomment-5551008307)) |
> | 0006 | https://github.com/longbridge/gpui-kit/pull/2949 | open; reworked after review, fork branch `feat/inline-code-mono-font-family` @ `314efe8` ([rework notes](https://github.com/longbridge/gpui-kit/pull/2949#issuecomment-5551113807)) |
>
> When one merges: drop its file from `scripts/patches/` at the next vendor bump and
> note the upstream commit here. 0001–0003 went that way at the `928c3eb7` bump;
> their PR bodies (`0001.md`, `0002.md`, `0003.md`) were deleted with the diffs —
> the merged PRs above carry the text.

Every patch in `scripts/patches/` is a fix to `gpui-kit` that belongs upstream.
(0005, which pinned zed git revs, was our build hygiene; it was retired at the
`1f7f8c6` bump because gpui-kit now takes GPUI from crates.io as `gpui-pre-*`
and the pin lives in our `Cargo.lock` instead.)

Each file here is a ready-to-paste PR body with the reproduction that found the
defect. Opening the PRs is a deliberate, attributed act — they go out under a
real name to a real project — so it is not done by an unattended agent. The
sequence when you do:

```sh
# one branch per patch, from the pinned base
git -C vendor/gpui-component checkout -b fix/autoscroll-notify-participant 928c3eb776a3d733d9b771f7dea27a6a79242ced
git -C vendor/gpui-component apply ../../scripts/patches/0004-autoscroll-notify-participant.diff
git -C vendor/gpui-component commit -am "$(head -1 docs/upstream/0004.md | sed 's/^# //')"
# push to a fork of longbridge/gpui-kit, open the PR with the body from docs/upstream/0004.md
```

0004 and 0006 are independent of each other and cherry-pick together cleanly
onto `928c3eb7`. The line counts below are the reworked revisions, not the
first submissions.

| # | Patch | Lines | Body |
|---|---|---|---|
| 0004 | Drag-autoscroll never moved a virtualized list | +191 −69 | `0004.md` |
| 0006 | Inline code spans rendered in the body font, not mono | +723 −188 | `0006.md` |

Landed upstream and no longer carried here: 0001 hard breaks (`c3937a3`),
0002 stale list heights after same-count replace (`65db562`), 0003 soft-break
reflow (`b0a1836`).

Every one was measured on a real 3 900-note vault before and after; the numbers
in each body are from those runs, not estimates.

## Pending 0008 — rendered-text find

[0008](0008.md) adds search decorations and shared searchable Custom parts,
building on 0006. Its diff lives in `scripts/patches/0008-text-view-search-highlights.diff`;
gpui core remains unmodified.

## Pending 0009 — exact original Source copy

[0009](0009.md) binds optional original source to parsed replacements and preserves
its whitespace in select-all Copy. It follows 0008 in the cumulative patch stack.
