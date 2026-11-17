#!/usr/bin/env python3
"""Hand off completed build artifacts to a non-cancellable publication workflow."""
import argparse
import io
import json
import re
import xml.etree.ElementTree as ET
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import urllib.parse
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'updater'))
from release import Forgejo, R2
sys.path.insert(0, str(Path(__file__).resolve().parent))
import prepare
import release_notes
from coalesce import MACOS_BETA_UNTIL
from datetime import datetime

SCRIPTS = Path(__file__).resolve().parents[1]
WORKFLOWS = {'macos': 'macos-release.yml', 'linux': 'linux-release.yml',
             'windows': 'windows-diagnostic.yml'}
ARTIFACTS = {'macos': {'macos-publication': 'macos'},
             'linux': {'arch-publication': 'arch'},
             'windows': {'okilum-windows-velopack': 'windows', 'windows-portable': 'portable'}}


def preview_inputs(run):
    payload = run.get('event_payload') or {}
    if isinstance(payload, str):
        payload = json.loads(payload)
    return payload.get('inputs', {})


def validate_preview(client, platform, run):
    """The only exception to trusted-main publication: an exact Mac PR preview.

    Rechecked on the protected publisher, never accepted from artifact metadata.
    A notarization begun inside the approved window may finish after its expiry.
    """
    inputs = preview_inputs(run)
    number, source = inputs.get('preview_pr', ''), inputs.get('preview_head', '')
    if (platform != 'macos' or run['workflow_id'] != WORKFLOWS['macos']
            or run['trigger_event'] != 'workflow_dispatch' or run['is_fork_pull_request']
            or not re.fullmatch('[1-9][0-9]*', str(number))
            or not re.fullmatch('[0-9a-f]{40}', source)
            or source != run['commit_sha'] or run['prettyref'] == 'main'
            or inputs.get('beta_tick') or inputs.get('source')):
        raise ValueError('Publication requires the exact trusted macOS PR-preview path')
    created = datetime.fromisoformat(run['created'].replace('Z', '+00:00'))
    if created >= MACOS_BETA_UNTIL:
        raise ValueError('Mac PR-preview run was created after the approved window')
    pr = client.call('GET', '/pulls/' + str(number))
    if (pr['number'] != int(number) or pr['state'] != 'open' or pr['merged'] or pr['draft']
            or (pr['head']['repo'] or {}).get('full_name') != 'BeFeast/okilum'
            or pr['base']['repo']['full_name'] != 'BeFeast/okilum' or pr['base']['ref'] != 'main'
            or pr['head']['sha'] != source or pr['head']['ref'] != run['prettyref']):
        raise ValueError('Mac PR-preview source is closed, draft, untrusted or superseded')
    status = client.call('GET', '/commits/' + source + '/status')
    macos = [s for s in status['statuses'] if s['context'] == 'ci / macos (pull_request)']
    if status['sha'] != source or len(macos) != 1 or macos[0]['status'] != 'success':
        raise ValueError('Mac PR-preview source has no exact-head macOS success')
    return pr


def eligible(run, platform, head, preview=None):
    if (run['workflow_id'] != WORKFLOWS[platform]
            or (run['prettyref'] != 'main' and not (platform == 'macos' and preview))
            or run['is_fork_pull_request'] or run['trigger_event'] not in ['push', 'workflow_dispatch', 'schedule']):
        raise ValueError('Publication requires a trusted main release build')
    return run['status'] == 'success' and (snapshot_run(run, platform) or run['commit_sha'] == head)


def snapshot_run(run, platform):
    # Finish useful source-pinned main builds while newer main commits queue.
    # Published build numbers remain monotonic; advancing main must not starve beta.
    return run['trigger_event'] in ['push', 'schedule', 'workflow_dispatch']


def published_build(store, platform):
    keys = {'linux': 'okilum/arch/beta/x86_64/latest.json',
            'windows': 'okilum/windows/beta/releases.beta.json',
            'macos': 'okilum/appcast.xml'}
    current = store.call('GET', keys[platform])
    if current is None:
        return 0
    if platform == 'linux':
        return int(json.loads(current)['build'])
    if platform == 'windows':
        versions = [a['Version'] for a in json.loads(current)['Assets']]
        if not versions or any(not re.fullmatch(r'0\.1\.[0-9]+', v) for v in versions):
            raise ValueError('Unexpected Windows feed version')
        return max(int(v.split('.')[2]) for v in versions)
    channel = ET.fromstring(current).find('channel')
    if channel is None:
        raise ValueError('Invalid macOS appcast')
    # Sparkle accepts stable on every channel; match its existing all-item guard.
    version = '{http://www.andymatuschak.org/xml-namespaces/sparkle}version'
    return max((int(item.findtext(version)) for item in channel.findall('item')), default=0)


