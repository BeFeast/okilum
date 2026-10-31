"""Strict bounded evidence parser. Missing, failed or ambiguous markers fail closed."""
import json
import re

MAX_LOG_BYTES = 32 * 1024 * 1024
PREFIX = re.compile(r'^(?:\d{4}-\d\d-\d\dT[0-9:.]+Z )?OKILUM_GATE_V1 (BEGIN|END) (\{.*\})$')


def executed(raw, *, sha, run, attempt, job, step):
    if not isinstance(raw, bytes) or len(raw) > MAX_LOG_BYTES:
        return False
    try:
        text = raw.decode('utf-8')
    except UnicodeDecodeError:
        return False
    if (type(run) is not int or type(attempt) is not int or run <= 0 or attempt <= 0
            or not isinstance(sha, str) or not re.fullmatch('[0-9a-f]{40}', sha)):
        return False
    identity = dict(sha=sha, run=run, attempt=attempt, job=job, step=step)
    markers = []
    for line in text.splitlines():
        match = PREFIX.fullmatch(line)
        if not match:
            continue
        try:
            value = json.loads(match[2])
        except json.JSONDecodeError:
            return False
        # A marker for another step cannot prove this step. A matching step
        # with the wrong source/run/job/attempt is conflicting evidence.
        if value.get('step') != step:
            continue
        expected = identity if match[1] == 'BEGIN' else dict(identity, exit=0)
        if (any(type(value.get(k)) is not type(v) for k, v in expected.items())
                or value != expected):
            return False
        markers.append(match[1])
    return markers == ['BEGIN', 'END']
