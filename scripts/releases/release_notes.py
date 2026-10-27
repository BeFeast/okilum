"""Per-platform beta and stable notes; sidecar only, never part of an artifact descriptor."""
import html
import json
import re
import subprocess
import xml.etree.ElementTree as ET

PREFIX = 'okilum/releases'
MAINTENANCE = 'Maintenance and stability fixes'
HIDDEN = {'ci', 'test', 'chore', 'docs', 'refactor'}
CONVENTIONAL = re.compile(r'^([a-z]+)(?:\([^)]*\))?!?:\s*(.*)$', re.I)
SPARKLE = '{http://www.andymatuschak.org/xml-namespaces/sparkle}'
OKILUM = '{https://git.oklabs.uk/BeFeast/okilum/update-metadata}'


def key(source, platform, channel):
    if (not re.fullmatch('[0-9a-f]{40}', source) or platform not in ('macos', 'linux', 'linux-appimage', 'windows')
            or channel not in ('beta', 'stable')):
        raise ValueError('Notes require a full source SHA, known platform and channel')
    return f'{PREFIX}/{source}/{platform}/{channel}/notes.json'


def predecessor(store, platform, channel='beta'):
    """Read the actual published channel head BEFORE publication changes it."""
    if channel not in ('beta', 'stable'):
        raise ValueError('Unknown notes channel')
    if platform == 'macos':
        raw = store.call('GET', 'okilum/appcast.xml')
        if raw is None:
            return None
        items = ET.fromstring(raw).findall('./channel/item')
        beta = [i for i in items if i.findtext(SPARKLE + 'channel') == channel]
        if not beta:
            return None
        item = max(beta, key=lambda i: int(i.findtext(SPARKLE + 'version')))
        result = {'build': int(item.findtext(SPARKLE + 'version')),
                  'source': item.findtext(OKILUM + 'source')}
    elif platform == 'linux':
        raw = store.call('GET', f'okilum/arch/{channel}/x86_64/latest.json')
        if raw is None:
            return None
        result = json.loads(raw)
    elif platform == 'linux-appimage':
        raw = store.call('GET', f'okilum/linux/{channel}/latest.json')
        if raw is None:
            return None
        result = json.loads(raw)
        if result.get('platform') != 'linux-appimage' or result.get('channel') != channel:
            raise ValueError('Invalid previous AppImage channel identity')
    elif platform == 'windows':
        raw = store.call('GET', f'okilum/windows/{channel}/releases.{channel}.json')
        if raw is None:
            return None
        versions = [a['Version'] for a in json.loads(raw)['Assets']]
        if not versions or any(not re.fullmatch(r'0\.1\.[0-9]+', v) for v in versions):
            raise ValueError('Invalid previous Windows channel version')
        build = max(int(v.split('.')[2]) for v in versions)
        raw = store.call('GET', f'okilum/windows/builds/{build}/release.json')
        if raw is None:
            raise ValueError('Missing previous Windows channel source')
        result = json.loads(raw)
        if result['build'] != build:
            raise ValueError('Previous Windows build identity mismatch')
    else:
        raise ValueError('Unknown notes platform')
    if (type(result['build']) is not int or result['build'] <= 0
            or not re.fullmatch('[0-9a-f]{40}', result['source'])):
        raise ValueError('Invalid previous published build identity')
    return {'build': result['build'], 'source': result['source']}


def pr_item(pr, include_hidden=False):
    # Template instructions/examples inside comments are not authored summaries.
    body = re.sub(r'<!--.*?-->', '', pr.get('body') or '', flags=re.S)
    lines = re.findall(r'^User-visible:[ \t]*(.*?)[ \t]*$', body, flags=re.M | re.I)
    explicit = next((line.strip() for line in lines if line.strip()), '')
    title = ' '.join(pr['title'].split())
    match = CONVENTIONAL.match(title)
    prefix, text = (match[1].lower(), match[2]) if match else ('', title)
    labels = {str(label['name'] if isinstance(label, dict) else label).lower()
              for label in pr.get('labels', [])}
    labels |= {re.split(r'[:/]', label)[-1].strip() for label in list(labels)}
    if not include_hidden and not explicit and (prefix in HIDDEN or labels & HIDDEN):
        return None
    kind = 'feature' if prefix == 'feat' or (prefix != 'fix' and labels & {'feature', 'enhancement'}) else 'fix'
    return {'pr': pr['number'], 'kind': kind, 'text': explicit or text}


