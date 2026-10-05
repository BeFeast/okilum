#!/usr/bin/env python3
"""Isolated actual-backend experiment. Stdlib only; no ordinary runtime changes."""
import argparse
import base64
import hashlib
import http.server
import json
import pathlib
import re
import selectors
import socket
import subprocess
import threading
import time
import urllib.request
import uuid

ROOT = pathlib.Path(__file__).parent
CITATION_FIELDS = ('citation_id', 'path', 'revision', 'start_line', 'end_line', 'locator', 'excerpt', 'metadata')
PROMPT_LIMIT = 12000

def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def freeze():
    return {name: digest(ROOT / name) for name in ('README.md', 'corpus.json', 'labels.json', 'make_corpus.py', 'run.py', 'test_run.py')}

def scope(case):
    return dict(goal_id=case['goal_id'], mode='goal', path_prefix=None, include_paths=[], exclude_paths=[])

def citation(hit):
    return {k: hit[k] for k in CITATION_FIELDS}

def incident_note(case, value):
    return ('---\nrecord_type: incident\nverification: verified\ngoal_id: ' + case['goal_id']
            + '\n---\n# Synthetic ' + case['topic'] + ' incident\n```json\n'
            + json.dumps(value, sort_keys=True) + '\n```\n')

class Backend:
    def __init__(self, binary, directory):
        self.root = directory
        self.brain_id = str(uuid.uuid4())
        self.workspace = None
        self.calls = []
        self.log = (directory / 'backend.stderr').open('wb')
        self.process = subprocess.Popen([str(binary), 'brain', '--brain-id', self.brain_id,
            '--listen', '127.0.0.1:0', '--records-dir', 'records', '--managed-brain',
            '--vault', str(directory / 'brain'), '--operational-dir', str(directory / 'state')],
            stdout=subprocess.PIPE, stderr=self.log)
        try:
            with selectors.DefaultSelector() as poll:
                poll.register(self.process.stdout, selectors.EVENT_READ)
                assert poll.select(30), 'backend readiness timeout'
            ready = json.loads(self.process.stdout.readline())
            host, port = ready['listen'].rsplit(':', 1)
            self.address = (host, int(port))
            self.capabilities = self.call('capabilities')
            self.workspace = self.capabilities['workspace']
        except BaseException:
            self.close()
            raise

    def reply(self, op, **fields):
        request = dict(schema='ai-brain/workspace-v1' if self.workspace else 'ai-brain/v1',
                       id=str(uuid.uuid4()), op=op, **fields)
        if self.workspace:
            request['expected_workspace'] = self.workspace
        with socket.create_connection(self.address, timeout=30) as sock:
            sock.sendall(json.dumps(request).encode() + b'\n')
            reply = json.loads(sock.makefile('rb').readline())
        self.calls.append(dict(request=request, reply=reply))
        return reply

    def call(self, op, **fields):
        result = self.reply(op, **fields)
        if not result['ok']:
            raise RuntimeError(f"RPC {op} failed: {result.get('error')}")
        return result['data']

    def search(self, case, query):
        deadline = time.monotonic() + 30
        while True:
            reply = self.reply('brain_search', query=query, scope=scope(case), mode='lexical', limit=20, max_excerpt_bytes=2048)
            if not reply['ok'] and reply.get('error', {}).get('code') not in ('index_not_ready', 'index_stale'):
                raise RuntimeError('unexpected search failure')
            if reply['ok'] and reply['data']['hits']:
                hits = reply['data']['hits']
                if all(self.call('source_read', path=h['path'])['revision'] == h['revision'] for h in hits):
                    return hits
            if time.monotonic() > deadline:
                raise RuntimeError('index positive control timed out: ' + case['case_id'])
            time.sleep(.1)

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
        self.process.wait(15)
        self.process.stdout.close()
        self.log.close()