SOURCE_FILE = 'release-source.txt'
# The artifact folder each build writes SOURCE_FILE into (see the release workflows).
SOURCE_FOLDER = {'linux': 'arch', 'windows': 'windows'}


def built_source(root, platform, run):
    """The commit the artifact was built from (#1040).

    A run dispatched with an explicit source checks out that older main commit, so the
    run's own commit is not the source. The build writes the checked-out commit into its
    publication artifact; artifacts without the file come from ordinary builds.
    """
    if platform not in SOURCE_FOLDER:
        if platform == 'macos' and (run['trigger_event'] == 'schedule' or preview_inputs(run).get('hourly_source')):
            marker = root / 'macos' / 'release.json'
            if not marker.exists():
                return run['commit_sha']
            metadata = json.loads(marker.read_bytes())
            source = metadata['SOURCE_SHA']
            if source != run['commit_sha']:
                subprocess.run(['git', 'merge-base', '--is-ancestor', source, 'origin/main'],
                               check=True, capture_output=True, timeout=30)
            return source
        return run['commit_sha']
    marker = root / SOURCE_FOLDER[platform] / SOURCE_FILE
    if not marker.exists():
        # Only a manual run can carry an explicit source; without its record the built
        # commit is unknown, so it must not be published under the run's commit.
        if run['trigger_event'] == 'workflow_dispatch':
            raise ValueError('Manual build did not record its source commit')
        return run['commit_sha']
    source = marker.read_text().strip()
    if not re.fullmatch('[0-9a-f]{40}', source):
        raise ValueError('Invalid source recorded by the build')
    if source != run['commit_sha']:
        ancestor = subprocess.run(['git', 'merge-base', '--is-ancestor', source, 'origin/main'],
                                  capture_output=True, text=True, timeout=30)
        if ancestor.returncode != 0:
            raise ValueError(f'Recorded source {source} is not a commit on main')
    return source


def extract(data, directory):
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for entry in archive.infolist():
            path = Path(entry.filename)
            if path.is_absolute() or '..' in path.parts or (entry.external_attr >> 16) & 0o170000 == 0o120000:
                raise ValueError('Unsafe build artifact path')
        archive.extractall(directory)


WINDOWS_RECEIPT = 'windows-handoff'
WINDOWS_RECEIPT_FILE = 'windows-handoff.json'


def windows_receipt(artifacts, run_id):
    """The run's hosted-artifact receipt, or None for a run with Forgejo copies (signer).

    Exactly one form must be present: a receipt next to Forgejo copies is ambiguous.
    """
    current = [a for a in artifacts if not a['expired'] and a['run_id'] == run_id]
    receipts = [a for a in current if a['name'] == WINDOWS_RECEIPT]
    copies = [a for a in current if a['name'] in ARTIFACTS['windows']]
    if len(receipts) > 1 or (receipts and copies):
        raise ValueError('Ambiguous Windows publication artifacts')
    return receipts[0] if receipts else None


def one(root, pattern):
    files = list(root.glob(pattern))
    if len(files) != 1:
        raise ValueError(f'Expected one {pattern} artifact')
    return str(files[0])


def resumable_notes(store, client, build, source):
    """Mac beta notes; a retried publication of the same build keeps its first range (#1559).

    An attempt that already recorded notes for this build reuses them. One that
    stopped after the appcast write (this build is already the beta head) takes
    the newest older beta as its predecessor, never itself.
    """
    raw = store.call('GET', release_notes.key(source, 'macos', 'beta'))
    if raw is not None:
        existing = json.loads(raw)
        if (existing['build'], existing['source'], existing['platform'], existing['channel']) == (
                build, source, 'macos', 'beta'):
            return existing
    notes = release_notes.prepare(store, client, 'macos', build, source)
    if notes['previous_build'] != build:
        return notes
    previous = release_notes.predecessor(store, 'macos', below=build)
    return {**notes, 'previous_build': previous['build'] if previous else None,
            'previous_source': previous['source'] if previous else None,
            'items': release_notes.merged_items(client, previous['source'] if previous else None, source)}


