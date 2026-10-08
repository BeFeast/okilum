#!/usr/bin/env python3
"""Guarded Maestro Inbox adapter; no fleet control, launches or automatic resend."""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import stat
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from t3_questions import (Inbox, NoRedirect, Unavailable, canonical, digest,
                          private_json, require, text, unique)

NAMESPACE = uuid.UUID('c17fc904-8a31-4ab1-9122-54162cc15f32')
LIMIT = 16 * 1024 * 1024


def bounded(value, limit, required=True):
    require(isinstance(value, str) and len(value.encode()) <= limit and
            (not required or value.strip()) and '\0' not in value)
    return value


def approval_target(approval):
    target = approval.get('target')
    # Maestro omits a null target for this global action. Preserve that meaning
    # explicitly in Inbox; missing targets for scoped actions remain invalid.
    if target is None and approval.get('action') == 'change_global_config':
        return {'scope': 'global'}
    require(isinstance(target, dict) and len(canonical(target).encode()) <= 16384)
    return target


def project(raw, config, approval=False):
    require(isinstance(raw, dict))
    require(raw.get('instance_id') == config['instance_id'] and
            raw.get('project_id') == config['source_project_id'], 'source_scope')
    require(raw.get('kind') == ('approval' if approval else 'question'))
    caps = raw.get('capabilities')
    require(isinstance(caps, list) and all(isinstance(c, str) for c in caps) and
            len(set(caps)) == len(caps))
    revision = text(raw.get('revision'))
    identity = text(raw.get('id' if approval else 'question_id'))
    source = {'kind': 'maestro', 'instance_id': config['instance_id'],
              'project_id': config['source_project_id'], 'question_id': identity}
    detail = None
    if approval:
        a = raw.get('approval')
        require(isinstance(a, dict) and a.get('id') == identity)
        action = text(a.get('action'))
        require(action != 'stop_worker' and action in config['approval_actions'], 'unsupported_action')
        require(set(caps) <= {'approve', 'reject'})
        target = approval_target(a)
        detail = {'action': action, 'target': target,
                  'repo': bounded(a['repo'], 1024) if a.get('repo') else None,
                  'summary': bounded(a.get('summary'), 16384), 'risk': text(a.get('risk')),
                  'payload_hash': text(a.get('payload_hash')),
                  'target_state_hash': a.get('target_state_hash') or None}
        if detail['target_state_hash'] is not None:
            text(detail['target_state_hash'])
        source.update(record_kind='approval', thread_id='approval', generation='approval')
        status = a.get('status')
        require(status in ('pending', 'approved', 'rejected', 'stale', 'superseded', 'executed',
                           'execution_failed', 'execution_skipped', 'awaiting_dispatch', 'executing'))
        state = 'pending' if status == 'pending' else 'withdrawn' if status in ('stale', 'superseded') else 'answered'
        # Terminal records have no capabilities but remain readable.
        choices = sorted(caps) or ['approve', 'reject']
        fields = [{'id': 'decision', 'prompt': detail['summary'], 'options':
                   [{'id': c, 'label': c.title()} for c in choices], 'allow_text': False, 'multiple': False}]
        can_reply = status == 'pending' and bool(caps)
    else:
        require(set(caps) <= {'reply'})
        source.update(worker_id=text(raw.get('worker_id')), thread_id=text(raw.get('thread_id')),
                      generation=text(raw.get('worker_generation')))
        status = raw.get('status')
        require(status in ('pending', 'answered', 'withdrawn'))
        state = status
        options = unique(raw.get('options') or [], 'id')
        require(len(options) <= 100 and type(raw.get('allow_text')) is bool)
        fields = [{'id': 'answer', 'prompt': bounded(raw.get('prompt'), 32768),
                   'options': [{'id': k, 'label': bounded(v.get('label'), 4096)} for k, v in options.items()],
                   'allow_text': raw['allow_text'], 'multiple': False}]
        require(fields[0]['allow_text'] or options)
        can_reply = status == 'pending' and 'reply' in caps and not raw.get('pending_operation_id')
    # Capability changes must invalidate displayed consent even if the native
    # revision did not change. Native revision is retained verbatim, not parsed.
    display_revision = canonical([revision, sorted(caps)])
    require(len(display_revision.encode()) <= 512)
    q = {'id': str(uuid.uuid5(NAMESPACE, canonical(source))), 'project_id': config['project_id'],
         'source': source, 'source_revision': display_revision, 'state': state,
         'fields': fields, 'can_reply': can_reply,
         'thread_title': 'Maestro approval' if approval else 'Maestro question'}
    if detail is not None:
        q['approval'] = detail
    require(len(canonical(q).encode()) <= 100 * 1024)
    return q