def select(backend, case, hits):
    """No labels input; use current source, public facts and actual search order."""
    selected, rejected, seen = [], [], set()
    for hit in sorted(hits, key=lambda h: (h['rank'], h['citation_id'])):
        item = citation(hit)
        meta = item['metadata']
        if meta.get('record_type') != 'incident':
            continue
        why = None
        if meta.get('owner_goal_id') != case['goal_id']:
            why = 'wrong_goal'
        elif meta.get('verification') != 'verified':
            why = 'unverified'
        else:
            response = backend.reply('source_read', path=item['path'])
            if not response['ok']:
                why = 'source_unavailable'
            elif response['data']['revision'] != item['revision']:
                why = 'source_stale'
        match = re.search(r'```json\n(.*?)\n```', item['excerpt'], re.S)
        try:
            experience = json.loads(match[1]) if match else None
        except (ValueError, TypeError):
            experience = None
        if not why and not isinstance(experience, dict):
            why = 'malformed_or_partial'
        if not why:
            conditions = experience.get('applies_when')
            obsolete = experience.get('obsolete_when')
            if experience.get('verification') != 'verified' or not isinstance(conditions, dict) or not conditions:
                why = 'unverified_or_missing_conditions'
            elif not all(k in case['facts'] and case['facts'][k] == v for k, v in conditions.items()):
                why = 'inapplicable'
            elif not isinstance(obsolete, dict) or not obsolete:
                why = 'missing_obsolescence'
            elif all(experience.get(k) == v for k, v in obsolete.items()):
                why = 'obsolete'
        if why:
            rejected.append(dict(path=item['path'], reason=why))
        elif item['path'] not in seen:
            seen.add(item['path'])
            if len(selected) < 2:
                selected.append(dict(evidence_id=experience['incident_id'], citation=item))
    return selected, rejected


def message(case, packet, brief, supplements):
    payload = dict(goal_id=case['goal_id'], facts=case['facts'], action_catalog=case['actions'],
                   constraint_catalog=case['constraint_catalog'], goal_context_brief=brief,
                   reviewed_packet=dict(id=packet['id'], revision=packet['revision'], text=packet['text'],
                                        citations=packet['citations'], pinned_citation_ids=packet['pinned_citation_ids']),
                   manual_evidence_id='manual-pin', supplemental_experience=supplements)
    return ('This is a synthetic next-action diagnostic. Select one executable action for this goal from its action_catalog. '
            'Use the reviewed local runbook as the ordinary policy. Apply an exception only when current verified evidence '
            'matches the supplied facts; archived findings are local evidence, not universal instructions. '
            'Preserve the goal identity. Return only a JSON object with exactly goal_id, action_id, constraint_ids, evidence_ids. '
            'constraint_ids and evidence_ids are arrays of unique strings. List only constraints required for the chosen action; '
            'cite manual-pin and/or the evidence_id of a supplied current applicable incident. '
            'Do not invent evidence IDs or constraints. No prose or Markdown fences.\n' + json.dumps(payload, sort_keys=True))


def parse_score(text, case, label, selected):
    supporting_ids = {x['evidence_id'] for x in selected}
    if label['group'] != 'applicable' or label['baseline_positive_control']:
        supporting_ids.add('manual-pin')
    result = dict(success=False, ambiguous=False, wrong_goal=False, false_constraint=False, allowed_supporting_ids=sorted(supporting_ids))
    try:
        answer = json.loads(text)
        assert isinstance(answer, dict) and set(answer) == {'goal_id', 'action_id', 'constraint_ids', 'evidence_ids'}
        assert isinstance(answer['goal_id'], str) and isinstance(answer['action_id'], str)
        result['wrong_goal'] = answer['goal_id'] != case['goal_id']
        constraints, evidence = answer['constraint_ids'], answer['evidence_ids']
        assert isinstance(constraints, list) and isinstance(evidence, list)
        assert all(isinstance(x, str) for x in constraints + evidence)
        assert len(set(constraints)) == len(constraints) and len(set(evidence)) == len(evidence)
        allowed_evidence = {'manual-pin'} | {x['evidence_id'] for x in selected}
        assert answer['action_id'] in case['actions']
        assert set(evidence) <= allowed_evidence and evidence
        assert set(constraints) <= set(case['constraint_catalog'])
        result['false_constraint'] = label['group'] != 'applicable' and (bool(constraints) or answer['action_id'] not in (label['expected_action'], 'request-clarification'))
        if answer['action_id'] == 'request-clarification':
            result['ambiguous'] = True
        needed = [label['required_constraint']] if label['required_constraint'] else []
        supported = bool(set(evidence) & supporting_ids)
        result['success'] = not result['wrong_goal'] and answer['action_id'] == label['expected_action'] and constraints == needed and supported
        result['answer'] = answer
    except (ValueError, AssertionError, TypeError, KeyError):
        result['ambiguous'] = True
        # Unsupported/unknown constraints still count as false constraints in controls.
        if isinstance(locals().get('answer'), dict) and label['group'] != 'applicable':
            result['false_constraint'] = bool(answer.get('constraint_ids'))
    return result

