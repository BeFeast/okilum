#!/usr/bin/env python3
"""Read-only Forgejo projection. Private config, bounded GETs, atomic derived cache."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import stat
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

MAX_BYTES = 32 * 1024 * 1024


class Unavailable(Exception):
    pass


def require(value, code='invalid_source'):
    if not value:
        raise Unavailable(code)


def text(value, limit=4096):
    require(isinstance(value, str) and len(value) <= limit)
    return value


def identity(value):
    require(type(value) is int and value > 0)
    return value


def private_json(path):
    meta = Path(path).lstat()
    require(stat.S_ISREG(meta.st_mode) and not meta.st_mode & 0o077 and meta.st_size <= 16384, 'private_config_required')
    return json.loads(Path(path).read_text())


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class Client:
    def __init__(self, base, token):
        self.base = base
        self.remaining = 64 * 1024 * 1024
        self.deadline = time.monotonic() + 240
        self.token = token
        self.http = urllib.request.build_opener(NoRedirect(), urllib.request.ProxyHandler({}))

    def get(self, path):
        require(path.startswith('/') and not path.startswith('//'), 'invalid_route')
        require(time.monotonic() < self.deadline and self.remaining > 0, 'source_budget')
        request = urllib.request.Request(self.base + '/api/v1' + path,
                    headers={'Authorization': 'token ' + self.token, 'Accept': 'application/json'})
        try:
            with self.http.open(request, timeout=15) as response:
                raw = response.read(min(MAX_BYTES, self.remaining) + 1)
                self.remaining -= len(raw)
                require(self.remaining >= 0, 'source_budget')
                require(len(raw) <= MAX_BYTES, 'source_limit')
                return json.loads(raw)
        except (urllib.error.URLError, ValueError, TimeoutError, OSError):
            raise Unavailable('source_unavailable') from None

    def pages(self, path):
        # Request one extra empty page: do not assume the server honors limit=50.
        result, seen = [], set()
        for page in range(1, 101):
            rows = self.get(path + ('&' if '?' in path else '?') + f'limit=50&page={page}')
            require(isinstance(rows, list), 'invalid_page')
            if not rows:
                return result
            for row in rows:
                require(isinstance(row, dict), 'invalid_page')
                key = identity(row.get('id'))
                require(key not in seen, 'duplicate_page')
                seen.add(key)
            result.extend(rows)
        raise Unavailable('page_limit')


def origin_link(base, value):
    value = text(value)
    url, expected = urllib.parse.urlsplit(value), urllib.parse.urlsplit(base)
    require(url.scheme == expected.scheme and url.netloc == expected.netloc and not url.username and not url.password, 'foreign_link')
    return value


def repo_summary(base, row):
    full = text(row['full_name'], 512)
    require(len(full.split('/')) == 2 and all(full.split('/')), 'invalid_repo')
    units = {unit: row.get('has_' + unit, True) for unit in ('issues','pull_requests','releases')}
    require(all(type(flag) is bool for flag in units.values()), 'invalid_repo_units')
    return {'id': identity(row['id']), 'name': full, 'url': origin_link(base, row['html_url']), 'units': units}


def discover(client):
    # Forgejo /user/repos can omit public organization repositories. Enumerate
    # memberships explicitly as well; a partial org traversal is not discovery.
    rows = client.pages('/user/repos')
    for org in client.pages('/user/orgs'):
        name = text(org.get('name') or org.get('username'), 256)
        require(name and '/' not in name, 'invalid_org')
        rows.extend(client.pages('/orgs/' + urllib.parse.quote(name, safe='') + '/repos'))
    found = {}
    for row in rows:
        repo = repo_summary(client.base, row)
        found[repo['id']] = repo
    return list(found.values())


def item_summary(base, row):
    return {'id': identity(row['id']), 'number': identity(row['number']),
            'title': text(row['title']), 'url': origin_link(base, row['html_url']),
            'state': text(row['state'], 32), 'updated_at': text(row['updated_at'], 64),
            'assignees': [text(a['login'], 256) for a in (row.get('assignees') or [])]}


def sha(value):
    require(isinstance(value, str) and len(value) in (40, 64) and all(c in '0123456789abcdef' for c in value), 'invalid_commit')
    return value


def read_repo(client, summary):
    base = client.base
    route = '/repos/' + '/'.join(urllib.parse.quote(p, safe='') for p in summary['name'].split('/'))
    units = summary['units']
    issues = [item_summary(base, r) for r in client.pages(route + '/issues?state=open&type=issues') if not r.get('pull_request')] if units['issues'] else []
    pulls = []
    for row in (client.pages(route + '/pulls?state=open') if units['pull_requests'] else []):
        item = item_summary(base, row)
        item['head_commit'] = sha(row['head']['sha'])
        # Commit status API returns a list with ordinary pagination, unlike combined status.
        item['checks'] = [{'id': identity(s['id']), 'context': text(s['context'], 512),
                          'state': text(s['status'], 32), 'updated_at': text(s['updated_at'], 64)}
                         for s in client.pages(route + '/commits/' + item['head_commit'] + '/statuses')]
        pulls.append(item)
    releases = []
    for row in (client.pages(route + '/releases') if units['releases'] else []):
        if row.get('draft'):
            continue
        releases.append({'id': identity(row['id']), 'tag': text(row['tag_name'], 512),
                         'name': text(row['name']), 'url': origin_link(base, row['html_url']),
                         'prerelease': bool(row['prerelease']), 'published_at': text(row['published_at'], 64),
                         'target_commitish': text(row['target_commitish'], 256),
                         'assets': [{'name': text(a['name'], 512), 'url': origin_link(base, a['browser_download_url'])}
                                    for a in row['assets']]})
    return dict(summary, issues=issues, pulls=pulls, releases=releases)


def empty(config):
    return {'version': 1, 'owner_id': config['owner_id'], 'instance': config['base_url'],
            'account_id': config['account_id'], 'account_login': None,
            'checked_at': None, 'discovered_at': None, 'error': None, 'repos': [],
            'projects': config['projects']}


def collect(client, config, old, now):
    client.remaining = 64 * 1024 * 1024
    client.deadline = time.monotonic() + 240
    view = json.loads(json.dumps(old or empty(config)))
    require(all(view[k] == empty(config)[k] for k in ('version','owner_id','instance','account_id')), 'cache_identity_changed')
    view.update(checked_at=now, projects=config['projects'])
    try:
        account = client.get('/user')
        require(identity(account['id']) == config['account_id'], 'account_changed')
        login = text(account['login'], 256)
        rows = discover(client)
        require(len({r['id'] for r in rows}) == len(rows), 'duplicate_repo')
    except (Unavailable, KeyError, TypeError, ValueError) as error:
        view['error'] = str(error) if isinstance(error, Unavailable) else 'invalid_source'
        return view
    known = {r['id']: r for r in view['repos']}
    repos = []
    for summary in rows:
        previous = known.pop(summary['id'], dict(summary, issues=[], pulls=[], releases=[], synced_at=None))
        try:
            current = read_repo(client, summary)
            current.update(synced_at=now, checked_at=now, error=None, listed=True)
        except (Unavailable, KeyError, TypeError, ValueError) as error:
            current = dict(previous, **summary, checked_at=now, listed=True,
                           error=str(error) if isinstance(error, Unavailable) else 'invalid_source')
        repos.append(current)
    # Disappearance is not deletion; retain last observations, explicitly unavailable.
    for previous in known.values():
        repos.append(dict(previous, listed=False, checked_at=now, error='no_longer_accessible'))
    view.update(account_login=login, repos=repos, discovered_at=now, error=None)
    return view


def write_cache(path, value):
    raw = json.dumps(value, ensure_ascii=False).encode()
    require(len(raw) <= MAX_BYTES, 'cache_limit')
    path = Path(path)
    fd, name = tempfile.mkstemp(prefix='.forgejo-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as out:
            out.write(raw)
            out.flush()
            os.fsync(out.fileno())
        os.replace(name, path)
        fd = os.open(path.parent, os.O_DIRECTORY)
        try: os.fsync(fd)
        finally: os.close(fd)
    finally:
        if os.path.exists(name): os.unlink(name)


def configuration(path):
    c = private_json(path)
    require(set(c) == {'owner_id','base_url','account_id','credential_file','cache_file','projects'}, 'invalid_config')
    require(str(uuid.UUID(c['owner_id'])) == c['owner_id'], 'invalid_owner')
    identity(c['account_id'])
    u = urllib.parse.urlsplit(c['base_url'])
    require(u.scheme == 'https' and u.hostname and not u.username and not u.password and
            not u.query and not u.fragment and not u.path, 'invalid_origin')
    require(isinstance(c['projects'], dict) and len(c['projects']) <= 100, 'invalid_projects')
    for project, refs in c['projects'].items():
        require(str(uuid.UUID(project)) == project and isinstance(refs, list) and len(refs) <= 100, 'invalid_project')
        for ref in refs:
            require(set(ref) == {'repo_id','launch_target_ids'}, 'invalid_repo_link')
            identity(ref['repo_id'])
            require(isinstance(ref['launch_target_ids'], list), 'invalid_target_links')
            for target in ref['launch_target_ids']: text(target, 512)
    return c


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--once', action='store_true')
    args = parser.parse_args()
    os.umask(0o077)
    config = configuration(args.config)
    credential = private_json(config['credential_file'])
    require(isinstance(credential.get('token'), str) and len(credential['token']) >= 20, 'invalid_credential')
    path = Path(config['cache_file'])
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    lock = os.open(path.with_suffix('.lock'), os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    client = Client(config['base_url'], credential['token'])
    old = json.loads(path.read_bytes()) if path.exists() else None
    while True:
        result = collect(client, config, old, int(time.time()))
        write_cache(path, result)
        old = result
        print(json.dumps({'event':'forgejo_sync','status':'unavailable' if result['error'] or any(r['error'] for r in result['repos']) else 'ok'}), flush=True)
        if args.once: break
        time.sleep(300)


if __name__ == '__main__':
    try:
        main()
    except Exception:
        print('{"event":"forgejo_stopped","status":"configuration_or_storage_error"}', flush=True)
        raise SystemExit(1) from None