class Source:
    def __init__(self, config, token):
        self.config, self.token = config, token
        self.base = config['maestro_url'].rstrip('/') + '/api/v1/inbox/projects/' + urllib.parse.quote(config['source_project_id'], safe='')
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def request(self, method, suffix, body=None):
        req = urllib.request.Request(self.base + suffix, method=method,
            data=None if body is None else canonical(body).encode(),
            headers={'Authorization': 'Bearer ' + self.token, 'Content-Type': 'application/json'})
        try:
            try:
                response = self.opener.open(req, timeout=15)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                status = response.code
                data = response.read(LIMIT + 1)
            require(len(data) <= LIMIT, 'response_too_large')
            result = json.loads(data) if data else {}
            require(isinstance(result, dict), 'invalid_source_response')
            return status, result
        except Unavailable:
            raise
        except (OSError, ValueError):
            raise Unavailable('source_unavailable') from None

    def read(self, suffix):
        status, data = self.request('GET', suffix)
        require(status == 200, 'source_unavailable')
        require(data.get('instance_id') == self.config['instance_id'], 'source_instance_changed')
        return data

    def snapshot(self):
        snap = self.read('/questions')
        require(type(snap.get('truncated')) is bool and type(snap.get('snapshot_cursor')) is int and snap['snapshot_cursor'] >= 0)
        records = unique(snap.get('questions'), 'question_id')
        cursor = snap['snapshot_cursor']
        if snap['truncated']:
            records, cursor = {}, 0
            for _ in range(100):
                page = self.read('/changes?cursor=' + str(cursor))
                require(isinstance(page.get('changes'), list) and type(page.get('has_more')) is bool)
                for change in page['changes']:
                    require(isinstance(change, dict) and type(change.get('cursor')) is int and change['cursor'] > cursor)
                    q = change.get('question'); require(isinstance(q, dict))
                    records[text(q.get('question_id'))] = q
                    cursor = change['cursor']
                next_cursor = page.get('next_cursor')
                require(type(next_cursor) is int and next_cursor == cursor)
                require(not page['has_more'] or next_cursor > 0 and bool(page['changes']), 'source_cursor_stalled')
                cursor = next_cursor
                if not page['has_more']:
                    break
            else:
                raise Unavailable('source_page_limit')
        require(cursor >= snap['snapshot_cursor'], 'incomplete_change_feed')
        approvals = unique(self.read('/approvals').get('approvals'), 'id')
        result = {}
        for raw in records.values():
            q = project(raw, self.config); result[q['id']] = q
        for raw in approvals.values():
            # Source may expose more actions than this narrower pilot allows.
            action = raw.get('approval', {}).get('action')
            if action not in self.config['approval_actions'] or action == 'stop_worker':
                continue
            q = project(raw, self.config, True); result[q['id']] = q
        return cursor, result

    def lookup(self, operation_id):
        return self.request('GET', '/operations/' + urllib.parse.quote(operation_id, safe=''))

    def send(self, path, body):
        # No HTTP retries, including after timeout or ambiguous server failure.
        return self.request('POST', path, body)


class Journal:
    def __init__(self, path, config):
        path = Path(path)
        require(path.parent.is_dir() and not path.parent.is_symlink() and stat.S_IMODE(path.parent.stat().st_mode) & 0o077 == 0, 'private_state_required')
        if path.exists() or path.is_symlink():
            require(path.is_file() and not path.is_symlink() and stat.S_IMODE(path.stat().st_mode) & 0o077 == 0, 'private_state_required')
        self.db = sqlite3.connect(path)
        os.chmod(path, 0o600)
        self.db.executescript('''PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
          CREATE TABLE IF NOT EXISTS binding(id INTEGER PRIMARY KEY CHECK(id=1),body TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS observation(id INTEGER PRIMARY KEY CHECK(id=1),sequence INTEGER NOT NULL,cursor INTEGER NOT NULL,body TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS intents(id TEXT PRIMARY KEY,body TEXT NOT NULL);''')
        binding = canonical({k: config[k] for k in ('instance_id','source_project_id','project_id','maestro_url','inbox_url','approval_actions')})
        with self.db:
            self.db.execute('INSERT OR IGNORE INTO binding VALUES(1,?)', (binding,))
        if self.db.execute('SELECT body FROM binding').fetchone()[0] != binding:
            self.db.close()
            raise Unavailable('state_binding_changed')

    def observe(self, cursor, records):
        old = self.db.execute('SELECT sequence,cursor,body FROM observation').fetchone()
        require(old is None or cursor >= old[1], 'source_cursor_regressed')
        body = canonical(records)
        seq = 1 if old is None else old[0] + (body != old[2])
        with self.db:
            self.db.execute('INSERT OR REPLACE INTO observation VALUES(1,?,?,?)', (seq, cursor, body))
        return seq

    def intent(self, op, path, command):
        body = canonical({'question': op['question'], 'request': op['request'], 'path': path, 'command': command})
        old = self.db.execute('SELECT body FROM intents WHERE id=?', (op['request']['operation_id'],)).fetchone()
        require(old is None or old[0] == body, 'operation_payload_changed')
        with self.db:
            self.db.execute('INSERT OR IGNORE INTO intents VALUES(?,?)', (op['request']['operation_id'], body))
        return old is None