def publish(client, platform, run_id):
    # Dispatch is the build's final step. Wait only for its final success record;
    # a newer push cancelling it must never authorize publication.
    for _ in range(120):
        run = client.call('GET', f'/actions/runs/{run_id}')
        head = client.call('GET', '/branches/main')['commit']['id']
        if (run['status'] in ['failure', 'cancelled', 'skipped']
                or (not snapshot_run(run, platform) and run['commit_sha'] != head)):
            print('Cancelled, failed or superseded build: nothing published')
            return
        is_preview = bool(preview_inputs(run).get('preview_pr'))
        if is_preview and run['status'] != 'success':
            # Publication is dispatched just before the source run finishes.
            # PR/head checks are needed at publication, not on every wait tick.
            time.sleep(5)
            continue
        preview = validate_preview(client, platform, run) if is_preview else None
        if eligible(run, platform, head, preview):
            break
        time.sleep(5)
    else:
        raise ValueError('Source build did not finish successfully')
    build = 5000 + run['index_in_repo']
    # Serialized with the other shared-lock publishers and stable promotion
    # (SHARED_PUBLICATION_GROUP; the AppImage beta has its own group). Complete
    # trusted snapshots remain useful after a merge, but never roll back a feed.
    # Only a manual run can carry an explicit older source (#1040), which goes to the
    # archive instead; it is decided after reading the artifact.
    if run['trigger_event'] != 'workflow_dispatch' and published_build(R2(), platform) > build:
        print(f'Newer {platform} build already published: nothing changed')
        return
    artifacts = client.call('GET', f'/actions/runs/{run_id}/artifacts')
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        if platform == 'windows' and windows_receipt(artifacts, run_id):
            # #2927: an unsigned main beta hands over a receipt for the hosted
            # artifact; its exact bytes are fetched and re-verified from GitHub.
            receipt = windows_receipt(artifacts, run_id)
            extract(client.call('GET', f'/actions/artifacts/{receipt["id"]}/zip', raw=True), root / 'receipt')
            import windows_handoff
            windows_handoff.fetch_published(json.loads((root / 'receipt' / WINDOWS_RECEIPT_FILE).read_text()),
                                            run_id, build, root)
        else:
            for name, folder in ARTIFACTS[platform].items():
                matching = [a for a in artifacts if a['name'] == name and not a['expired'] and a['run_id'] == run_id]
                if len(matching) != 1:
                    raise ValueError(f'Missing or ambiguous artifact: {name}')
                data = client.call('GET', f'/actions/artifacts/{matching[0]["id"]}/zip', raw=True)
                extract(data, root / folder)
        source = built_source(root, platform, run)
        # An explicit older commit is archived for promotion and never moves the beta feed.
        # A scheduled tick also differs from its run's commit: it builds main's tip, which
        # is newer than the tick's snapshot, so it publishes to beta as usual.
        archive_only = (source != run['commit_sha'] and run['trigger_event'] == 'workflow_dispatch'
                        and preview_inputs(run).get('hourly_source') != source)
        if not archive_only and published_build(R2(), platform) > build:
            print(f'Newer {platform} build already published: nothing changed')
            return
        # Recheck after downloads; no public mutation precedes this check.
        if (client.call('GET', '/branches/main')['commit']['id'] != run['commit_sha']
                and not snapshot_run(run, platform)):
            print('Superseded during artifact transfer: nothing published')
            return
        if preview:
            preview = validate_preview(client, platform, run)
        env = {**os.environ, 'GITHUB_SHA': source,
               'GITHUB_RUN_NUMBER': str(run['index_in_repo'])}
        hourly = bool(preview_inputs(run).get('hourly_source'))
        if hourly:
            if source != preview_inputs(run)['hourly_source']:
                raise ValueError('Hourly artifact source differs from the requested source')
            import hourly_beta
            # Beta has no certification gate (Oleg 05.11); the pin must still
            # be first-parent main and must not move the feed backwards.
            hourly_beta.main_source(source)
            previous = release_notes.predecessor(R2(), platform)
            if previous and previous['source'] != source:
                relation = subprocess.run(['git', 'merge-base', '--is-ancestor', previous['source'], source],
                                          capture_output=True, timeout=30)
                if relation.returncode != 0:
                    print('Newer beta source already published: nothing changed')
                    return
        # Capture the same-platform beta predecessor before ANY publisher writes.
        # Sidecars never alter descriptor, artifact, signing or promotion contracts.
        notes = (release_notes.prepare_preview(R2(), preview, build, source) if preview
                 else resumable_notes(R2(), client, build, source) if platform == 'macos'
                 else release_notes.prepare(R2(), client, platform, build, source))
        # Selection and publication are separated by compilation/notarization.
        # A racing hourly/manual producer may have published this source in that
        # interval. The existing publication lock makes this final check atomic
        # with feed writes. Daily Windows re-sign deliberately replaces the build
        # for the same source and must retain its existing path.
        if (not archive_only and not preview and not preview_inputs(run).get('resign_build')
                and notes['previous_source'] == source and notes['previous_build'] != build):
            print(f'{platform} beta source already published: no duplicate beta')
            return
        if platform == 'macos':
            notes_file = root / 'notes.json'
            notes_file.write_text(json.dumps(notes))
            env['OKILUM_RELEASE_NOTES'] = str(notes_file)
        extra = ['--archive-only'] if archive_only else []
        if platform == 'macos':
            command = [str(SCRIPTS / 'releases/macos-artifact.py'), 'publish', str(root / 'macos')]
        elif platform == 'linux':
            command = [str(SCRIPTS / 'arch/publish.py'), 'publish', '--build', str(build),
                       '--source', source, '--package', one(root / 'arch', '*.pkg.tar.zst'), *extra]
        else:
            command = [str(SCRIPTS / 'windows/publish.py'), 'publish', '--build', str(build),
                       '--source', source, '--directory', str(root / 'windows'),
                       '--portable', one(root / 'portable', '*.zip'), *extra]
        subprocess.run([sys.executable, *command], env=env, check=True)
        release_notes.record(R2(), notes)
        print(f'Published {platform} build {build} from completed run {run_id}, source {source}'
              + (' (archive only)' if archive_only else ''))
    # The last missing build of a requested promotion (#1040) starts it.
    if not preview:
        prepare.complete(R2(), client, source)


