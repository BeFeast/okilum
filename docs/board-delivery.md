# Read-only Board delivery (#1768)

Delivery is an optional projection above the kanban, not the removed stage
title/goal/Refresh/Stage note header (#1795). Platforms and channels remain
separate. Opening a feed, source, artifact, Actions job or scenario is the only
action; this surface never publishes, installs, promotes, accepts or dispatches.

## Explicit opt-in and compatibility

The project owner authors `ops/board-delivery.toml` beside the existing Board
manifest. The app reads it and never creates or rewrites it. A separate sidecar
keeps version-1 Board manifests readable by older builds on other channels/OSes.
The existing related-note and revision-aware navigation path opens scenarios.
Missing sidecar preserves the existing Board. Invalid sidecar is a visible source
problem, not an empty healthy summary. No project name implies a feed URL.

```toml
version = 1
evidence = ["qa/board-scenario.md"]

[[feeds]]
platform = "macos"
channel = "beta"
format = "appcast"
url = "https://updates.example/project/beta/appcast.xml"
next = "Try the Board scenario on the exact published build"
# blocker = "Waiting for the explicitly named native check"

[[feeds]]
platform = "windows"
channel = "stable"
format = "velopack"
url = "https://updates.example/project/windows/stable/releases.stable.json"
metadata = "https://updates.example/project/windows/builds/{build}/release.json"

[[feeds]]
platform = "linux"
channel = "beta"
format = "appimage"
url = "https://updates.example/project/linux/beta/latest.json"
```

URLs above are intentionally examples, not an enrollment of this repository.
The project owner supplies its real public feed bindings. Each platform/channel
has at most one binding. An unbound track is explicitly unbound; it does not
inherit another OS's channel head. `next` and `blocker` are owner-authored facts,
not decisions inferred from a CI result. A candidate without published metadata
is not a published build.

Appcast reads the selected channel's highest `sparkle:version`, its explicit
`okilum:source`, and enclosure URL. Velopack joins the highest full package to
the bound immutable archive's exact version/build/source and matching feed asset.
AppImage reads its existing `linux-appimage` source/build/channel/key descriptor.
Conflicting identities fail closed. None downloads or installs a package.

For projects with an explicit Sprint publication receipt, `format = "receipt"`
reads exactly this JSON shape (the app does not manufacture such a receipt):

```json
{"platform":"linux","channel":"sprint","build":"run-123","source":"0123456789012345678901234567890123456789","artifact":"https://builds.example/exact.AppImage"}
```

An Actions link, successful launch log or run number alone is not a Sprint feed
or a downloadable artifact. If no such receipt exists, Sprint remains unbound.
An explicit HTTP HEAD success establishes artifact availability separately from
publication metadata. Redirects, missing files and stale checks remain unconfirmed.
The app does not claim checksum/signature verification from HEAD; installers keep
their existing verification path.

Alternatively `format = "actions"` binds one exact Forgejo **API run ID** in
`url` and one literal `artifact_name` selected for this platform by the project
owner. The reader fetches that run's `/artifacts` list, requires exactly one
matching unexpired non-empty artifact, and uses the run's explicit commit SHA.
The build label is `run-<index_in_repo>`; it is not a platform release version.
An optional artifact head SHA must agree with the run. This does not treat a
successful Mac job as Linux success or discover platform ownership from an
artifact name. The archive download must separately pass the availability read;
an authenticated/redirecting download remains unconfirmed without importing tokens.

## Inclusion, CI and exact human evidence

Each card's explicit `pull_requests` retains the issue→PR edge, including when
several issues share one PR. The reader fetches that PR and its source's public
Forgejo commit statuses. Status destinations expose the actual Actions jobs;
there is no GitHub API call. The PR head names its checks; the head and, after merge, explicit merge commit
are inclusion witnesses. This retains a pre-merge Sprint after the PR merges.
Inclusion requires equal source or a successful reverse Forgejo comparison
with zero commits (the PR source is an ancestor of the exact build). Missing or
inconsistent comparison metadata means unknown, never a guess from a version/time.

The owner records scenarios and human assertions in the named Markdown notes.
Only explicit YAML frontmatter is an interface; free prose, issue closure and
comments such as “works” are not parsed as acceptance. Other note frontmatter is
preserved because this reader performs no writes.

```yaml
---
delivery:
  - issue: 42
    pr: 57
    platform: linux
    channel: beta
    build: "123"
    source: "0123456789012345678901234567890123456789"
    artifact: https://updates.example/project/builds/123/Okilum.AppImage
    scenario: Open the Board, expand the issue and follow its scenario note
    installed_at: "2026-10-30T12:00:00Z"
    accepted_by: Oleg
    accepted_at: "2026-10-30T12:10:00Z"
---

The actual scenario, evidence and discussion stay here as ordinary Markdown.
```

Installation and acceptance fields are independently optional. Only an explicit
`accepted_by: Oleg` with time is Oleg acceptance. Every assertion is scoped to
that exact issue/PR/build/source/artifact/platform/channel/scenario. A newer build
does not inherit it. The UI opens the original note so the assertion is inspectable;
the app does not certify the author of a manually edited note.

Ready on this OS requires confirmed inclusion, fresh available artifact, a
scenario for that exact build, and no observed failed CI. A build for another OS
does not move this OS's card to Ready. Verified requires the exact human assertion;
installation/signing/CI alone never supplies it. A failed CI result remains visible
even alongside explicit acceptance. In opted-in projects legacy `stage = ready`
or `verified` remains canonical planning metadata but cannot bypass exact evidence.
Without opt-in, the earlier manual-stage projection remains compatible.

## Independent snapshots and lifetime

Binding, every feed, artifact, PR/CI and evidence note have their own cached value,
last successful read and error. Failed reads retain their previous values and
timestamps. Stale/conflicting facts remain visible and cannot grant readiness.
Removing a scenario does not erase the issue. Reads run on the Board worker and
only while visible, using its existing configurable timer. Owner/generation checks
reject late replies after hiding or switching the project. Public metadata is
bounded to 1 MiB per response and a bounded refresh budget; exceeding it is a
visible partial-source failure, not a successful empty result.

Regression coverage includes exact accepted-old/new-build separation, other-OS
isolation, multi-issue PR edges, independent stale/failing sources, conflicting
notes, failed CI, unavailable inclusion proof, real publisher formats and the
actual native Board summary/details navigation. These are automated assertions,
not human or native delivery acceptance.