def command(op):
    q, r = op['question'], op['request']; s = q['source']
    require(r['question_id'] == q['id'] and r['expected_revision'] == q['source_revision'], 'invalid_intent')
    revision, capabilities = json.loads(q['source_revision'])
    require(isinstance(revision, str) and isinstance(capabilities, list), 'invalid_intent')
    body = {'operation_id': text(r['operation_id']), 'instance_id': s['instance_id'],
            'project_id': s['project_id'], 'expected_revision': revision}
    require(len(r['answers']) == 1 and len(q['fields']) == 1, 'invalid_intent')
    answer, field = r['answers'][0], q['fields'][0]
    require(answer['id'] == field['id'], 'invalid_intent')
    selected, custom = answer['option_ids'], answer['text']
    require(isinstance(selected, list) and isinstance(custom, str) and len(custom.encode()) <= 16384 and not (selected and custom), 'invalid_intent')
    require(len(selected) <= 1 and all(v in [o['id'] for o in field['options']] for v in selected), 'invalid_intent')
    require((custom.strip() and field['allow_text']) or selected, 'invalid_intent')
    identity = urllib.parse.quote(s['question_id'], safe='')
    if q.get('approval'):
        require(s.get('record_kind') == 'approval' and field['id'] == 'decision' and not custom and
                len(selected) == 1 and selected[0] in ('approve', 'reject') and selected[0] in capabilities, 'invalid_intent')
        return '/approvals/' + identity + '/' + selected[0], body
    require(s.get('worker_id') and 'reply' in capabilities, 'invalid_intent')
    body.update(thread_id=s['thread_id'], worker_generation=s['generation'], answer={'text':custom, 'option_ids':selected})
    return '/questions/' + identity + '/reply', body


class Runner:
    def __init__(self, config, journal, source, inbox):
        self.config, self.journal, self.source, self.inbox = config, journal, source, inbox
        self.ready = False

    def observe(self):
        cursor, records = self.source.snapshot()
        sequence = self.journal.observe(cursor, records)
        for q in records.values():
            self.inbox.observe(q, sequence)
        return records

    def receipt(self, op, status, receipt):
        if status == 404:
            return False
        require(status in (200, 409) and receipt.get('operation_id') == op['request']['operation_id'], 'receipt_unconfirmed')
        state = receipt.get('status')
        require((status == 409 and state == 'rejected') or (status == 200 and state in ('accepted','delivered')), 'receipt_unconfirmed')
        if status == 200:
            q = op['question']; source = q['source']; path, expected = command(op)
            if q.get('approval'):
                current = receipt.get('current', {}); a = current.get('approval', {})
                require(current.get('project_id') == source['project_id'] and current.get('id') == source['question_id'], 'receipt_identity_mismatch')
                detail = q['approval']
                require(approval_target(a) == detail['target'] and all(a.get(k) == detail[k] for k in ('action','payload_hash')) and (a.get('repo') or None) == detail.get('repo') and (a.get('target_state_hash') or None) == detail.get('target_state_hash'), 'receipt_payload_mismatch')
                require(state == 'delivered' and a.get('status') == ('approved' if path.endswith('/approve') else 'rejected'), 'receipt_decision_mismatch')
            else:
                current = receipt.get('question', {})
                require(all(current.get(native) == source[local] for native, local in
                    [('instance_id','instance_id'),('project_id','project_id'),('worker_id','worker_id'),('thread_id','thread_id'),('worker_generation','generation'),('question_id','question_id')]), 'receipt_identity_mismatch')
                answer = current.get('answer', {})
                require(answer.get('text', '') == expected['answer']['text'] and (answer.get('option_ids') or []) == expected['answer']['option_ids'], 'receipt_payload_mismatch')
                require(current.get('pending_operation_id' if state == 'accepted' else 'answer_operation_id') == expected['operation_id'], 'receipt_identity_mismatch')
        if op['state'] == 'queued':
            self.inbox.advance(op, 'uncertain')
        if op['state'] != state:
            self.inbox.advance(op, state, 'source_rejected' if state == 'rejected' else None)
        return True

    def reconcile(self, op, recovery):
        q = op['question']; s = q['source']
        require(s['kind'] == 'maestro' and s['instance_id'] == self.config['instance_id'] and
                s['project_id'] == self.config['source_project_id'] and q['project_id'] == self.config['project_id'], 'source_scope')
        if q.get('approval'):
            require(q['approval']['action'] in self.config['approval_actions'], 'source_scope')
        path, body = command(op)
        fresh = self.journal.intent(op, path, body)
        status, receipt = self.source.lookup(body['operation_id'])
        if self.receipt(op, status, receipt):
            return
        if op['state'] != 'queued':
            return
        if recovery or not fresh:
            self.inbox.advance(op, 'uncertain', 'recovery_required'); return
        # Re-read immediately before dispatch; source still atomically checks the
        # exact native revision to close the race with this observation.
        current = self.observe().get(q['id'])
        if not current or current['source'] != s or current['source_revision'] != q['source_revision'] or not current['can_reply']:
            self.inbox.advance(op, 'rejected', 'source_stale'); return
        require(current['fields'] == q['fields'] and current.get('approval') == q.get('approval'), 'source_payload_changed')
        self.inbox.advance(op, 'uncertain')
        status, receipt = self.source.send(path, body)
        self.receipt(op, status, receipt)

    def step(self):
        operations = list(self.inbox.operations())
        self.observe()
        for op in operations:
            if op['state'] not in ('delivered','rejected'):
                self.reconcile(op, recovery=not self.ready)
        self.ready = True


