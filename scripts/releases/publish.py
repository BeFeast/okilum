#!/usr/bin/env python3
"""Publish a complete same-commit Beta, or explicitly promote it without rebuilding."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

import catalog

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS / 'updater'))
from release import R2, Forgejo, promote as promote_macos
import appcast


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


windows = module('windows_publish', SCRIPTS / 'windows/publish.py')
arch = module('arch_publish', SCRIPTS / 'arch/publish.py')


class GitHub:
    def call(self, method, path, body=None, binary=False):
        base = 'https://uploads.github.com' if binary else 'https://api.github.com'
        data = body if binary else (catalog.encode(body) if body is not None else None)
        req = urllib.request.Request(base + '/repos/BeFeast/tessera' + path, data=data, method=method,
            headers={'Authorization': 'Bearer ' + os.environ['MIRROR_TOKEN'],
                     'Accept': 'application/vnd.github+json',
                     'Content-Type': 'application/octet-stream' if binary else 'application/json',
                     'X-GitHub-Api-Version': '2022-11-28'})
        try:
            with urllib.request.urlopen(req, timeout=300) as response:
                raw = response.read()
                return json.loads(raw) if raw else None
        except urllib.error.HTTPError as error:
            if method == 'GET' and error.code == 404:
                return None
            raise


def choose(store, build=None):
    raw = store.call('GET', 'tessera/appcast.xml')
    if raw is None:
        return None
    items = ET.fromstring(raw).findall('./channel/item')
    for item in sorted(items, key=lambda i: int(i.findtext(appcast.s('version'))), reverse=True):
        number = int(item.findtext(appcast.s('version')))
        if build is not None and number != build:
            continue
        value = catalog.bundle(store, item.findtext(appcast.t('source')), number)
        if value:
            return value
        if build is not None:
            raise ValueError('Selected build lacks completed artifacts for all three platforms')
    return None


def checked_git(*args):
    return subprocess.check_output(['git', *args], text=True).strip()


def notes(forgejo, release, previous):
    # First-parent ancestry bounds the release even when branch commits predate it.
    source = release['source']
    revision = f'{previous["source"]}..{source}' if previous else source
    commits = set(checked_git('rev-list', '--first-parent', revision).splitlines())
    lines = []
    page = 1
    while True:
        prs = forgejo.call('GET', f'/pulls?state=closed&sort=recentupdate&limit=50&page={page}')
        for pr in prs:
            if pr.get('merged') and pr.get('merge_commit_sha') in commits:
                title = ' '.join(pr['title'].split())
                lines.append(f'- {title} ([#{pr["number"]}]({pr["html_url"]}))')
        if len(prs) < 50:
            break
        page += 1
    builds = ', '.join(f'{p}: {v["build"]}' for p, v in release['platforms'].items())
    return '\n'.join([f'Same-source builds — {builds}.', '', *lines])


def mirror_tag(release, stable):
    tag = f'v0.1.{release["build"]}' if stable else 'beta'
    checked_git('fetch', '--no-tags', 'origin', 'main')
    checked_git('merge-base', '--is-ancestor', release['source'], 'origin/main')
    # A credential helper sends credentials through stdin, never a logged URL.
    helper = '!f() { echo username=oleg; echo "password=$FORGEJO_TOKEN"; }; f'
    remote = checked_git('ls-remote', 'origin', f'refs/tags/{tag}')
    if stable and remote and remote.split()[0] != release['source']:
        raise ValueError('Stable release tag already names another commit')
    checked_git('tag', '-f', tag, release['source'])
    refspec = ('+' if not stable else '') + f'refs/tags/{tag}:refs/tags/{tag}'
    checked_git('-c', 'credential.helper=', '-c', f'credential.helper={helper}', 'push', 'origin', refspec)
    subprocess.run(['bash', str(SCRIPTS / 'releases/mirror.sh')], check=True,
                   env={**os.environ, 'RELEASE_TAG': tag})
    return tag


def github_release(github, tag, release, files, body, stable):
    # List includes authenticated drafts, including an interrupted replacement.
    current = None
    page = 1
    while True:
        candidates = github.call('GET', f'/releases?per_page=100&page={page}')
        matching = [r for r in candidates if r['tag_name'] == tag]
        if matching:
            if current is not None or len(matching) != 1:
                raise ValueError('Duplicate releases for tag; reconcile drafts before retrying')
            current = matching[0]
        if len(candidates) < 100:
            break
        page += 1
    if current is None:
        current = github.call('POST', '/releases', {'tag_name': tag, 'draft': True,
                              'name': 'Beta' if not stable else f'Tessera 0.1.{release["build"]}',
                              'prerelease': not stable})
    rid = current['id']
    # Hide an existing rolling release while replacing its matching asset set.
    github.call('PATCH', f'/releases/{rid}', {'draft': True})
    for old in current.get('assets', []):
        github.call('DELETE', f'/releases/{rid}/assets/{old["id"]}')
    for name, data in files.items():
        github.call('POST', f'/releases/{rid}/assets?name={urllib.parse.quote(name)}', data, binary=True)
    github.call('PATCH', f'/releases/{rid}', {
        'name': f'Tessera 0.1.{release["build"]}' if stable else 'Beta',
        'body': body, 'draft': False, 'prerelease': not stable})
    if stable:
        github.call('PATCH', f'/releases/{rid}', {'make_latest': 'true'})


def preflight_stable(store, release):
    """Check all channel rollback guards and archived metadata before mutating any."""
    win_build = release['platforms']['windows']['build']
    meta = json.loads(store.call('GET', f'tessera/windows/builds/{win_build}/release.json'))
    if meta['source'] != release['source'] or meta['build'] != win_build:
        raise ValueError('Windows archived source mismatch')
    class ValidateOnly:
        def call(self, method, key):
            return store.call(method, key)

        def put(self, *_):
            pass

    # Run the real Windows preparation/validation, discarding its planned writes.
    windows.promote(win_build, ValidateOnly())
    linux = release['platforms']['linux']['build']
    meta = json.loads(store.call('GET', f'tessera/arch/builds/{linux}/manifest.json'))
    if meta['source'] != release['source'] or meta['build'] != linux:
        raise ValueError('Arch archived source mismatch')
    package = release['platforms']['linux']['assets'][0]
    if meta['sha256'] != package['sha256'] or meta['filename'] != package['name']:
        raise ValueError('Arch manifest differs from release catalog')
    old = store.call('GET', 'tessera/arch/stable/x86_64/latest.json')
    if old and json.loads(old)['build'] > linux:
        raise ValueError('Arch stable would roll back')
    found = False
    for item in ET.fromstring(store.call('GET', 'tessera/appcast.xml')).findall('./channel/item'):
        if int(item.findtext(appcast.s('version'))) == release['build']:
            mac = release['platforms']['macos']['assets'][0]
            enclosure = item.find('enclosure')
            if (item.findtext(appcast.t('source')) != release['source'] or enclosure is None
                    or enclosure.get('url') != 'https://updates.befeast.com/' + mac['key']
                    or enclosure.get('length') != str(mac['size'])
                    or not enclosure.get(appcast.s('edSignature'))):
                raise ValueError('macOS appcast differs from release catalog')
            found = True
        if item.findtext(appcast.s('channel')) == 'stable' and int(item.findtext(appcast.s('version'))) > release['build']:
            raise ValueError('macOS stable would roll back')
    if not found:
        raise ValueError('Selected macOS build is absent from appcast')


def execute(store, github, forgejo, build=None, supersede_pending=False):
    stable = build is not None
    channel = 'stable' if stable else 'beta'
    key = f'{catalog.PREFIX}/{channel}.json'
    old = store.call('GET', key)
    previous = json.loads(old) if old else None
    # Snapshot an in-progress stable transaction so retries cannot select new
    # platform reruns for the same source. Feeds across OSes are not atomic.
    pending_key = f'{catalog.PREFIX}/promoting.json'
    pending = store.call('GET', pending_key) if stable else None
    if pending:
        candidate = json.loads(pending)
        if candidate['build'] != build and (previous is None or previous['build'] != candidate['build']):
            if not supersede_pending or build <= candidate['build']:
                raise ValueError('Finish the interrupted promotion or explicitly supersede it with a newer build')
        release = candidate if candidate['build'] == build else choose(store, build)
    else:
        release = choose(store, build)
    if release is None:
        if stable:
            raise ValueError('Unknown selected build')
        print('No completed three-platform beta yet')
        return
    if previous and previous['build'] > release['build']:
        raise ValueError('Refusing release rollback')
    if previous == release:
        print(f'{channel} already published')
        return
    files = catalog.download(store, release)
    prior_stable = store.call('GET', f'{catalog.PREFIX}/stable.json')
    body = notes(forgejo, release, json.loads(prior_stable) if prior_stable else None)
    if not stable:
        body = 'Experimental Beta. Stable promotion awaits cross-platform QA.\n\n' + body
    if stable:
        preflight_stable(store, release)
        # Validate Arch signature and metadata before touching Windows/macOS.
        with arch.SigningKey() as signing, tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            name = arch.package_name(release['platforms']['linux']['build'])
            path = directory / name
            path.write_bytes(files[name])
            Path(str(path) + '.sig').write_bytes(files[name + '.sig'])
            signing.verify(path)
            arch.validate_package(path, release['platforms']['linux']['build'])
            store.put(pending_key, catalog.encode(release), 'application/json', 'no-cache')
            # Mirror/tag first: an unavailable GitHub cannot leave promoted feeds.
            tag = mirror_tag(release, True)
            arch.execute(SimpleNamespace(command='promote', build=release['platforms']['linux']['build']),
                         store, signing, directory)
            windows.promote(release['platforms']['windows']['build'], store)
            promote_macos(SimpleNamespace(app='tessera', build=release['build']))
        store.put('tessera/macos/latest.zip', files['Tessera-macos.zip'], 'application/zip', 'no-cache')
    else:
        tag = mirror_tag(release, False)
    github_release(github, tag, release, files, body, stable)
    store.put(key, catalog.encode(release), 'application/json', 'no-cache')
    print(f'Published {channel}: {release["build"]}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build', type=int, help='Explicit stable promotion: accepted macOS build')
    parser.add_argument('--supersede-pending', action='store_true',
                        help='Owner-approved recovery: replace an interrupted promotion with a newer build')
    args = parser.parse_args()
    if args.supersede_pending and args.build is None:
        parser.error('--supersede-pending requires --build')
    if args.build is not None and args.build < 1:
        parser.error('Build must be positive')
    execute(R2(), GitHub(), Forgejo(), args.build, args.supersede_pending)
