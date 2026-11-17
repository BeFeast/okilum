"""release-publish concurrency: the AppImage beta no longer queues behind other platforms (#3043).

The shared lock stays where publications touch cross-platform state (appcast,
catalog, #1040 promotion request, stable promotion). Publications of one platform
never overlap, and a retry attempt queues behind the run it retries.
"""
from pathlib import Path
import json
import re
import tempfile
import unittest
from unittest.mock import Mock, patch

import yaml

import publication
import test_appimage_publication as appimage_fixture

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / '.forgejo/workflows'
# The one expression form the workflow uses; a different form must update this test.
GROUP_FORM = re.compile(r"\$\{\{ inputs\.platform == '([a-z]+)' && '([a-z-]+)' \|\| '([a-z-]+)' \}\}")


def workflow(name):
    return yaml.safe_load((WORKFLOWS / name).read_text())


def evaluate(group, platform):
    """Forgejo's value of the release-publish group for a dispatch with this platform input."""
    match = GROUP_FORM.fullmatch(group)
    if match is None:
        if '${{' in group:
            raise AssertionError(f'Unrecognized group expression: {group}')
        return group
    own_platform, own_group, shared = match.groups()
    return own_group if platform == own_platform else shared


class PublicationGroups(unittest.TestCase):
    def setUp(self):
        self.concurrency = workflow('release-publish.yml')['concurrency']

    def test_workflow_group_matches_the_documented_mapping(self):
        for platform in (*publication.PUBLISH_PLATFORMS, 'unknown'):
            with self.subTest(platform=platform):
                self.assertEqual(evaluate(self.concurrency['group'], platform),
                                 publication.publication_group(platform))
        self.assertIs(self.concurrency['cancel-in-progress'], False)

    def test_only_appimage_leaves_the_shared_lock(self):
        groups = {p: publication.publication_group(p) for p in publication.PUBLISH_PLATFORMS}
        self.assertEqual(groups, {'macos': 'okilum-macos-release', 'windows': 'okilum-macos-release',
                                  'linux': 'okilum-macos-release', 'appimage': 'okilum-publish-appimage'})

    def test_group_depends_on_the_platform_alone(self):
        # Not run_id, attempt or github.*: two publications of one platform, and a
        # retry attempt (#1559) of a failed one, always meet in the same group.
        expression = self.concurrency['group']
        self.assertEqual(re.findall(r'(?:inputs|github|env|vars)\.[a-z_]+', expression), ['inputs.platform'])

    def test_stable_promotion_still_holds_the_shared_lock(self):
        stable = workflow('releases.yml')['concurrency']
        self.assertTrue(stable['group'].startswith(
            "${{ (inputs.build != '' || inputs.sync_stable) && '" + publication.SHARED_PUBLICATION_GROUP + "' ||"))
        self.assertIs(stable['cancel-in-progress'], False)


class Recording(appimage_fixture.Store):
    def __init__(self):
        super().__init__()
        self.reads = []

    def call(self, method, key):
        self.reads.append(key)
        return super().call(method, key)