def configuration(path):
    c = private_json(path)
    require(set(c) == {'instance_id','source_project_id','project_id','maestro_url','inbox_url',
                      'maestro_credential_file','inbox_credential_file','state_file','approval_actions'}, 'invalid_configuration')
    for k in ('instance_id','source_project_id','project_id'):
        text(c[k])
    require(str(uuid.UUID(c['project_id'])) == c['project_id'], 'invalid_configuration')
    require(isinstance(c['approval_actions'], list) and len(c['approval_actions']) <= 20 and
            len(set(c['approval_actions'])) == len(c['approval_actions']) and all(isinstance(a, str) and a and len(a) <= 128 and a != 'stop_worker' and all(ch.isascii() and (ch.isalnum() or ch == '_') for ch in a) for a in c['approval_actions']), 'invalid_configuration')
    for k in ('maestro_url','inbox_url'):
        u = urllib.parse.urlsplit(c[k])
        require(u.hostname and not u.username and not u.password and not u.query and not u.fragment and u.path in ('','/'), 'invalid_configuration')
        require(u.scheme == 'https' or (k == 'maestro_url' and u.scheme == 'http' and u.hostname in ('localhost','127.0.0.1','::1')), 'https_required')
    return c


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True); parser.add_argument('--once', action='store_true')
    args = parser.parse_args(); os.umask(0o077)
    config = configuration(args.config)
    import fcntl
    state = Path(config['state_file'])
    require(state.parent.is_dir() and not state.parent.is_symlink() and stat.S_IMODE(state.parent.stat().st_mode) & 0o077 == 0, 'private_state_required')
    fd = os.open(state.with_suffix('.lock'), os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    journal = Journal(state, config)
    credentials = [private_json(config[k]) for k in ('maestro_credential_file','inbox_credential_file')]
    require(all(isinstance(c.get('token'), str) and len(c['token']) >= 32 for c in credentials), 'invalid_credential')
    require(credentials[0]['token'] != credentials[1]['token'], 'credentials_must_differ')
    runner = Runner(config, journal, Source(config, credentials[0]['token']), Inbox(config['inbox_url'], credentials[1]['token']))
    while True:
        try:
            runner.step(); print('{"event":"maestro_sync","status":"ok"}', flush=True)
        except Unavailable as e:
            print(canonical({'event':'maestro_sync','status':'unavailable','code':str(e)}), flush=True)
        if args.once:
            break
        time.sleep(5)


if __name__ == '__main__':
    try:
        main()
    except Exception:
        print('{"event":"maestro_stopped","status":"configuration_or_storage_error"}', flush=True)
        raise SystemExit(1) from None
