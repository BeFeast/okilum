"""Fixed #1414 evidence contract. Offline validation never grants native authority."""
import hashlib
import json
from pathlib import Path
import re
from urllib.parse import parse_qs, unquote, urlsplit

SCENARIO = 'external-file-identity-v1'
STEPS = ('first-copy', 'write-failure', 'reopen', 'valid-id', 'legacy', 'mismatch')
UNKNOWN = {'name': 'QA Ελληνικά', 'future': {'revision': 23, 'flags': [True, None, 'שלום']}}
INITIAL = ('{ "future" : {"revision":23,"flags":[true,null,"שלום"]},\r\n'
           '  "name" : "QA Ελληνικά" }\r\n').encode('utf-8')


def digest(data):
    return hashlib.sha256(data).hexdigest()


def blob(root, record):
    relative = Path(record['path'])
    if relative.is_absolute() or '..' in relative.parts or '\\' in str(relative) or ':' in str(relative):
        raise ValueError('Unsafe evidence path')
    candidate = (root / relative).resolve()
    if not candidate.is_relative_to(root.resolve()):
        raise ValueError('Evidence symlink escape')
    data = candidate.read_bytes()
    if digest(data) != record['sha256']:
        raise ValueError('Evidence hash mismatch')
    return data


def metadata(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('Duplicate metadata key')
            result[key] = value
        return result
    value = json.loads(data.decode('utf-8'), object_pairs_hook=unique,
                       parse_constant=lambda value: (_ for _ in ()).throw(ValueError('Non-JSON number')))
    if not isinstance(value, dict):
        raise ValueError('Metadata object required')
    return value


def link(value, folder, note, identity=None):
    uri = urlsplit(value)
    if (uri.scheme != 'okilum' or uri.netloc != 'v' or uri.fragment
            or unquote(uri.path) != '/' + folder + '/' + note):
        raise ValueError('Wrong clipboard/deep-link target')
    query = parse_qs(uri.query, keep_blank_values=True)
    expected = {} if identity is None else {'id': [identity]}
    if query != expected:
        raise ValueError('Wrong or duplicate link identity')


def validate(root, receipt):
    """Require complete owned observations; missing native instrumentation is STOP.

    The caller must independently inspect frames and establish provenance. This
    validates evidence consistency, not whether a JSON author drove the real UI.
    """
    if receipt['scenario'] != SCENARIO or receipt['steps_order'] != list(STEPS):
        raise ValueError('Unknown/incomplete scenario')
    source = receipt['source']
    if not re.fullmatch('[a-f0-9]{40}', source):
        raise ValueError('Exact artifact source required')
    if receipt['mode'] != 'copied-build' or receipt['native_driver'] != SCENARIO:
        raise ValueError('Reviewed copied-build driver required; no release gate substitution')
    evidence = receipt['steps']
    if set(evidence) != set(STEPS):
        raise ValueError('Extra/missing scenario steps')
    for name in STEPS:
        step = evidence[name]
        if step['source'] != source or type(step['pid']) is not int or step['pid'] <= 0:
            raise ValueError('Wrong app identity')
        frame = blob(root, step['frame'])
        if not frame.startswith(b'\x89PNG\r\n\x1a\n') or step['inspected_sha256'] != digest(frame):
            raise ValueError('Exact native frame inspection required')
    first = evidence['first-copy']
    before, after = blob(root, first['before']), blob(root, first['after'])
    if before != INITIAL:
        raise ValueError('Unknown-field fixture bytes changed before First Copy')
    value = metadata(after)
    identity = value.get('id', '')
    if not isinstance(identity, str) or not re.fullmatch('[A-Za-z0-9-]{8,64}', identity):
        raise ValueError('First Copy did not persist a valid ID')
    # Preserve every preexisting JSON value. Whitespace/key order can change:
    # production uses serde_json::to_vec_pretty, not a byte-preserving editor.
    if (json.dumps({k: v for k, v in value.items() if k not in ('id', 'version')}, sort_keys=True, ensure_ascii=False)
            != json.dumps(UNKNOWN, sort_keys=True, ensure_ascii=False)
            or type(value.get('version')) is not int or value['version'] != 1):
        raise ValueError('Unknown metadata was lost or rewritten')
    link(first['clipboard'], 'IdentitySeed', 'Home.md', identity)
    failure = evidence['write-failure']
    if blob(root, failure['before']) != INITIAL or blob(root, failure['after']) != INITIAL:
        raise ValueError('Failed write overwrote metadata')
    if (set(failure['probe']) != {'read_succeeded', 'write_denied', 'replace_denied'}
            or any(v is not True for v in failure['probe'].values())):
        raise ValueError('Write-failure positive control missing')
    if failure['temporary_files_after'] != []:
        raise ValueError('Failed write left partial metadata')
    link(failure['clipboard'], 'IdentityFailure', 'Home.md')
    reopened = evidence['reopen']
    if reopened['pid'] == first['pid'] or reopened['previous_pid'] != first['pid']:
        raise ValueError('Ordinary process reopen not proven')
    if blob(root, reopened['after']) != after:
        raise ValueError('Reopen changed stored identity or metadata')
    link(reopened['clipboard'], 'IdentitySeed', 'Home.md', identity)
    for name in ('valid-id', 'legacy'):
        step = evidence[name]
        if step['opened_note'] != step['expected_note'] or step['opened_root'] != step['expected_root']:
            raise ValueError('Positive navigation control failed')
        if step['opened_note'] != 'Landing.md':
            raise ValueError('Positive control must navigate to a distinct note')
        if blob(root, step['before']) != blob(root, step['after']):
            raise ValueError('Navigation mutated metadata')
    valid = evidence['valid-id']
    if valid['link_folder'] == valid['opened_folder'] or valid['resolved_id'] != identity:
        raise ValueError('Cross-folder valid-ID control missing')
    link(valid['uri'], valid['link_folder'], 'Landing.md', identity)
    legacy = evidence['legacy']
    link(legacy['uri'], legacy['opened_folder'], 'Landing.md')
    refusal = evidence['mismatch']
    losing = refusal['losing_id']
    if losing == identity or not re.fullmatch('[A-Za-z0-9-]{8,64}', losing):
        raise ValueError('Mismatch fixture did not disagree')
    link(refusal['uri'], refusal['link_folder'], 'Landing.md', losing)
    if (refusal['navigation_before'] != refusal['navigation_after']
            or refusal['navigation_after']['note'] == 'Landing.md'
            or not refusal['visible_refusal'].strip()):
        raise ValueError('Mismatch did not refuse without navigation')
    if blob(root, refusal['before']) != blob(root, refusal['after']):
        raise ValueError('Mismatch rewrote/rebound metadata')
    return {'scenario': SCENARIO, 'status': 'EVIDENCE_CONSISTENT',
            'source': source, 'native_acceptance': 'requires reviewed driver + frame/provenance review'}