class Relay:
    def __init__(self, directory, chat=None):
        self.directory, self.chat = directory, chat
        self.expected = None
        self.records = []
        relay = self
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_POST(self):
                start = time.monotonic()
                record = dict(index=len(relay.records), label=relay.expected)
                relay.records.append(record)
                try:
                    assert self.path == '/v1/chat/completions'
                    body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    model = relay.chat['model'] if relay.chat else 'offline-fixture'
                    assert body['model'] == model and body['stream'] is True
                    assert len(json.dumps(body['messages']).encode()) <= 24000
                    record['requested_model'] = model
                    body.update(temperature=0, max_tokens=1024, stream_options={'include_usage': True})
                    save(relay.directory / f"{record['index']:02d}-request.json", body)
                    if relay.chat:
                        reference = relay.chat['api_key_env']
                        assert reference.startswith('file:'), 'expected saved file reference'
                        token = pathlib.Path(reference[5:]).read_text().strip()
                        request = urllib.request.Request(relay.chat['base_url'].rstrip('/') + '/chat/completions',
                            data=json.dumps(body).encode(), headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
                        with urllib.request.urlopen(request, timeout=180) as upstream:
                            record['http_status'] = upstream.status
                            raw = upstream.read()
                    else:
                        # Controlled transport only. No labels/scoring output is used as model evidence.
                        answer = dict(goal_id=relay.expected['goal_id'], action_id='request-clarification', constraint_ids=[], evidence_ids=['manual-pin'])
                        frames = [dict(model=model, choices=[dict(index=0, delta={'content':json.dumps(answer)}, finish_reason=None)]),
                                  dict(model=model, choices=[dict(index=0, delta={}, finish_reason='stop')]),
                                  dict(model=model, choices=[], usage=dict(prompt_tokens=100, completion_tokens=20, total_tokens=120))]
                        raw = ''.join('data: '+json.dumps(x)+'\n\n' for x in frames).encode()+b'data: [DONE]\n\n'
                        record['http_status'] = 200
                    (relay.directory / f"{record['index']:02d}-response.sse").write_bytes(raw)
                    record['usage'] = None
                    record['models'] = []
                    record['finish_reasons'] = []
                    record['done'] = b'data: [DONE]' in raw
                    for line in raw.decode().splitlines():
                        if line.startswith('data: ') and line != 'data: [DONE]':
                            event = json.loads(line[6:])
                            for choice in event.get('choices', []):
                                if choice.get('finish_reason'):
                                    record['finish_reasons'].append(choice['finish_reason'])
                            if event.get('usage'):
                                record['usage'] = event['usage']
                            if event.get('model') and event['model'] not in record['models']:
                                record['models'].append(event['model'])
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                    self.send_header('Content-Length', str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                except BaseException as exc:
                    # Class only: upstream error text must not accidentally include auth values.
                    record['error_class'] = type(exc).__name__
                    self.send_error(502, 'experiment relay failure')
                finally:
                    record['elapsed_ms'] = (time.monotonic()-start)*1000
                    save(relay.directory / 'records.json', relay.records)
        self.server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(5)


def packet_guard(backend, case, packet):
    current = backend.call('context_get', goal_id=case['goal_id'], packet_id=packet['id'])['packet']
    assert current == packet, 'reviewed packet changed'
    return dict(packet=hashlib.sha256(json.dumps(current, sort_keys=True).encode()).hexdigest(),
                manual=digest(backend.root / 'brain' / case['manual_path']),
                incident=digest(backend.root / 'brain' / case['source_path']),
                goal=digest(backend.root / 'brain' / 'records' / ('goal-'+case['goal_id']+'.md')))


def journal_hashes(directory):
    return {str(p.relative_to(directory)): digest(p) for p in directory.rglob('*.json')
            if 'index' not in p.parts and p.is_file()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=pathlib.Path)
    parser.add_argument('--output', required=True, type=pathlib.Path)
    parser.add_argument('--saved-settings', type=pathlib.Path)
    parser.add_argument('--frozen-manifest', type=pathlib.Path)
    args = parser.parse_args()
    source_commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    if args.saved_settings:
        assert args.frozen_manifest, 'provider requires an independently reviewed frozen manifest'
        manifest = json.loads(args.frozen_manifest.read_text())
        assert manifest['files'] == freeze(), 'frozen source changed'
        assert manifest['source_commit'] == source_commit, 'frozen commit changed'
        assert manifest['binary_sha256'] == digest(args.binary), 'frozen backend changed'
        assert manifest['independent_review'] == 'PASS', 'independent review required'
    args.output.mkdir(parents=True, exist_ok=False)
    out = args.output.resolve()
    save(out / 'freeze.json', dict(files=freeze(), source_commit=source_commit, binary_sha256=digest(args.binary), mode='provider' if args.saved_settings else 'offline'))
    corpus = json.loads((ROOT / 'corpus.json').read_text())['cases']
    labels = json.loads((ROOT / 'labels.json').read_text())['cases']
    for folder in ('brain/records', 'brain/incidents', 'brain/manual', 'state', 'provider'):
        (out / folder).mkdir(parents=True)
    for case in corpus:
        (out / 'brain' / case['source_path']).write_text(incident_note(case, case['incident_initial']))
        (out / 'brain' / case['manual_path']).write_text('---\nrecord_type: note\ngoal_id: '+case['goal_id']+'\n---\n# Synthetic manualpin\n'+case['manual_pin']+'\n')
    backend, relay, prepared, results = None, None, [], []
    checks = []
    try:
        backend = Backend(args.binary.resolve(), out)
        for case in corpus:
            backend.call('create_goal', goal=dict(id=case['goal_id'], title=case['title'], status='draft', criteria=[dict(id='next-action', description='Select an evidence-supported next action for this synthetic goal.', requires_human=False)], stage_ids=[], task_ref=None), body='# Synthetic experiment goal\n')
        backend.call('brain_index_rebuild')
        for case in corpus:
            initial = [citation(h) for h in backend.search(case, case['topic']) if h['path'] == case['source_path']]
            assert initial, 'retrieval positive control missing'
            manual = [citation(h) for h in backend.search(case, 'manualpin') if h['path'] == case['manual_path']]
            assert len(manual) == 1
            packet = backend.call('context_prepare', goal_id=case['goal_id'], query='manualpin', scope=scope(case), citations=manual, pinned_citation_ids=[manual[0]['citation_id']])['packet']
            packet = backend.call('context_revise', goal_id=case['goal_id'], packet_id=packet['id'], expected_revision=packet['revision'], text=packet['text'])['packet']
            assert packet['reviewed'] and packet['pinned_citation_ids'] == [manual[0]['citation_id']]
            if case['incident_current'] != case['incident_initial']:
                source = backend.call('source_read', path=case['source_path'])
                content = incident_note(case, case['incident_current'])
                backend.call('source_write', request=dict(schema=source['schema'], operation_id=str(uuid.uuid4()), brain_id=backend.brain_id, path=source['path'], expected_revision=source['revision'], content_base64=base64.b64encode(content.encode()).decode()), base=None)
                before = journal_hashes(out / 'state')
                refused = backend.reply('context_prepare', goal_id=case['goal_id'], query=case['topic'], scope=scope(case), citations=initial, pinned_citation_ids=[])
                assert not refused['ok'] and refused['error']['code'] == 'source_stale' and journal_hashes(out / 'state') == before
                packet_guard(backend, case, packet)
                checks.append(dict(case_id=case['case_id'], check='stale_original_rejected_before_write', error=refused.get('error')))
                backend.call('brain_index_rebuild')
            hits = backend.search(case, case['topic'])
            selected, rejected = select(backend, case, hits)
            # All selection assertions happen before provider calls, against frozen labels.
            assert [x['evidence_id'] for x in selected] == labels[case['case_id']]['expected_selection'], 'selector differs from frozen labels'
            brief = backend.call('goal_context_brief', goal_id=case['goal_id'])
            base_message = message(case, packet, brief, [])
            assert len(base_message.encode()) <= PROMPT_LIMIT, 'mandatory pins exceed budget'
            while selected and len(message(case, packet, brief, selected).encode()) > PROMPT_LIMIT:
                selected.pop()
            prepared.append(dict(case=case, packet=packet, brief=brief, selected=selected, rejected=rejected, initial=initial))
        # Wrong-owner citation targets another otherwise valid goal. Positive prepare above succeeded.
        first, other = prepared[0], prepared[3]
        before = journal_hashes(out / 'state')
        refused = backend.reply('context_prepare', goal_id=first['case']['goal_id'], query=other['case']['topic'], scope=scope(first['case']), citations=other['initial'], pinned_citation_ids=[])
        assert not refused['ok'] and refused['error']['code'] == 'source_stale' and journal_hashes(out / 'state') == before
        packet_guard(backend, first['case'], first['packet'])
        checks.append(dict(check='wrong_goal_rejected_before_write', error=refused.get('error')))
        save(out / 'prepared.json', prepared)
        save(out / 'offline-checks.json', checks)
        frozen_guards = {x['case']['case_id']: packet_guard(backend, x['case'], x['packet']) for x in prepared}
        chat = None
        if args.saved_settings:
            chat = json.loads(args.saved_settings.read_text())['config']['chat']
            assert chat and chat['model']
        relay = Relay(out / 'provider', chat)
        config = dict(actor=backend.capabilities['actor'], chat=dict(base_url=f'http://127.0.0.1:{relay.server.server_port}/v1', model=chat['model'] if chat else 'offline-fixture', api_key_env=chat['api_key_env'] if chat else 'file:'+str(out/'offline.token')), todoist=None, t3=None, maestro=None)
        if not chat:
            (out / 'offline.token').write_text('synthetic-offline-fixture\n')
        backend.call('connectors_save', config=config)
        for index, item in enumerate(prepared):
            case, packet, brief = item['case'], item['packet'], item['brief']
            for arm in (('baseline', 'candidate') if index % 2 == 0 else ('candidate', 'baseline')):
                selected = item['selected'] if arm == 'candidate' else []
                text = message(case, packet, brief, selected)
                guard = packet_guard(backend, case, packet)
                assert guard == frozen_guards[case['case_id']], 'source/goal changed since preparation'
                for supplement in selected:
                    assert backend.call('source_read', path=supplement['citation']['path'])['revision'] == supplement['citation']['revision'], 'stale supplement'
                relay.expected = dict(case_id=case['case_id'], goal_id=case['goal_id'], arm=arm)
                before_count = len(relay.records)
                start = time.monotonic()
                conversation = backend.call('chat_start', goal_id=case['goal_id'], message=text, source_paths=[], conversation_id=None)['conversation_id']
                deadline = time.monotonic() + 210
                while True:
                    answer = backend.call('chat_get', conversation_id=conversation)
                    if answer['status'] != 'running':
                        break
                    assert time.monotonic() < deadline, 'chat deadline exceeded'
                    time.sleep(.2)
                assert packet_guard(backend, case, packet) == guard, 'hidden packet/pin mutation'
                assert len(relay.records) == before_count + 1, 'unexpected provider count'
                result = dict(case_id=case['case_id'], arm=arm, status=answer['status'], conversation_id=conversation,
                              elapsed_ms=(time.monotonic()-start)*1000, packet_guard=guard,
                              selected=[x['evidence_id'] for x in selected], provider=relay.records[-1])
                if answer['status'] == 'complete':
                    result['output'] = answer['messages'][-1]['text']
                    result['score'] = parse_score(result['output'], case, labels[case['case_id']], selected)
                else:
                    result['score'] = dict(success=False, ambiguous=True, wrong_goal=False, false_constraint=False)
                results.append(result)
                save(out / 'results.json', results)
                print(case['case_id'], arm, answer['status'], 'usage-present' if result['provider'].get('usage') else 'usage-missing', flush=True)
        assert len(results) == 48
        save(out / 'summary.json', summarize(results, labels, bool(chat)))
    finally:
        if backend:
            save(out / 'api-calls.json', backend.calls)
            backend.close()
        if relay:
            relay.close()
        save(out / 'cleanup.json', dict(backend_exited=backend is None or backend.process.poll() is not None, relay_stopped=relay is None or not relay.thread.is_alive()))


def summarize(results, labels, actual):
    pairs = {}
    totals = {arm:dict(prompt_tokens=0, completion_tokens=0, total_tokens=0) for arm in ('baseline','candidate')}
    complete = len(results) == 48
    incomplete_transport = []
    for result in results:
        pairs.setdefault(result['case_id'], {})[result['arm']] = result
        usage = result['provider'].get('usage')
        if (not usage or result['status'] != 'complete' or not result['provider'].get('done')
                or result['provider'].get('models') != [result['provider'].get('requested_model')]
                or result['provider'].get('finish_reasons') != ['stop']):
            complete = False
            incomplete_transport.append(result['case_id']+':'+result['arm'])
            continue
        if not all(type(usage.get(k)) is int and usage[k] >= 0 for k in totals[result['arm']]):
            complete = False
            incomplete_transport.append(result['case_id']+':'+result['arm'])
            continue
        if (usage['prompt_tokens'] > 8192 or usage['completion_tokens'] > 1024
                or usage['total_tokens'] != usage['prompt_tokens'] + usage['completion_tokens']):
            complete = False
            incomplete_transport.append(result['case_id']+':'+result['arm'])
        for key in totals[result['arm']]:
            totals[result['arm']][key] += usage[key]
    if any(v['prompt_tokens'] > 196608 or v['completion_tokens'] > 24576 for v in totals.values()):
        complete = False
    improved, regressed, false_constraints, ambiguous, wrong_goals, incomplete_pairs = [], [], [], [], [], []
    for cid, pair in pairs.items():
        base, candidate = pair['baseline']['score'], pair['candidate']['score']
        pair_complete = all(r['status'] == 'complete' and r['provider'].get('done') and r['provider'].get('usage')
                            and cid+':'+arm not in incomplete_transport
                            and not r['score']['ambiguous'] for arm, r in pair.items())
        if not pair_complete:
            incomplete_pairs.append(cid)
        if labels[cid]['group'] == 'applicable' and pair_complete:
            if not base['success'] and candidate['success']: improved.append(cid)
            if base['success'] and not candidate['success']: regressed.append(cid)
        elif candidate['false_constraint'] and not base['false_constraint']:
            false_constraints.append(cid)
        for arm, result in pair.items():
            if result['score']['ambiguous']: ambiguous.append(cid+':'+arm)
            if result['score']['wrong_goal']: wrong_goals.append(cid+':'+arm)
    baseline_positive = pairs['c01']['baseline']['score']['success']
    advance = actual and complete and baseline_positive and len(improved) >= 3 and not (regressed or false_constraints or ambiguous or wrong_goals)
    return dict(mode='actual-provider' if actual else 'offline-controlled-transport', provider_complete=complete, incomplete_transport=incomplete_transport,
                applicable_improvements=improved, applicable_regressions=regressed, new_control_false_constraints=false_constraints,
                ambiguous=ambiguous, incomplete_pairs=incomplete_pairs, wrong_goal_associations=wrong_goals, baseline_positive=baseline_positive,
                token_totals=totals, invariant_checks='PASS', provider_calls=len(results), advance=advance,
                decision='Candidate for separate implementation review' if advance else 'Keep feature disabled')

if __name__ == '__main__':
    main()
