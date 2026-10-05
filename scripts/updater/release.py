#!/usr/bin/env python3
"""Publish macOS builds to the shared BeFeast update feed and Forgejo releases.

  release.py --app tessera publish ARCHIVE --build N --short-version V
             --source SHA --tree SHA --signature EDSIG --channel beta
  release.py --app tessera promote --build N

The feed is the Cloudflare R2 bucket `befeast-updates` behind
https://updates.befeast.com, one folder per app: `<app>/appcast.xml` and
`<app>/<build>/<zip>`. Each build is also kept as Forgejo release
`macos-stable-<build>` in the calling repository for rollback. If that repository
has a `macos-stable` release (Tessera's feed up to build 5873), the appcast is
mirrored there too.

Environment: FORGEJO_TOKEN, GITHUB_REPOSITORY (owner/repo, set by Actions),
FORGEJO_URL (defaults to the LAN address the macOS runner already uses),
R2_ENDPOINT, R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY.
"""
import argparse
import datetime
import hashlib
import hmac
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import uuid

REPO = os.environ.get('GITHUB_REPOSITORY', 'BeFeast/tessera')
FEED_TAG = 'macos-stable'
BUCKET = 'befeast-updates'
PUBLIC = 'https://updates.befeast.com'
HERE = pathlib.Path(__file__).parent
sys.path.insert(0, str(HERE.parent / 'releases'))
import catalog


class Forgejo:
    def __init__(self):
        self.base = os.environ.get('FORGEJO_URL', 'https://git.oklabs.uk').rstrip('/')
        self.token = os.environ['FORGEJO_TOKEN']

    def call(self, method, path, body=None, content_type='application/json', raw=False):
        url = path if path.startswith('http') else f'{self.base}/api/v1/repos/{REPO}{path}'
        data = json.dumps(body).encode() if content_type == 'application/json' and body is not None else body
        request = urllib.request.Request(url, data=data, method=method, headers={
            'Authorization': f'token {self.token}', 'Content-Type': content_type})
        try:
            with urllib.request.urlopen(request, timeout=300) as response:
                payload = response.read()
        except urllib.error.HTTPError as error:
            sys.exit(f'{method} {url}: {error.code} {error.read()[:500]!r}')
        return payload if raw else (json.loads(payload) if payload else None)

    def release(self, tag):
        request = urllib.request.Request(f'{self.base}/api/v1/repos/{REPO}/releases/tags/{tag}',
                                         headers={'Authorization': f'token {self.token}'})
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return json.loads(response.read())
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return None
            raise

    def upload(self, release, name, data):
        boundary = uuid.uuid4().hex
        body = (f'--{boundary}\r\nContent-Disposition: form-data; name="attachment"; '
                f'filename="{name}"\r\nContent-Type: application/octet-stream\r\n\r\n').encode()
        body += data + f'\r\n--{boundary}--\r\n'.encode()
        return self.call('POST', f'/releases/{release["id"]}/assets?name={name}', body,
                         f'multipart/form-data; boundary={boundary}')

    def download(self, tag, name):
        return self.call('GET', f'{self.base}/{REPO}/releases/download/{tag}/{name}', raw=True)


class R2:
    """Minimal S3 (SigV4) client for one R2 bucket; stdlib only."""

    def __init__(self):
        self.endpoint = os.environ['R2_ENDPOINT'].rstrip('/')
        self.key = os.environ['R2_ACCESS_KEY_ID']
        self.secret = os.environ['R2_SECRET_ACCESS_KEY']

    def call(self, method, key, data=b'', content_type=None, cache=None):
        url = f'{self.endpoint}/{BUCKET}/{urllib.parse.quote(key)}'
        host = urllib.parse.urlparse(url).netloc
        now = datetime.datetime.now(datetime.timezone.utc)
        stamp, day = now.strftime('%Y%m%dT%H%M%SZ'), now.strftime('%Y%m%d')
        headers = {'host': host, 'x-amz-content-sha256': hashlib.sha256(data).hexdigest(),
                   'x-amz-date': stamp}
        if content_type:
            headers['content-type'] = content_type
        if cache:
            headers['cache-control'] = cache
        names = ';'.join(sorted(headers))
        canonical = '\n'.join([method, urllib.parse.urlparse(url).path, '',
                               *(f'{k}:{headers[k]}' for k in sorted(headers)), '', names,
                               headers['x-amz-content-sha256']])
        scope = f'{day}/auto/s3/aws4_request'
        to_sign = '\n'.join(['AWS4-HMAC-SHA256', stamp, scope,
                             hashlib.sha256(canonical.encode()).hexdigest()])
        signing = f'AWS4{self.secret}'.encode()
        for part in (day, 'auto', 's3', 'aws4_request'):
            signing = hmac.new(signing, part.encode(), hashlib.sha256).digest()
        signature = hmac.new(signing, to_sign.encode(), hashlib.sha256).hexdigest()
        headers['authorization'] = (f'AWS4-HMAC-SHA256 Credential={self.key}/{scope}, '
                                    f'SignedHeaders={names}, Signature={signature}')
        del headers['host']
        request = urllib.request.Request(url, data=data or None, method=method, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=300) as response:
                return response.read()
        except urllib.error.HTTPError as error:
            if method == 'GET' and error.code == 404:
                return None
            sys.exit(f'R2 {method} {key}: {error.code} {error.read()[:500]!r}')

    def put(self, key, data, content_type, cache=None):
        self.call('PUT', key, data, content_type, cache)