class AppImageTouchesNoSharedState(unittest.TestCase):
    """Why the AppImage beta may leave the shared lock: every object it reads or writes is its own."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.directory = self.root / 'input'
        self.directory.mkdir()
        self.receipt, _, image, provenance = appimage_fixture.evidence()
        (self.directory / appimage_fixture.artifact.IMAGE).write_bytes(image)
        (self.directory / 'provenance.json').write_text(json.dumps(provenance))
        for guard in (patch('subprocess.run', return_value=Mock(returncode=0)),
                      patch.object(appimage_fixture.publisher.release_notes, 'merged_items', return_value=[])):
            guard.start()
            self.addCleanup(guard.stop)

    def publish(self, store, **kwargs):
        return appimage_fixture.publisher.publish(
            store, appimage_fixture.Key(), self.root, self.directory, appimage_fixture.SOURCE,
            self.receipt, 9999, client=Mock(), **kwargs)

    def test_beta_and_archive_only_stay_within_appimage_objects(self):
        own = ('okilum/linux/beta/', 'okilum/linux/builds/',
               f'okilum/releases/{appimage_fixture.SOURCE}/linux-appimage/')
        for archive_only in (False, True):
            store = Recording()
            # A newer AppImage beta forces the archive-only path on the second pass.
            if archive_only:
                store.objects['okilum/linux/beta/latest.json'] = json.dumps(
                    {'build': 6000, 'source': 'b' * 40}).encode()
            with self.subTest(archive_only=archive_only):
                self.publish(store)
                self.assertTrue(store.writes)
                outside = [k for k, _ in store.writes if not k.startswith(own)]
                self.assertEqual(outside, [])
                # Never the appcast, Arch/Windows feeds, the catalog or a promotion request.
                shared = [k for k in store.reads if not k.startswith(own)]
                self.assertEqual(shared, [])


# Dry run of the 17.11 night queue (#3043), from the Forgejo API: release-publish
# runs with their platform input, created time and observed run time, plus the
# releases.yml mirror jobs that share the `publish` label. Seconds after 03:00Z.
PUBLISH_SLOTS = 2  # slava-publish and slava-publish-extra advertise `publish`
NIGHT = [  # (run, platform or None for releases.yml, created, duration)
    (28947, 'windows', 14 * 60 + 58, 347),
    (28949, 'macos', 16 * 60 + 56, 254),
    (28955, None, 20 * 60 + 43, 106),
    (28956, 'windows', 20 * 60 + 53, 343),
    (28957, 'appimage', 23 * 60 + 53, 260),
    (28958, None, 24 * 60 + 57, 110),
    (28960, 'appimage', 29 * 60 + 32, 228),
]


def simulate(group, runs=NIGHT, slots=PUBLISH_SLOTS):
    """Forgejo 17: FIFO per group, cancel-in-progress false, never replaces a pending run."""
    pending = sorted(runs, key=lambda r: r[2])
    running, started, now = {}, {}, 0
    groups = {r[0]: (evaluate(group, r[1]) if r[1] else 'okilum-github-beta') for r in runs}
    while pending:
        busy = {groups[run] for run in running}
        for run, platform, created, duration in list(pending):
            ahead = [r for r in pending if groups[r[0]] == groups[run] and r[2] < created]
            if created <= now and len(running) < slots and groups[run] not in busy and not ahead:
                running[run] = now + duration
                started[run] = now
                busy.add(groups[run])
                pending.remove((run, platform, created, duration))
        upcoming = [end for end in running.values()] + [r[2] for r in pending if r[2] > now]
        now = min(t for t in upcoming if t > now) if any(t > now for t in upcoming) else now + 1
        running = {run: end for run, end in running.items() if end > now}
    return {run: (start, start + next(r[3] for r in runs if r[0] == run)) for run, start in started.items()}


class NightQueueDryRun(unittest.TestCase):
    def test_old_shared_group_reproduces_the_observed_queue(self):
        timeline = simulate('okilum-macos-release')
        # Observed: AppImage 28957 started 03:30:47 and 28960 at 03:35:09 (the
        # model omits the ~2 s Forgejo takes between one run and the next).
        self.assertAlmostEqual(timeline[28957][0], 30 * 60 + 47, delta=10)
        self.assertAlmostEqual(timeline[28960][0], 35 * 60 + 9, delta=10)

    def test_appimage_starts_on_dispatch_and_platforms_never_overlap_themselves(self):
        timeline = simulate(self.group())
        created = {run: at for run, _, at, _ in NIGHT}
        # 28957 found a free runner slot and starts at once (was 6 min 54 s in the
        # lock). 28960 waits only for a runner slot, never for another platform:
        # 31 s here, against 5 min 37 s in the lock.
        self.assertEqual(timeline[28957][0], created[28957])
        self.assertLessEqual(timeline[28960][0] - created[28960], 60)
        for run in (28957, 28960):
            self.assertLessEqual(timeline[run][1] - created[run], 5 * 60)
            self.assertGreater(simulate('okilum-macos-release')[run][1] - created[run], 9 * 60)
        by_group = {}
        for run, platform, *_ in NIGHT:
            if platform:
                by_group.setdefault(publication.publication_group(platform), []).append(timeline[run])
        for group, spans in by_group.items():
            spans.sort()
            with self.subTest(group=group):
                self.assertTrue(all(a[1] <= b[0] for a, b in zip(spans, spans[1:])))
        # The shared-lock order and timing of Windows/macOS are unchanged.
        old = simulate('okilum-macos-release')
        for run in (28947, 28949, 28956):
            self.assertEqual(timeline[run], old[run])

    def group(self):
        return workflow('release-publish.yml')['concurrency']['group']


if __name__ == '__main__':
    unittest.main()
