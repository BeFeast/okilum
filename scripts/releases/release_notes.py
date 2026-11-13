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
# Only the trailer exactly as `git revert` writes it, in a commit whose subject
# git wrote too; a quoted or paraphrased trailer is not a revert.
REVERT_TRAILER = re.compile(r'^This reverts commit ([0-9a-f]{40})(?:\.|, reversing)$', re.M)
REVERTS = re.compile(r'^Reverts:[ \t]*(#[1-9][0-9]*(?:[ \t]*,[ \t]*#[1-9][0-9]*)*)[ \t]*$', re.M | re.I)
FENCE = re.compile(r'^[ \t]{0,3}(`{3,}|~{3,}).*?(?:^[ \t]{0,3}\1[^\n]*$|\Z)', re.M | re.S)
# `none`, optionally with a parenthesized reason; «None of the files…» is a summary.
NONE = re.compile(r'none\.?(?:[ \t]*\([^()]*\))?\.?', re.I)
OKILUM = '{https://git.oklabs.uk/BeFeast/okilum/update-metadata}'
FORGEJO_START = '<!-- okilum-release-notes:start -->'
FORGEJO_END = '<!-- okilum-release-notes:end -->'


def forgejo_body(notes, existing):
    """Render the canonical sidecar items, replacing only our managed notes block.

    Publication's signing/channel footer stays verbatim. Stable can replace the
    Beta range with its pinned Stable range, including on an interrupted retry.
    """
    groups = {'feature': [], 'fix': []}
    for item in notes['items']:
        kind = item['kind']
        if kind in HIDDEN:
            continue
        if kind not in groups:
            raise ValueError('Unknown release-note kind')
        number = item['pr']
        if type(number) is not int or number <= 0:
            raise ValueError('Invalid release-note PR number')
        # The app displays authored summaries as plain text. Keep Markdown from
        # turning them into headings/HTML, and link issue references in that text.
        text = re.sub(r'([\\`*_\[\]<>])', r'\\\1', ' '.join(item['text'].split()))
        text = re.sub(r'(?<![\w/\\])#([1-9][0-9]*)\b',
                      r'[#\1](https://git.oklabs.uk/BeFeast/okilum/issues/\1)', text)
        groups[kind].append(f'- {text} ([#{number}](https://git.oklabs.uk/BeFeast/okilum/pulls/{number}))')
    sections = [f'### {title}\n\n' + '\n'.join(groups[kind])
                for kind, title in [('feature', 'New'), ('fix', 'Fixed')] if groups[kind]]
    summary = '\n\n'.join(sections) if sections else 'без видимых изменений'
    # Only bodies written by this renderer have a managed block. Legacy bodies
    # consist solely of the technical footer and are retained in full.
    if existing.startswith(FORGEJO_START + '\n') and FORGEJO_END in existing:
        existing = existing.split(FORGEJO_END, 1)[1].removeprefix('\n\n')
    block = f'{FORGEJO_START}\n{summary}\n{FORGEJO_END}'
    return block + ('\n\n' + existing if existing else '')


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


def authored_body(pr):
    # Template instructions/examples inside comments are not authored text.
    return re.sub(r'<!--.*?-->', '', pr.get('body') or '', flags=re.S)


def explicit_text(pr):
    """The first nonempty `User-visible:` line; exactly `none` (with an optional
    parenthesized reason) means hidden."""
    lines = re.findall(r'^User-visible:[ \t]*(.*?)[ \t]*$', authored_body(pr), flags=re.M | re.I)
    text = next((line.strip() for line in lines if line.strip()), '')
    return None if NONE.fullmatch(text) else text


def reverts_marker(pr):
    """PR numbers named by `Reverts: #N[, #M]` lines of their own, outside code fences."""
    body = FENCE.sub('', authored_body(pr))
    return {int(n) for line in REVERTS.findall(body) for n in re.findall(r'[0-9]+', line)}


def pr_item(pr, include_hidden=False):
    explicit = explicit_text(pr)
    if explicit is None and not include_hidden:
        return None
    explicit = explicit or ''
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


def git(*args):
    return subprocess.run(['git', *args], check=True, capture_output=True, text=True, timeout=30).stdout


def is_ancestor(commit, descendant):
    """A clean yes/no only; missing objects or history are an error, never «no»."""
    result = subprocess.run(['git', 'merge-base', '--is-ancestor', commit, descendant],
                            capture_output=True, text=True, timeout=30)
    if result.returncode not in (0, 1):
        raise ValueError(f'Cannot decide ancestry of {commit} in {descendant}: {result.stderr.strip()}')
    return result.returncode == 0


def has_commit(sha):
    return subprocess.run(['git', 'cat-file', '-e', f'{sha}^{{commit}}'],
                          capture_output=True, timeout=30).returncode == 0


def revert_trailers(log):
    """(commit, reverted sha) from `git log --format=%H%x00%s%x00%B%x1e` output."""
    found = []
    for entry in log.split('\x1e'):
        parts = entry.strip('\n').split('\0')
        # Git 2.43+ titles a revert of a revert `Reapply "…"`.
        if len(parts) == 3 and parts[1].startswith(('Revert "', 'Reapply "')):
            found += [(parts[0], target) for target in REVERT_TRAILER.findall(parts[2])]
    return found