# A failed publication of a completed build is re-run from the same artifacts
# (reuse, no rebuild), a bounded number of times (#1559). Before this, one
# failed publisher step left the beta at the previous build until a human
# noticed or the next main push rebuilt. Only a failed publish job is retried:
# a successful one, or one that decided "nothing to publish", never repeats.
PUBLISH_PLATFORMS = (*WORKFLOWS, 'appimage')

# release-publish concurrency (#3043); the workflow's group expression mirrors
# this. Forgejo gives a run exactly one group, so a platform either shares the
# lock or has its own. The shared lock is also the one stable promotion holds
# (releases.yml). It covers state more than one platform touches: the macOS
# appcast (stable promotion rewrites it), the per-source catalog and the #1040
# promotion request (prepare.complete reads every platform's entry, then marks
# and promotes; R2 has no compare-and-swap), and the Windows/Arch beta objects
# stable promotion copies. The AppImage beta touches none of these: it writes
# only okilum/linux/{builds,beta} and its own release notes, and AppImage stable
# promotion reads only signed immutable builds/<n> objects. A group keyed on the
# platform alone still keeps two publications of one platform (a retry attempt
# included) from overlapping, which the per-feed monotonic checks rely on.
SHARED_PUBLICATION_GROUP = 'okilum-macos-release'
OWN_PUBLICATION_GROUP = {'appimage': 'okilum-publish-appimage'}


def publication_group(platform):
    return OWN_PUBLICATION_GROUP.get(platform, SHARED_PUBLICATION_GROUP)


MAX_PUBLICATION_ATTEMPTS = 3
RETRY_DELAY_SECONDS = 60
DISPATCH_ATTEMPTS = 3
FAILED_ISSUE_TITLE = 'Beta publication failed'


def dispatch_publication(client, platform, run_id, attempt=1, sleep=time.sleep):
    """Start release-publish for a completed build; a transient API error is retried.

    A lost dispatch is a missing publication, as silent as a failed one. A
    duplicate dispatch is harmless: the publisher refuses an already published
    source or an older build under its lock.
    """
    if platform not in PUBLISH_PLATFORMS or int(run_id) < 1 or int(attempt) < 1:
        raise ValueError('Invalid publication request')
    inputs = {'platform': platform, 'run_id': str(run_id)}
    if int(attempt) > 1:
        inputs['attempt'] = str(attempt)
    for tries in range(1, DISPATCH_ATTEMPTS + 1):
        try:
            client.call('POST', '/actions/workflows/release-publish.yml/dispatches',
                        {'ref': 'main', 'inputs': inputs})
            return
        except (OSError, SystemExit) as error:  # Forgejo.call exits on an HTTP error
            if tries == DISPATCH_ATTEMPTS:
                raise SystemExit(f'Could not dispatch {platform} publication of run {run_id} '
                                 f'after {tries} tries: {error}')
            print(f'Publication dispatch failed ({error}); retrying')
            sleep(10 * tries)


FEED_PLATFORM = {'macos': 'macos', 'windows': 'windows', 'linux': 'linux', 'appimage': 'linux-appimage'}