def update_appcast(app, r2, forgejo, appcast_args):
    current = r2.call('GET', f'{app}/appcast.xml')
    with tempfile.TemporaryDirectory() as tmp:
        old, new = pathlib.Path(tmp, 'old.xml'), pathlib.Path(tmp, 'appcast.xml')
        prefix = []
        if current is not None:
            old.write_bytes(current)
            prefix = ['--appcast', str(old)]
        subprocess.run([sys.executable, HERE / 'appcast.py', *prefix, '--output', new, *appcast_args],
                       check=True)
        data = new.read_bytes()
    # Clients re-check every hour; keep edge caches from serving a stale feed.
    r2.put(f'{app}/appcast.xml', data, 'application/xml', 'no-cache')
    # Mirror to the old feed, so builds that still point there move to this one.
    feed = forgejo.release(FEED_TAG)
    if feed is not None:
        # Forgejo cannot replace an asset in place; the old feed is briefly absent.
        for asset in feed['assets']:
            if asset['name'] == 'appcast.xml':
                forgejo.call('DELETE', f'/releases/{feed["id"]}/assets/{asset["id"]}')
        forgejo.upload(feed, 'appcast.xml', data)
    print(data.decode())


def publish(a):
    forgejo, r2 = Forgejo(), R2()
    tag = f'macos-stable-{a.build}'
    archive = pathlib.Path(a.archive)
    if forgejo.release(tag) is not None:
        sys.exit(f'Release {tag} already exists')
    release = forgejo.call('POST', '/releases', {
        'tag_name': tag, 'target_commitish': a.source, 'name': f'Tessera {a.short_version} ({a.build})',
        'body': f'Signed and notarized macOS arm64 build {a.build} from {a.source}.\n'
                f'Channel at publication: {a.channel}.',
        'draft': False, 'prerelease': a.channel != 'stable'})
    data = archive.read_bytes()
    forgejo.upload(release, archive.name, data)
    key = f'{a.app}/{a.build}/{archive.name}'
    r2.put(key, data, 'application/zip')
    update_appcast(a.app, r2, forgejo, ['add', '--build', str(a.build), '--short-version', a.short_version,
                             '--channel', a.channel, '--url', f'{PUBLIC}/{key}',
                             '--length', str(archive.stat().st_size), '--signature', a.signature,
                             '--source', a.source, '--tree', a.tree])
    if a.app == 'tessera':
        catalog.record(r2, 'macos', a.build, a.source,
                       [catalog.asset(key, 'Tessera-macos.zip', data)])
        r2.put('tessera/macos/beta/latest.zip', data, 'application/zip', 'no-cache')


def promote(a):
    forgejo = Forgejo()
    update_appcast(a.app, R2(), forgejo, ['promote', '--build', str(a.build)])
    release = forgejo.release(f'macos-stable-{a.build}')
    if release is not None:
        forgejo.call('PATCH', f'/releases/{release["id"]}', {'prerelease': False})


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--app', required=True, help='feed folder, e.g. tessera')
    sub = p.add_subparsers(dest='command', required=True)
    a = sub.add_parser('publish')
    a.add_argument('archive')
    for name in ('--short-version', '--source', '--tree', '--signature'):
        a.add_argument(name, required=True)
    a.add_argument('--build', type=int, required=True)
    a.add_argument('--channel', choices=('beta', 'stable'), required=True)
    a = sub.add_parser('promote')
    a.add_argument('--build', type=int, required=True)
    args = p.parse_args()
    {'publish': publish, 'promote': promote}[args.command](args)


if __name__ == '__main__':
    main()