class Commits:
    """Map any commit to the merged PR that brought it in (None: not a PR's change)."""

    def __init__(self, client, source, inventory, by_sha):
        self.client, self.inventory, self.by_sha = client, inventory, by_sha
        rows = [line.split() for line in git('rev-list', '--first-parent', '--parents', source).splitlines()]
        self.main = [row[0] for row in rows]
        # Squash/rebase merges leave the PR's own commits off main: only the
        # Forgejo PR commit list can name their owner.
        self.squashed = [by_sha[row[0]] for row in rows if len(row) <= 2 and row[0] in by_sha]
        self.cache, self.pr_commits = {}, {}

    def owner(self, sha):
        if sha not in self.cache:
            self.cache[sha] = self._owner(sha)
        return self.cache[sha]

    def _owner(self, sha):
        if sha in self.by_sha:
            return self.by_sha[sha]
        if has_commit(sha) and is_ancestor(sha, self.main[0]):
            # Oldest first-parent commit containing it; ancestry is monotonic.
            low, high = 0, len(self.main) - 1
            while low < high:
                middle = (low + high + 1) // 2
                if is_ancestor(sha, self.main[middle]):
                    low = middle
                else:
                    high = middle - 1
            return self.by_sha.get(self.main[low])
        for number in self.squashed:
            if sha in self._commits_of(number):
                return number
        return None

    def _commits_of(self, number):
        if number not in self.pr_commits:
            shas = set()
            for page in range(1, 101):
                rows = self.client.call('GET', f'/pulls/{number}/commits?limit=50&page={page}'
                                        '&stat=false&verification=false&files=false') or []
                shas |= {row['sha'] for row in rows}
                if len(rows) < 50:
                    break
            else:
                raise ValueError(f'Incomplete commit list for PR {number}')
            self.pr_commits[number] = shas
        return self.pr_commits[number]


def revert_edges(revision, commits, in_range, inventory):
    """Map each reverting PR (None: a revert pushed without a PR) to the PRs it reverts.

    Sources: a `Reverts: #N` line in an in-range PR body, and `git revert`
    commits reachable in the range, whose target may be a PR merge or squash
    commit, one of a PR's own commits, or a revert commit (a revert of a revert).
    """
    if git('rev-parse', '--is-shallow-repository').strip() == 'true':
        raise ValueError('Release notes need full history to detect reverts')
    edges = {}
    for number in in_range:
        for target in reverts_marker(inventory[number]):
            if target in inventory and target != number:
                edges.setdefault(number, set()).add(target)
    for sha, target in revert_trailers(git('log', '--format=%H%x00%s%x00%B%x1e', revision)):
        source, reverted = commits.owner(sha), commits.owner(target)
        if reverted is not None and reverted != source:
            edges.setdefault(source, set()).add(reverted)
    return edges


def is_revert_pr(pr):
    if reverts_marker(pr):
        return True
    merge = pr['merge_commit_sha']
    squashed = len(git('rev-list', '--no-walk', '--parents', merge).split()) <= 2
    revision = ['-1', merge] if squashed else [f'{merge}^1..{merge}']
    return bool(revert_trailers(git('log', '--format=%H%x00%s%x00%B%x1e', *revision)))


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
    inventory, by_sha = {}, {}
    # Closed PRs are paginated; a recently updated old PR must never enter the range.
    for page in range(1, 101):
        prs = client.call('GET', f'/pulls?state=closed&sort=recentupdate&limit=50&page={page}')
        for pr in prs:
            sha = pr.get('merge_commit_sha')
            if pr.get('merged') and sha:
                inventory[pr['number']] = pr
                by_sha[sha] = pr['number']
        if len(prs) < 50:
            break
    else:
        raise ValueError('Incomplete merged PR inventory for notes')
    in_range = {number: commits.index(pr['merge_commit_sha'])
                for number, pr in inventory.items() if pr['merge_commit_sha'] in commits}
    edges = revert_edges(revision, Commits(client, source, inventory, by_sha), in_range, inventory)
    # A reverted PR's line is false once its revert is in the same build. A
    # revert of a revert does not resurrect it: the re-land PR's own
    # User-visible line is the only claim that the change is back.
    dropped = {target for targets in edges.values() for target in targets if target in in_range}
    rows, removed = [], set()

    def removed_again(index, target):
        # Only a change that reached the previous build of this channel can be
        # «removed again»; a reverted revert is the re-land PR's to describe.
        earlier = inventory[target]
        if (target in removed or target in in_range or previous_source is None
                or not is_ancestor(earlier['merge_commit_sha'], previous_source) or is_revert_pr(earlier)):
            return
        item = pr_item(earlier)
        if item:
            removed.add(target)
            rows.append((index, target, {'pr': target, 'kind': 'fix', 'text': f'Removed again: {item["text"]}'}))

    for number, index in sorted(in_range.items(), key=lambda row: row[1]):
        if number in dropped:
            continue
        pr = inventory[number]
        if number in edges and not explicit_text(pr):
            # A revert without its own summary says only what it took away
            # from an earlier build; within one build the pair cancels out.
            for target in sorted(edges[number]):
                removed_again(index, target)
            continue
        item = pr_item(pr)
        if item:
            rows.append((index, number, item))
    # A revert pushed straight to main has no PR of its own.
    for target in sorted(edges.get(None, ())):
        removed_again(0, target)
    return [item for _, _, item in sorted(rows, key=lambda row: (row[0], row[1]))]


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