def feed_state(client, platform, run_id, feeds=None):
    """What the public beta feed serves now, said against this run's build."""
    try:
        if feeds is None:
            import hourly_beta
            feeds = hourly_beta.PublicFeeds()
        head = release_notes.predecessor(feeds, FEED_PLATFORM[platform])
        build = None
        if platform != 'appimage':  # AppImage numbers its builds in its own handoff
            build = 5000 + int(client.call('GET', f'/actions/runs/{run_id}')['index_in_repo'])
    except (Exception, SystemExit) as error:
        return f'The {platform} beta feed could not be read ({type(error).__name__}); its state is unknown.'
    if head is None:
        return f'The {platform} beta feed serves no build.'
    serves = f"build {head['build']} (source {head['source'][:8]})"
    if build is None:
        return f'The {platform} beta feed serves {serves}.'
    if head['build'] == build:
        return (f'The {platform} beta feed already serves this {serves}; '
                'a later publication step did not complete.')
    if head['build'] > build:
        return f'The {platform} beta feed serves newer {serves}; build {build} is superseded.'
    return f'The {platform} beta feed still serves previous {serves}; build {build} is not published.'


def report_exhausted(client, platform, run_id, attempts, run_url, state):
    """Leave a visible trace for the manager: one open issue, one comment per stuck build."""
    body = (f'The {platform} beta publication of completed build run {run_id} failed '
            f'{attempts} times. {state}\n\n'
            f'Last publication run: {run_url}\n\n'
            'Recovery without a rebuild: dispatch `release-publish` on main with '
            f'`platform={platform}` and `run_id={run_id}` once the cause is fixed '
            '(or the next main push builds and publishes a newer beta). '
            'Close this issue when the beta is current again.')
    issues = client.call('GET', '/issues?state=open&type=issues&limit=50&q='
                         + urllib.parse.quote(FAILED_ISSUE_TITLE)) or []
    existing = next((i for i in issues if i.get('title') == FAILED_ISSUE_TITLE), None)
    if existing:
        client.call('POST', f"/issues/{existing['number']}/comments", {'body': body})
    else:
        client.call('POST', '/issues', {'title': FAILED_ISSUE_TITLE, 'body': body})


def retry(client, platform, run_id, attempt, *, run_url='', summary=None, sleep=time.sleep, feeds=None):
    """Called only after a failed publish job of attempt `attempt`.

    Re-dispatches the same completed build until MAX_PUBLICATION_ATTEMPTS, then
    fails visibly: an error annotation, the job summary and a tracking issue,
    each stating what the public feed actually serves.
    """
    if platform not in PUBLISH_PLATFORMS or run_id < 1 or attempt < 1:
        raise ValueError('Invalid publication retry request')
    if attempt < MAX_PUBLICATION_ATTEMPTS:
        sleep(RETRY_DELAY_SECONDS * attempt)
        dispatch_publication(client, platform, run_id, attempt + 1, sleep=sleep)
        print(f'Publication attempt {attempt} of {platform} run {run_id} failed; '
              f'dispatched attempt {attempt + 1}/{MAX_PUBLICATION_ATTEMPTS} (reuse, no rebuild)')
        return
    state = feed_state(client, platform, run_id, feeds)
    message = (f'{platform} beta publication of run {run_id} failed {attempt} times; '
               f'no further automatic retry. {state}')
    if summary:
        with Path(summary).open('a') as handle:
            handle.write(message + '\n')
    try:
        report_exhausted(client, platform, run_id, attempt, run_url, state)
    except (Exception, SystemExit) as error:  # the red job below stays the primary signal
        print(f'Could not record the failure in an issue: {error}')
    print('::error::' + message)
    raise SystemExit(message)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['dispatch', 'publish', 'retry'])
    parser.add_argument('platform', choices=PUBLISH_PLATFORMS)
    parser.add_argument('--run', type=int)
    parser.add_argument('--attempt', type=int, default=1)
    args = parser.parse_args()
    client = Forgejo()
    if args.command == 'dispatch':
        dispatch_publication(client, args.platform, int(os.environ['GITHUB_RUN_ID']))
    elif args.run is None or args.run < 1:
        parser.error('--run must identify the source build')
    elif args.command == 'retry':
        retry(client, args.platform, args.run, args.attempt,
              run_url=os.environ.get('RUN_URL', ''), summary=os.environ.get('GITHUB_STEP_SUMMARY'))
    elif args.platform == 'appimage':
        parser.error('AppImage publication is appimage_handoff.py publish')
    else:
        publish(client, args.platform, args.run)
