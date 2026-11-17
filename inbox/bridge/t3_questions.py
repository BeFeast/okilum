#!/usr/bin/env python3
"""Narrow T3 protocol-2 question bridge; no launch, shell or Maestro actions."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import sys
import stat
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

LIMIT = 4 * 1024 * 1024
# Older question rows are recovered from bounded T3 history only. Exhausting a
# budget leaves the affected questions held; it never closes them.
HISTORY_PAGES = 20
HISTORY_BYTES = 8 * 1024 * 1024
HISTORY_CURSOR_BYTES = 4096
NAMESPACE = uuid.UUID('67e761b6-fc1c-4eb5-b170-633015602705')


class Unavailable(Exception):
    """Safe error code only; never wrap transport exceptions containing tokens."""


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False)


def digest(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def require(condition, code='invalid_source'):
    if not condition:
        raise Unavailable(code)


def text(value):
    require(isinstance(value, str) and 0 < len(value.encode()) <= 512 and
            not any(ord(c) < 32 for c in value))
    return value


def unique(rows, key):
    result = {}
    require(isinstance(rows, list))
    for row in rows:
        require(isinstance(row, dict))
        identity = text(row.get(key))
        require(identity not in result)
        result[identity] = row
    return result


def history_complete(snapshot):
    """Modern T3 ends history with an explicit null cursor. The removed legacy
    budget flag is honoured only when present; its absence proves nothing."""
    return (snapshot.get('hasMoreHistory') is False and
            snapshot.get('payloadBudgetExceeded', False) is False and
            (('historyCursor' in snapshot and snapshot['historyCursor'] is None) or
             ('historyCursor' not in snapshot and snapshot.get('payloadBudgetExceeded') is False)))


def missing_items(snapshot):
    """User-input requests whose question row is outside the bounded window."""
    require(isinstance(snapshot, dict))
    p = snapshot.get('projection', {})
    require(isinstance(p, dict) and isinstance(p.get('turnItems', []), list))
    present = {item.get('requestId') for item in p.get('turnItems', [])
               if isinstance(item, dict) and item.get('type') == 'user_input_request'}
    requests = p.get('runtimeRequests')
    require(isinstance(requests, list))
    return {text(r.get('id')) for r in requests
            if isinstance(r, dict) and r.get('kind') == 'user_input' and r.get('id') not in present}


def project(snapshot, config, thread, history=None):
    """Request state comes only from the snapshot's complete control state.

    T3 bounds turn items, not runtime requests, and always retains rows of
    pending requests. A request whose question row is neither in the window
    nor in recovered `history` is returned in `incomplete`: it is held, never
    withdrawn or acknowledged. Truncated control state fails the whole thread.
    """
    require(isinstance(snapshot, dict))
    require(snapshot.get('payloadBudgetExceeded', False) is False, 'partial_source')
    require(history_complete(snapshot) or snapshot.get('hasMoreHistory') is True, 'partial_source')
    sequence = snapshot.get('snapshotSequence')
    require(type(sequence) is int and 0 < sequence < 2**63)
    p = snapshot.get('projection', {})
    require(p.get('thread', {}).get('id') == thread and
            p['thread'].get('projectId') == config['source_project_id'], 'source_scope')
    require(thread in config['thread_ids'], 'source_scope')
    requests = unique(p.get('runtimeRequests'), 'id')
    turns = unique(p.get('providerTurns'), 'id')
    attempts = unique(p.get('attempts'), 'id')
    items = {}
    for item in p.get('turnItems', []):
        if item.get('type') == 'user_input_request':
            key = text(item.get('requestId'))
            require(key not in items)
            items[key] = item
    for key, item in (history or {}).items():
        items.setdefault(key, item)
    complete = history_complete(snapshot)
    result, incomplete = {}, set()
    for identity, request in requests.items():
        if request.get('kind') != 'user_input':
            continue
        turn_id = text(request.get('providerTurnId'))
        require(turn_id in turns)
        generation = text(turns[turn_id].get('runAttemptId'))
        require(generation in attempts)
        source = {'kind': 't3', 'instance_id': config['instance_id'],
                  'project_id': config['source_project_id'], 'thread_id': thread,
                  'question_id': identity, 'generation': generation}
        item = items.get(identity)
        if item is None:
            # A complete snapshot must contain every row; a bounded one may not.
            require(not complete, 'partial_source')
            incomplete.add(str(uuid.uuid5(NAMESPACE, canonical(source))))
            continue
        require(item.get('threadId') == thread and item.get('nodeId') == request.get('nodeId'))
        require(item.get('providerTurnId') == turn_id)
        require(attempts[generation].get('runId') == item.get('runId'))
        fields = []
        native = unique(item.get('questions'), 'id')
        require(0 < len(native) <= 10)
        for field_id, field in native.items():
            prompt = field.get('question')
            require(isinstance(prompt, str) and 0 < len(prompt.encode()) <= 32768)
            # Optional questions/attachments are not supported by this pilot UI.
            require(field.get('required', True) is True, 'unsupported_question')
            options = []
            seen = set()
            require(isinstance(field.get('options'), list) and len(field['options']) <= 100)
            for option in field['options']:
                value = text(option.get('value', option.get('label')))
                require(value not in seen)
                seen.add(value)
                label = option.get('label')
                description = option.get('description', '')
                require(isinstance(label, str) and isinstance(description, str))
                label = label + (' — ' + description if description else '')
                require(0 < len(label.encode()) <= 4096)
                options.append({'id': value, 'label': label})
            allow = field.get('allowCustomAnswer', True)
            multiple = field.get('multiSelect', False)
            require(type(allow) is bool and type(multiple) is bool and (allow or options))
            fields.append({'id': field_id, 'prompt': prompt, 'options': options,
                           'allow_text': allow, 'multiple': multiple})
        require(len(canonical(fields).encode()) <= 65536)
        status = request.get('status')
        require(status in ('pending', 'resolved', 'expired', 'cancelled'))
        capability = request.get('responseCapability', {})
        live = capability.get('type') == 'live' and isinstance(capability.get('providerSessionId'), str)
        q = {'id': str(uuid.uuid5(NAMESPACE, canonical(source))),
             'project_id': config['project_id'], 'source': source,
             'source_revision': digest({'source': source, 'fields': fields,
                                        'capability': capability}),
             'state': {'pending': 'pending', 'resolved': 'answered',
                       'expired': 'withdrawn', 'cancelled': 'withdrawn'}[status],
             'fields': fields, 'can_reply': status == 'pending' and live and
                 p['thread'].get('archivedAt') is None and p['thread'].get('deletedAt') is None}
        title = p['thread'].get('title')
        if isinstance(title, str) and title.strip() and len(title.encode()) <= 1024:
            q['thread_title'] = title
        result[q['id']] = (q, request.get('answers'))
    return sequence, result, incomplete


def recover_history(source, thread, snapshot, wanted):
    """Find question rows for `wanted` request IDs in bounded older history.

    Stops as soon as every row is found. Page count, total bytes and cursor
    size are finite; a repeated cursor, an exhausted budget or a page read
    against a different snapshot leaves the rest unrecovered (held). Rows are
    used only if the snapshot is unchanged afterwards, so an old question body
    is never combined with newer request/attempt state.
    """
    found = {}
    if not wanted or snapshot.get('hasMoreHistory') is not True:
        return found
    sequence = snapshot.get('snapshotSequence')
    cursor, seen, size = snapshot.get('historyCursor'), set(), 0
    for _ in range(HISTORY_PAGES):
        if not (isinstance(cursor, str) and 0 < len(cursor.encode()) <= HISTORY_CURSOR_BYTES
                and cursor not in seen):
            break
        seen.add(cursor)
        try:
            page = source.history(thread, cursor)
        except Unavailable:
            break  # Older history is unreachable; keep the rest held.
        if not isinstance(page, dict):
            break
        size += len(canonical(page).encode())
        if (size > HISTORY_BYTES or not isinstance(page.get('items'), list) or
                type(page.get('hasMoreHistory')) is not bool or 'nextCursor' not in page or
                page.get('snapshotSequence', sequence) != sequence):
            break
        for row in page['items']:
            require(isinstance(row, dict) and isinstance(row.get('item'), dict), 'source_scope')
            item = row['item']
            require(row.get('visibility') in ('local', 'inherited') and
                    row.get('sourceThreadId') == text(item.get('threadId')) and
                    row.get('sourceItemId') == text(item.get('id')), 'source_scope')
            if item['threadId'] != thread:
                # Inherited history cannot answer a request in this thread.
                require(row['visibility'] == 'inherited', 'source_scope')
                continue
            if item.get('type') != 'user_input_request':
                continue
            key = text(item.get('requestId'))
            require(found.get(key, item) == item, 'source_scope')
            if key in wanted:
                found[key] = item
        if wanted <= found.keys():
            break
        cursor = page['nextCursor']
        if not page['hasMoreHistory']:
            break
    if not found:
        return found
    current = source.snapshot(thread)
    if not isinstance(current, dict) or current.get('snapshotSequence') != sequence:
        return {}
    return found


def answers(operation):
    """Exact native option values; custom text and options cannot be combined."""
    q, r = operation['question'], operation['request']
    require(r['question_id'] == q['id'] and r['expected_revision'] == q['source_revision'], 'invalid_intent')
    fields = unique(q['fields'], 'id')
    replies = unique(r['answers'], 'id')
    require(fields.keys() == replies.keys(), 'invalid_intent')
    result = {}
    for key, field in fields.items():
        answer = replies[key]
        custom, selected = answer['text'], answer['option_ids']
        require(isinstance(custom, str) and isinstance(selected, list), 'invalid_intent')
        require(len(custom.encode()) <= 16384, 'invalid_intent')
        require(not (custom and selected), 'ambiguous_answer')
        if custom:
            require(field['allow_text'] and custom.strip(), 'invalid_intent')
            result[key] = custom
        else:
            valid = {o['id'] for o in field['options']}
            require(selected and all(isinstance(s, str) and s in valid for s in selected), 'invalid_intent')
            require(len(set(selected)) == len(selected), 'invalid_intent')
            require(field['multiple'] or len(selected) == 1, 'invalid_intent')
            result[key] = selected if field['multiple'] else selected[0]
    return result


class Journal:
    """Local durable operational state; backups are never permission to resend."""
    def __init__(self, path, config):
        path = Path(path)
        require(path.parent.is_dir() and not path.parent.is_symlink(), 'private_state_required')
        require(stat.S_IMODE(path.parent.stat().st_mode) & 0o077 == 0, 'private_state_required')
        if path.exists() or path.is_symlink():
            require(path.is_file() and not path.is_symlink(), 'private_state_required')
            require(stat.S_IMODE(path.stat().st_mode) & 0o077 == 0, 'private_state_required')
        self.db = sqlite3.connect(path)
        os.chmod(path, 0o600)
        self.db.executescript('''
        PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS binding(id INTEGER PRIMARY KEY CHECK(id=1), value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS observations(thread TEXT PRIMARY KEY, sequence INTEGER NOT NULL, body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS intents(id TEXT PRIMARY KEY, body TEXT NOT NULL, command TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS dispatch_attempts(id TEXT PRIMARY KEY, command TEXT NOT NULL);
        ''')
        binding = canonical({k: config[k] for k in ('instance_id','source_project_id','project_id','t3_url','inbox_url')})
        with self.db:
            self.db.execute('INSERT OR IGNORE INTO binding VALUES(1,?)', (binding,))
        if self.db.execute('SELECT value FROM binding').fetchone()[0] != binding:
            self.db.close()
            raise Unavailable('state_binding_changed')

    def observe(self, thread, sequence, records):
        body = canonical(records)
        old = self.db.execute('SELECT sequence,body FROM observations WHERE thread=?', (thread,)).fetchone()
        if old:
            require(sequence >= old[0], 'source_cursor_regressed')
            # Titles are display metadata; source identity/fields/answers stay exact.
            def without_titles(value):
                rows = json.loads(value)
                for question, _ in rows.values():
                    question.pop('thread_title', None)
                return rows
            if sequence == old[0]:
                # One cursor is one source state. Recovery may add or lose
                # older rows between polls, but never change a known question.
                now, before = without_titles(body), without_titles(old[1])
                require(all(now[k] == before[k] for k in now.keys() & before.keys()),
                        'source_cursor_conflict')
        with self.db:
            self.db.execute('INSERT INTO observations VALUES(?,?,?) ON CONFLICT(thread) DO UPDATE SET sequence=excluded.sequence,body=excluded.body', (thread, sequence, body))

    def intent(self, op, command):
        payload = canonical({'request': op['request'], 'question': op['question']})
        key = op['request']['operation_id']
        old = self.db.execute('SELECT body,command FROM intents WHERE id=?', (key,)).fetchone()
        if old:
            require(old == (payload, canonical(command)), 'operation_payload_changed')
            return False
        with self.db:
            self.db.execute('INSERT INTO intents VALUES(?,?,?)', (key, payload, canonical(command)))
        return True

    def attempted(self, op, command):
        """True only if this journal committed a dispatch attempt for `op`."""
        row = self.db.execute('SELECT command FROM dispatch_attempts WHERE id=?',
                              (op['request']['operation_id'],)).fetchone()
        require(row is None or row[0] == canonical(command), 'operation_payload_changed')
        return row is not None

    def mark_attempt(self, op, command):
        # Committed after Inbox holds `uncertain` and before source I/O. A crash
        # here allows only observation-based recovery, never a second send.
        require(op['state'] == 'uncertain', 'invalid_intent')
        with self.db:
            self.db.execute('INSERT INTO dispatch_attempts VALUES(?,?)',
                            (op['request']['operation_id'], canonical(command)))


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        raise Unavailable('redirect_rejected')


class Http:
    def __init__(self, base, token):
        self.base, self.token = base.rstrip('/'), token
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def request(self, method, path, body=None):
        request = urllib.request.Request(self.base + path, method=method,
            data=None if body is None else canonical(body).encode(),
            headers={'Authorization': 'Bearer ' + self.token, 'Content-Type': 'application/json',
                     'x-t3-orchestration-protocol': '2'})
        try:
            with self.opener.open(request, timeout=15) as response:
                data = response.read(LIMIT + 1)
            require(len(data) <= LIMIT, 'response_too_large')
            return json.loads(data) if data else None
        except urllib.error.HTTPError as error:
            error.close()
            raise Unavailable('http_not_found' if error.code == 404 else 'http_unavailable') from None
        except (OSError, ValueError):
            raise Unavailable('http_unavailable') from None


class T3(Http):
    def snapshot(self, thread):
        descriptor = self.request('GET', '/.well-known/t3/environment')
        require(descriptor.get('orchestrationProtocolVersion') == 2, 'unsupported_protocol')
        return self.request('GET', '/api/orchestration/threads/' + urllib.parse.quote(thread, safe='') + '/bounded')

    def history(self, thread, cursor):
        return self.request('GET', '/api/orchestration/threads/' + urllib.parse.quote(thread, safe='') +
                            '/history?' + urllib.parse.urlencode({'cursor': cursor}))

    def respond(self, command):
        # Transport is deliberately limited to native question answers.
        require(command['type'] == 'runtime-request.respond', 'unsupported_command')
        result = self._rpc('orchestration.dispatchCommand', command)
        require(type(result.get('sequence')) is int, 'source_outcome_uncertain')

    def launch(self, command):
        result = self._rpc('orchestration.launchThread', command)
        require(result.get('threadId') == command['threadId'], 'source_outcome_uncertain')

    def _rpc(self, method, command):
        require(method in ('orchestration.dispatchCommand', 'orchestration.launchThread'), 'unsupported_command')
        try:
            from websockets.sync.client import connect
            parts = urllib.parse.urlsplit(self.base)
            url = urllib.parse.urlunsplit(('wss' if parts.scheme == 'https' else 'ws',
                parts.netloc, parts.path + '/ws', 'orchestrationProtocol=2', ''))
            with connect(url, additional_headers={'Authorization': 'Bearer ' + self.token},
                         proxy=None, open_timeout=15, close_timeout=1, max_size=LIMIT) as ws:
                ws.send(canonical({'_tag':'Request','id':command['commandId'],
                    'tag':method,'headers':[],'payload':command}))
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    incoming = json.loads(ws.recv(timeout=max(0.01, deadline-time.monotonic())))
                    for message in incoming if isinstance(incoming, list) else [incoming]:
                        if message.get('_tag') == 'Ping':
                            ws.send('{"_tag":"Pong"}')
                        elif message.get('_tag') == 'Exit' and str(message.get('requestId')) == command['commandId']:
                            result = message.get('exit', {})
                            require(result.get('_tag') == 'Success' and isinstance(result.get('value'), dict), 'source_outcome_uncertain')
                            return result['value']
                raise Unavailable('source_outcome_uncertain')
        except Exception:
            raise Unavailable('source_outcome_uncertain') from None


class Inbox(Http):
    def observe(self, question, sequence):
        self.request('POST', '/api/bridge/v1/questions', {'question': question, 'sequence': sequence})

    def operations(self):
        cursor = 0
        for _ in range(100):
            page = self.request('GET', f'/api/bridge/v1/replies?after={cursor}&limit=100')
            require(isinstance(page.get('operations'), list), 'invalid_inbox_page')
            yield from page['operations']
            if page.get('has_more') is False:
                return
            next_cursor = page.get('next_cursor')
            require(type(next_cursor) is int and next_cursor > cursor, 'invalid_inbox_page')
            cursor = next_cursor
        raise Unavailable('inbox_page_limit')

    def advance(self, operation, next_state, error=None):
        self.request('POST', '/api/bridge/v1/replies/' + operation['request']['operation_id'],
                     {'expected': operation['state'], 'next': next_state,
                      'delivery_id': operation.get('delivery_id'), 'error_code': error})
        operation['state'] = next_state


class Runner:
    def __init__(self, config, journal, source, inbox):
        self.config, self.journal, self.source, self.inbox = config, journal, source, inbox
        # Operations queued before this process first listed them are never
        # released automatically; neither is work on a thread not yet observed.
        self.quarantine = None
        self.ready_threads = set()
        self.partial = {}

    def observe(self, thread):
        snapshot = self.source.snapshot(thread)
        missing = missing_items(snapshot)
        history = recover_history(self.source, thread, snapshot, missing) if missing else {}
        sequence, records, incomplete = project(snapshot, self.config, thread, history)
        self.journal.observe(thread, sequence, records)
        for question, _ in records.values():
            self.inbox.observe(question, sequence)
        if incomplete:
            self.partial[thread] = incomplete
        else:
            self.partial.pop(thread, None)
        return records, incomplete

    def step(self):
        """One poll. Returns threads whose source is partial (held, not failed).

        A partial thread never blocks healthy threads. Its unrecoverable
        questions are not re-observed, so Inbox shows them as stale and locks
        replies; their operations are left untouched until a full refresh.
        """
        operations = list(self.inbox.operations())
        if self.quarantine is None:
            self.quarantine = {o['request']['operation_id'] for o in operations if o['state'] == 'queued'}
        observed = set()
        for thread in self.config['thread_ids']:
            try:
                self.observe(thread)
                observed.add(thread)
            except Unavailable as error:
                if str(error) != 'partial_source':
                    raise
                self.partial[thread] = None
        for operation in operations:
            if operation['state'] in ('delivered', 'rejected'):
                continue
            thread = operation['question']['source'].get('thread_id')
            if thread in self.config['thread_ids'] and thread not in observed:
                continue  # Partial thread: held until a full refresh.
            try:
                self.reconcile(operation, recovery=operation['request']['operation_id'] in self.quarantine
                               or thread not in self.ready_threads)
            except Unavailable as error:
                # The pre-dispatch read can turn partial too: hold this thread only.
                if str(error) != 'partial_source':
                    raise
                self.partial[thread] = None
                observed.discard(thread)
        self.ready_threads |= observed
        return sorted(self.partial)

    def reconcile(self, op, recovery):
        q = op['question']
        source = q['source']
        require(source['kind'] == 't3' and source['instance_id'] == self.config['instance_id'] and
                source['project_id'] == self.config['source_project_id'] and
                q['project_id'] == self.config['project_id'] and
                source['thread_id'] in self.config['thread_ids'], 'operation_scope')
        # Fresh authoritative snapshot immediately before any attempted response.
        records, incomplete = self.observe(source['thread_id'])
        if q['id'] in incomplete:
            # The request exists but its question row is unrecoverable right
            # now. Partial data neither withdraws nor acknowledges it.
            return
        record = records.get(q['id'])
        if record is None:
            # Absence is not evidence of cancellation; no external action. A
            # replacement attempt is a different identity and never receives it.
            if op['state'] == 'queued':
                self.inbox.advance(op, 'uncertain', 'source_request_unavailable')
            return
        current, native_answers = record
        command = {'type':'runtime-request.respond',
                   'commandId':str(uuid.uuid5(NAMESPACE, 'reply:' + op['request']['operation_id'])),
                   'threadId':source['thread_id'],'requestId':source['question_id'],
                   'answers':answers(op)}
        require(current['source'] == source, 'source_identity_changed')
        fresh = self.journal.intent(op, command)
        # The revision covers source identity, attempt, fields and the response
        # capability (provider session). T3 keeps it unchanged on resolve.
        exact = current['source_revision'] == q['source_revision']
        if current['state'] == 'answered' and exact:
            # T3 persists resolved before provider callback: prove acceptance
            # only, and only for a dispatch this journal actually attempted.
            if native_answers != command['answers']:
                self.inbox.advance(op, 'rejected', 'source_answer_differs')
            elif not self.journal.attempted(op, command):
                if op['state'] == 'queued':
                    self.inbox.advance(op, 'uncertain', 'source_outcome_unconfirmed')
            else:
                if op['state'] == 'queued':
                    self.inbox.advance(op, 'uncertain')
                if op['state'] == 'uncertain':
                    self.inbox.advance(op, 'accepted')
            return
        if current['state'] != 'pending' or not exact or not current['can_reply']:
            # For an already attempted operation, a changed revision or source
            # expiry cannot prove that the earlier command wasn't applied.
            if op['state'] == 'queued' and fresh and not recovery:
                self.inbox.advance(op, 'rejected', 'source_stale')
            elif op['state'] == 'queued':
                self.inbox.advance(op, 'uncertain', 'recovery_required')
            return
        if op['state'] != 'queued':
            return
        self.inbox.advance(op, 'uncertain', 'recovery_required' if recovery or not fresh else None)
        if recovery or not fresh:
            return
        self.journal.mark_attempt(op, command)
        self.source.respond(command)
        # A receipt means accepted, not delivered. Lost receipts are reconciled
        # on a later step without invoking respond again.
        self.inbox.advance(op, 'accepted')


def private_json(path):
    path = Path(path)
    meta = path.lstat()
    require(stat.S_ISREG(meta.st_mode) and stat.S_IMODE(meta.st_mode) & 0o077 == 0 and meta.st_size <= 16384,
            'private_configuration_required')
    return json.loads(path.read_text())


def configuration(path):
    config = private_json(path)
    require(set(config) - {'launch_targets'} == {'instance_id','source_project_id','project_id','thread_ids',
                            't3_url','inbox_url','t3_credential_file','inbox_credential_file','state_file'}, 'invalid_configuration')
    for name in ('instance_id','source_project_id','project_id'):
        text(config[name])
    require(str(uuid.UUID(config['project_id'])) == config['project_id'], 'invalid_configuration')
    require(isinstance(config['thread_ids'], list) and 0 < len(config['thread_ids']) <= 20, 'invalid_configuration')
    require(len(set(config['thread_ids'])) == len(config['thread_ids']), 'invalid_configuration')
    for thread in config['thread_ids']:
        text(thread)
    for name in ('t3_url', 'inbox_url'):
        url = urllib.parse.urlsplit(config[name])
        require(url.hostname and not url.username and not url.password and not url.query and not url.fragment and
                url.path in ('', '/'), 'invalid_configuration')
        require(url.scheme == 'https' or (name == 't3_url' and url.scheme == 'http' and
                url.hostname in ('localhost', '127.0.0.1', '::1')), 'https_required')
    return config


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--once', action='store_true', help='Observe/reconcile only; startup never sends queued replies')
    args = parser.parse_args()
    os.umask(0o077)
    config = configuration(args.config)
    # One bridge process per state file; no parallel dispatchers.
    import fcntl
    state = Path(config['state_file'])
    journal = Journal(state, config)
    lock_path = state.with_suffix('.lock')
    fd = os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    credentials = [private_json(config[name]) for name in ('t3_credential_file','inbox_credential_file')]
    for credential in credentials:
        require(isinstance(credential.get('token'), str) and len(credential['token']) >= 32, 'invalid_credential')
    source, inbox = T3(config['t3_url'],credentials[0]['token']), Inbox(config['inbox_url'],credentials[1]['token'])
    runner = Runner(config,journal,source,inbox)
    from t3_launch import LaunchRunner
    launches = LaunchRunner(config,journal,source,inbox) if config.get('launch_targets') else None
    while True:
        try:
            partial = runner.step()
            if launches:
                launches.step()
            if partial:
                print(canonical({'event':'bridge_sync','status':'partial','code':'partial_source',
                                 'partial_threads':partial}), flush=True)
            else:
                print('{"event":"bridge_sync","status":"ok"}', flush=True)
        except Unavailable as error:
            print(canonical({'event':'bridge_sync','status':'unavailable','code':str(error)}), flush=True)
        if args.once:
            break
        time.sleep(5)


if __name__ == '__main__':
    sys.modules.setdefault('t3_questions', sys.modules[__name__])
    try:
        main()
    except Exception:
        # Never print config, credentials, source snapshots, answer bodies or
        # third-party exceptions, including on startup failures.
        print('{"event":"bridge_stopped","status":"configuration_or_storage_error"}', flush=True)
        raise SystemExit(1) from None