def merged_items(client, previous_source, source):
    """First-parent merge commits bound the range, not PR update/creation dates."""
    if not re.fullmatch('[0-9a-f]{40}', source) or (previous_source is not None
            and not re.fullmatch('[0-9a-f]{40}', previous_source)):
        raise ValueError('Invalid notes range')
    revision = f'{previous_source}..{source}' if previous_source else source
    result = subprocess.run(['git', 'rev-list', '--first-parent', revision],
                            check=True, capture_output=True, text=True, timeout=30)
    commits = result.stdout.splitlines()
    if not commits:
        return []
    merged = {}
    # Closed PRs are paginated; a recently updated old PR must never enter the range.
    for page in range(1, 101):
        prs = client.call('GET', f'/pulls?state=closed&sort=recentupdate&limit=50&page={page}')
        for pr in prs:
            sha = pr.get('merge_commit_sha')
            if pr.get('merged') and sha in commits:
                item = pr_item(pr)
                if item:
                    merged[pr['number']] = (commits.index(sha), item)
        if len(prs) < 50:
            break
    else:
        raise ValueError('Incomplete merged PR inventory for notes')
    return [item for _, item in sorted(merged.values(), key=lambda row: (row[0], row[1]['pr']))]


def prepare(store, client, platform, build, source, channel='beta'):
    key(source, platform, channel)
    previous = predecessor(store, platform, channel)
    return {'build': build, 'source': source, 'platform': platform, 'channel': channel,
            'previous_build': previous['build'] if previous else None,
            'previous_source': previous['source'] if previous else None,
            'items': merged_items(client, previous['source'] if previous else None, source)}


def prepare_appimage(store, client, build, source, channel='beta'):
    """Pin a range before moving the feed; retry never makes a self predecessor."""
    path = key(source, 'linux-appimage', channel)
    raw = store.call('GET', path)
    if raw is not None:
        existing = json.loads(raw)
        if (existing['build'], existing['source'], existing['platform'], existing['channel']) != (
                build, source, 'linux-appimage', channel):
            raise ValueError('Conflicting AppImage notes identity')
        return existing
    previous = predecessor(store, 'linux-appimage', channel)
    if previous and (previous['build'] >= build or previous['source'] == source):
        raise ValueError('AppImage notes require an older distinct published predecessor')
    return prepare(store, client, 'linux-appimage', build, source, channel)


def prepare_preview(store, pr, build, source):
    key(source, 'macos', 'beta')
    if (pr['head']['sha'] != source or type(pr['number']) is not int or pr['number'] < 1):
        raise ValueError('Preview notes must identify the exact PR head')
    previous = predecessor(store, 'macos', 'beta')
    # A preview always names its PR, including maintenance changes. Its kind
    # comes from the title prefix; labels cannot change this explicit contract.
    item = pr_item(dict(pr, labels=[]), include_hidden=True)
    return {'build': build, 'source': source, 'platform': 'macos', 'channel': 'beta',
            'previous_build': previous['build'] if previous else None,
            'previous_source': previous['source'] if previous else None,
            'items': [item], 'preview': {'pr': pr['number']}}


def prepare_stable(store, client, platform, build, source):
    # A failed promotion may have already moved a channel. Reuse the persisted
    # original range on retry, never recapture the now-promoted build as its own
    # predecessor. JSON remains the same public contract as beta notes.
    raw = store.call('GET', key(source, platform, 'stable'))
    if raw is not None:
        existing = json.loads(raw)
        if (existing['source'], existing['platform'], existing['channel']) != (source, platform, 'stable'):
            raise ValueError('Stable notes identity mismatch')
        if existing['build'] == build:
            return existing
        if existing['build'] > build:
            raise ValueError('Refusing stable notes rollback')
        # A higher build of the same source is a NEW promotion only after the
        # prior pinned build actually reached this channel. Otherwise it could
        # replace an interrupted attempt's range using a partially moved feed.
        previous = predecessor(store, platform, 'stable')
        if previous != {'build': existing['build'], 'source': source}:
            raise ValueError('Finish the pinned Stable notes build before selecting another build')
    return prepare(store, client, platform, build, source, 'stable')


def record(store, notes):
    store.put(key(notes['source'], notes['platform'], notes['channel']),
              json.dumps(notes, sort_keys=True).encode(), 'application/json', 'no-cache')


def description(notes):
    """Identical item text for Sparkle; HTML is escaped before XML serialization."""
    if not notes['items']:
        return '<p>' + MAINTENANCE + '</p>'
    lines = []
    for item in notes['items']:
        number = item['pr']
        if type(number) is not int or number <= 0:
            raise ValueError('Invalid release-note PR number')
        lines.append(f'<li><a href="https://git.oklabs.uk/BeFeast/okilum/pulls/{number}">#{number}</a> '
                     + html.escape(item['text']) + '</li>')
    return '<ul>' + ''.join(lines) + '</ul>'
