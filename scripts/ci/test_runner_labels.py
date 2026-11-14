"""No Okilum Forgejo job may be scheduled on baldr or idunn (Oleg 14.11).

Runner inventory (Forgejo org + repository runner APIs, read-only, 14.11):

    label           runners that take it
    heavy           baldr (org ID 1)
    docker          baldr (ID 1), sindri (ID 3)
    ubuntu-latest   baldr (ID 1), sindri (ID 3), slava (ID 12)
    fast-linux      baldr (repo ID 21)
    heavy-sprint    baldr (repo ID 20, offline)
    idunn-sprint    idunn (repo ID 19)
    bridge          baldr-okilum-bridge (repo ID 17), -extra (ID 18)
    light           muninn (ID 11), slava (ID 12)
    publish         slava (repo IDs 13, 22)
    macos           m4-laptop (ID 2)
    windows-signer  okilum-win-signer (repo ID 16)

Work that compiles or tests moves to GitHub's hosted runners through a `bridge`
waiter (scripts/ci/github-pr.py, github-linux.py). The `bridge` waiters compile
nothing. Where those two runner processes are registered is a runner setting,
not a workflow setting. No other homelab machine is a target for moved jobs,
so any label outside ALLOWED fails until it is reviewed.

`runs-on` is checked after resolving what it can evaluate to: plain labels,
lists, `{labels: ...}` objects, `${{ }}` expressions (only operands that can be
the result count; `x == 'y'` and the left side of `&&` are conditions) and
`matrix.*` references (expanded with include/exclude). Anything the guard
cannot resolve statically (`inputs.*`, `vars.*`, function calls, ...) is a
violation unless UNRESOLVED_EXCEPTIONS names it with a reason.
"""
import itertools
import re
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
FORGEJO = ROOT / '.forgejo/workflows'
GITHUB = ROOT / '.github/workflows'
WORKFLOW_GLOBS = ('*.yml', '*.yaml')

# Labels that a baldr or idunn runner serves. Never use them again.
FORBIDDEN = {'heavy', 'heavy-sprint', 'fast-linux', 'docker', 'ubuntu-latest', 'idunn-sprint'}
# Labels Okilum workflows may use. Move build or test work to GitHub through `bridge`.
ALLOWED = {'bridge', 'light', 'publish', 'macos', 'windows-signer'}
# Temporary: these two beta builders still need their Forgejo-side signing,
# packaging and publication handoff. Issue #2927 moves them to GitHub and
# removes these entries. Nothing else may be added here.
EXCEPTIONS = {
    ('linux-release.yml', 'release'): 'ubuntu-latest',
    ('windows-diagnostic.yml', 'release'): 'ubuntu-latest',
}
# Expressions the guard cannot evaluate, each reviewed: (workflow, job, expression) -> reason.
UNRESOLVED_EXCEPTIONS = {
    ('ci.yml', 'macos-github', 'vars.MACOS_BRIDGE_RUNNER'):
        'Forgejo repository variable; docs/native-ci.md allows only `bridge` '
        '(unset falls back to the literal `light`)',
}
# The same rule for GitHub workflows; these variables select GitHub-hosted images.
GITHUB_UNRESOLVED_EXCEPTIONS = {
    ('forgejo-macos-depot.yml', 'macos', 'vars.OKILUM_DEPOT_MACOS_RUNNER'):
        'GitHub repository variable; docs/ci/depot-pilot.md allows only `depot-macos-15` '
        '(unset falls back to the hosted `macos-15`)',
    ('forgejo-macos.yml', 'macos', 'vars.OKILUM_GITHUB_MACOS_RUNNER'):
        'GitHub repository variable; docs/ci/depot-pilot.md allows only `depot-macos-15` '
        '(unset falls back to the hosted `macos-15`); scripts/ci/depot_guard.py enforces `depot-`',
    ('forgejo-macos.yml', 'production', 'vars.OKILUM_GITHUB_MACOS_RUNNER'):
        'GitHub repository variable; docs/ci/depot-pilot.md allows only `depot-macos-15` '
        '(unset falls back to the hosted `macos-15`); scripts/ci/depot_guard.py enforces `depot-`',
}
HINT = ('use a GitHub-hosted lane through a `bridge` waiter '
        '(scripts/ci/github-pr.py); baldr and idunn are not Okilum runners (Oleg 14.11)')

UNRESOLVED = '${{ '


def unresolved(text):
    """Marker for a value the guard cannot evaluate; never a real label."""
    return f'{UNRESOLVED}{text} }}}}'


def is_unresolved(label):
    return label.startswith(UNRESOLVED)


def expression_of(label):
    return label[len(UNRESOLVED):-3]


# --- GitHub/Forgejo expression subset -------------------------------------

TOKEN = re.compile(r"""\s*(?:
    (?P<str>'(?:[^']|'')*')
  | (?P<num>-?\d+(?:\.\d+)?)
  | (?P<op>&&|\|\||==|!=|<=|>=|<|>|!|\(|\)|,)
  | (?P<ref>[A-Za-z_][\w-]*(?:\.[\w*-]+|\[[^\]]*\])*)
)""", re.X)
FALSY = {'', 'false', 'null', '0'}


class ExpressionError(ValueError):
    pass


def tokenize(text):
    tokens, pos = [], 0
    text = text.strip()
    while pos < len(text):
        match = TOKEN.match(text, pos)
        if not match or match.end() == pos:
            raise ExpressionError(text)
        pos = match.end()
        kind = match.lastgroup
        tokens.append((kind, match.group(kind)))
    return tokens


class Parser:
    """Returns every outcome an expression can produce in a value position.

    Outcomes: ('lit', text), ('ref', path), ('bool', text), ('unknown', text).
    `a && b` yields only b's outcomes: a is returned only when falsy, and a
    falsy runs-on is a broken workflow, not a runner choice.
    """

    def __init__(self, text):
        self.text = text
        self.tokens = tokenize(text)
        self.pos = 0

    def peek(self):
        return self.tokens[self.pos] if self.pos < len(self.tokens) else (None, None)

    def take(self, value=None):
        token = self.peek()
        if token[0] is None or (value is not None and token[1] != value):
            raise ExpressionError(self.text)
        self.pos += 1
        return token

    def parse(self):
        result = self.or_expr()
        if self.pos != len(self.tokens):
            raise ExpressionError(self.text)
        return result

    def or_expr(self):
        operands = [self.and_expr()]
        while self.peek() == ('op', '||'):
            self.take()
            operands.append(self.and_expr())
        outcomes = []
        for index, operand in enumerate(operands):
            last = index == len(operands) - 1
            # A falsy literal on the left of || is never returned.
            outcomes += [o for o in operand if last or not (o[0] == 'lit' and o[1] in FALSY)]
        return outcomes

    def and_expr(self):
        result = self.compare()
        while self.peek() == ('op', '&&'):
            self.take()
            result = self.compare()
        return result

    def compare(self):
        result = self.unary()
        while self.peek()[0] == 'op' and self.peek()[1] in {'==', '!=', '<', '<=', '>', '>='}:
            self.take()
            self.unary()
            result = [('bool', 'comparison')]
        return result

    def unary(self):
        if self.peek() == ('op', '!'):
            self.take()
            self.unary()
            return [('bool', 'negation')]
        return self.primary()

    def primary(self):
        kind, value = self.take()
        if kind == 'str':
            return [('lit', value[1:-1].replace("''", "'"))]
        if kind == 'num':
            return [('lit', value)]
        if kind == 'op' and value == '(':
            result = self.or_expr()
            self.take(')')
            return result
        if kind == 'ref':
            if self.peek() == ('op', '('):
                start = self.pos - 1
                self.take('(')
                depth = 1
                while depth:
                    token = self.take()
                    depth += {('op', '('): 1, ('op', ')'): -1}.get(token, 0)
                text = ''.join(v for _, v in self.tokens[start:self.pos])
                return [('unknown', text)]
            if value in {'true', 'false', 'null'}:
                return [('lit', value)]
            return [('ref', value)]
        raise ExpressionError(self.text)


# --- matrix ----------------------------------------------------------------

def matrix_combinations(matrix):
    """Final matrix combinations after include/exclude, or None if dynamic."""
    if matrix is None:
        return []
    if not isinstance(matrix, dict):
        return None
    include = matrix.get('include') or []
    exclude = matrix.get('exclude') or []
    if not isinstance(include, list) or not isinstance(exclude, list):
        return None
    axes = {}
    for key, values in matrix.items():
        if key in {'include', 'exclude'}:
            continue
        values = values if isinstance(values, list) else [values]
        if any(isinstance(v, str) and '${{' in v for v in values):
            return None
        axes[key] = values
    combos = [dict(zip(axes, values)) for values in itertools.product(*axes.values())] if axes else []
    for rule in exclude:
        if not isinstance(rule, dict):
            return None
        combos = [c for c in combos if not all(c.get(k, object()) == v for k, v in rule.items())]
    originals = [dict(c) for c in combos]
    added = []
    for rule in include:
        if not isinstance(rule, dict):
            return None
        matched = False
        for combo, original in zip(combos, originals):
            if all(original[k] == v for k, v in rule.items() if k in original):
                combo.update(rule)
                matched = True
        if not matched:
            added.append(dict(rule))
    return combos + added


def matrix_values(path, combos):
    """Every value of `matrix.a.b` across combinations, unresolved where missing."""
    parts = path.split('.')[1:]
    if combos is None or not parts or any('[' in p or p == '*' for p in parts):
        return {unresolved(path)}
    if not combos:
        return {unresolved(path)}
    found = set()
    for combo in combos:
        value = combo
        for part in parts:
            value = value.get(part) if isinstance(value, dict) else None
        if isinstance(value, (str, int, float)) and not isinstance(value, bool) and str(value).strip():
            value = str(value).strip()
            found.add(unresolved(path) if '${{' in value else value)
        else:
            found.add(unresolved(path))
    return found


# --- labels ----------------------------------------------------------------

def expression_values(text, combos):
    try:
        outcomes = Parser(text).parse()
    except ExpressionError:
        return {unresolved(text.strip())}
    found = set()
    for kind, value in outcomes:
        if kind == 'lit':
            found.add(value.strip())
        elif kind == 'ref' and value.startswith('matrix.'):
            found |= matrix_values(value, combos)
        else:
            found.add(unresolved(value if kind != 'bool' else text.strip()))
    return found


def string_labels(value, combos):
    parts = re.split(r'\$\{\{(.*?)\}\}', value)
    if len(parts) == 1:
        return {value.strip()}
    if len(parts) == 3 and not parts[0].strip() and not parts[2].strip():
        return expression_values(parts[1], combos)
    # Text mixed with expressions: every combination of resolved pieces.
    options = []
    for index, part in enumerate(parts):
        options.append({part} if index % 2 == 0 else expression_values(part, combos))
    found = set()
    for pieces in itertools.product(*options):
        bad = [p for p in pieces if is_unresolved(p)]
        found |= set(bad) if bad else {''.join(pieces).strip()}
    return found


def labels(runs_on, matrix=None):
    """Every label a runs-on value can resolve to; unresolved parts as `${{ ... }}` markers."""
    combos = matrix_combinations(matrix)
    if isinstance(runs_on, dict):
        found = set()
        if 'group' in runs_on:
            found |= {f'group:{g}' for g in labels(runs_on['group'], matrix)}
        if 'labels' in runs_on:
            found |= labels(runs_on['labels'], matrix)
        return found or {unresolved('runs-on object without labels')}
    values = runs_on if isinstance(runs_on, list) else [runs_on]
    found = set()
    for value in values:
        if isinstance(value, (dict, list)):
            found.add(unresolved(f'nested runs-on {value!r}'))
            continue
        found |= string_labels(str(value), combos)
    return found


def workflow_files(directory):
    return sorted({p for pattern in WORKFLOW_GLOBS for p in directory.glob(pattern)})


def job_labels(job):
    strategy = job.get('strategy')
    if strategy is None:
        matrix = None
    elif isinstance(strategy, dict):
        matrix = strategy.get('matrix')
    else:
        matrix = unresolved('dynamic strategy')  # not a dict: matrix refs stay unresolved
    return labels(job['runs-on'], matrix)


def violations(directory=FORGEJO, exceptions=EXCEPTIONS, unresolved_exceptions=UNRESOLVED_EXCEPTIONS):
    problems, used, used_unresolved = [], set(), set()
    for path in workflow_files(directory):
        workflow = yaml.safe_load(path.read_text()) or {}
        for name, job in (workflow.get('jobs') or {}).items():
            if not isinstance(job, dict) or 'runs-on' not in job:
                continue
            for label in sorted(job_labels(job)):
                if is_unresolved(label):
                    key = (path.name, name, expression_of(label))
                    if unresolved_exceptions.get(key):
                        used_unresolved.add(key)
                    else:
                        problems.append(f'{path.name} {name}: runs-on `{label}` cannot be checked '
                                        f'statically; use a literal label or add a reviewed '
                                        f'UNRESOLVED_EXCEPTIONS entry; {HINT}')
                    continue
                if exceptions.get((path.name, name)) == label:
                    used.add((path.name, name))
                    continue
                if label in FORBIDDEN:
                    problems.append(f'{path.name} {name}: `{label}` can land on baldr/idunn; {HINT}')
                elif label not in ALLOWED:
                    problems.append(f'{path.name} {name}: unreviewed runner label `{label}`; {HINT}')
    stale = sorted(set(exceptions) - used)
    problems += [f'{file} {job}: exception no longer needed; remove it' for file, job in stale]
    stale = sorted(set(unresolved_exceptions) - used_unresolved)
    problems += [f'{file} {job}: unresolved exception `{expr}` no longer needed; remove it'
                 for file, job, expr in stale]
    return problems


class RunnerLabels(unittest.TestCase):
    def test_no_okilum_job_can_land_on_baldr_or_idunn(self):
        self.assertEqual(violations(), [])

    def test_exceptions_are_only_the_two_beta_builders(self):
        self.assertEqual(set(EXCEPTIONS), {('linux-release.yml', 'release'),
                                           ('windows-diagnostic.yml', 'release')})
        self.assertNotIn('ci-interactive', ALLOWED)
        self.assertFalse(ALLOWED & FORBIDDEN)

    def test_unresolved_exceptions_carry_a_reason(self):
        for table in (UNRESOLVED_EXCEPTIONS, GITHUB_UNRESOLVED_EXCEPTIONS):
            for key, reason in table.items():
                with self.subTest(key=key):
                    self.assertEqual(len(key), 3)
                    self.assertGreater(len(reason.strip()), 20)

    def test_expressions_are_resolved_to_their_literal_labels(self):
        self.assertEqual(labels("${{ vars.MACOS_BRIDGE_RUNNER || 'light' }}"),
                         {'light', unresolved('vars.MACOS_BRIDGE_RUNNER')})
        self.assertEqual(labels("${{ github.event_name == 'pull_request' && 'light' || 'publish' }}"),
                         {'light', 'publish'})
        self.assertEqual(labels("${{ inputs.x && 'heavy' || 'light' }}"), {'heavy', 'light'})
        self.assertEqual(labels(['bridge', 'heavy']), {'bridge', 'heavy'})
        self.assertEqual(labels({'labels': ['bridge', 'light']}), {'bridge', 'light'})
        self.assertEqual(labels({'labels': 'heavy'}), {'heavy'})
        self.assertEqual(labels("${{ matrix.os }}", {'os': ['macos-15', 'windows-2022']}),
                         {'macos-15', 'windows-2022'})

    def test_matrix_include_exclude_and_scalar_forms(self):
        self.assertEqual(labels('${{ matrix.runner }}', {'runner': 'heavy'}), {'heavy'})
        self.assertEqual(labels('${{ matrix.runner }}', {'runner': ['bridge', 'heavy'],
                                                         'exclude': [{'runner': 'heavy'}]}),
                         {'bridge'})
        self.assertEqual(labels('${{ matrix.runner }}', {'include': [{'runner': 'docker'}]}),
                         {'docker'})
        self.assertEqual(labels('${{ matrix.runner }}', {'os': ['a', 'b'],
                                                         'include': [{'os': 'a', 'runner': 'light'}]}),
                         {'light', unresolved('matrix.runner')})
        self.assertEqual(labels('${{ matrix.cfg.runner }}', {'cfg': [{'runner': 'fast-linux'}]}),
                         {'fast-linux'})
        self.assertEqual(labels('${{ matrix.runner }}', '${{ fromJSON(inputs.matrix) }}'),
                         {unresolved('matrix.runner')})

    def assert_one_violation(self, workflow, name='x.yml', expected='bridge'):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, name).write_text(yaml.safe_dump(workflow))
            found = violations(Path(tmp), exceptions={}, unresolved_exceptions={})
            self.assertEqual(len(found), 1, found)
            self.assertIn(expected, found[0])
            self.assertIn('bridge', found[0])

    def test_a_reintroduced_baldr_label_fails(self):
        for label in sorted(FORBIDDEN) + ['ci-interactive', "${{ inputs.x && 'heavy' || 'light' }}"]:
            with self.subTest(label=label):
                self.assert_one_violation(
                    {'jobs': {'build': {'runs-on': label, 'steps': [{'run': 'true'}]}}})

    def test_yaml_extension_is_scanned(self):
        self.assert_one_violation({'jobs': {'build': {'runs-on': 'heavy'}}},
                                  name='x.yaml', expected='`heavy`')

    def test_matrix_and_object_bypasses_fail(self):
        cases = {
            'matrix list': ({'runner': ['bridge', 'heavy']}, '${{ matrix.runner }}', '`heavy`'),
            'matrix scalar': ({'runner': 'docker'}, '${{ matrix.runner }}', '`docker`'),
            'matrix include': ({'include': [{'runner': 'ubuntu-latest'}]},
                               '${{ matrix.runner }}', '`ubuntu-latest`'),
            'matrix include extends': ({'os': ['a'], 'include': [{'os': 'a', 'runner': 'fast-linux'}]},
                                       '${{ matrix.runner }}', '`fast-linux`'),
            'matrix exclude keeps rest': ({'runner': ['heavy', 'docker'], 'exclude': [{'runner': 'docker'}]},
                                          '${{ matrix.runner }}', '`heavy`'),
            'runs-on list of matrix': ({'runner': ['heavy']}, ['bridge', '${{ matrix.runner }}'], '`heavy`'),
            'runs-on object labels': (None, {'labels': ['bridge', 'heavy']}, '`heavy`'),
            'runs-on object matrix': ({'runner': ['docker']}, {'labels': '${{ matrix.runner }}'}, '`docker`'),
        }
        for case, (matrix, runs_on, expected) in cases.items():
            with self.subTest(case=case):
                job = {'runs-on': runs_on}
                if matrix is not None:
                    job['strategy'] = {'matrix': matrix}
                self.assert_one_violation({'jobs': {'build': job}}, expected=expected)

    def test_unresolved_expressions_fail_unless_allowlisted(self):
        for runs_on in ('${{ inputs.runner }}', '${{ vars.RUNNER }}', '${{ github.event.inputs.runner }}',
                        '${{ fromJSON(inputs.runners) }}', '${{ matrix.runner }}',
                        "${{ vars.RUNNER || 'light' }}", 'self-${{ inputs.kind }}'):
            with self.subTest(runs_on=runs_on):
                self.assert_one_violation({'jobs': {'build': {'runs-on': runs_on}}},
                                          expected='cannot be checked statically')
        with self.subTest('dynamic matrix'):
            self.assert_one_violation(
                {'jobs': {'build': {'runs-on': '${{ matrix.runner }}',
                                    'strategy': {'matrix': '${{ fromJSON(inputs.m) }}'}}}},
                expected='cannot be checked statically')
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, 'x.yml').write_text(yaml.safe_dump(
                {'jobs': {'build': {'runs-on': "${{ vars.RUNNER || 'light' }}"}}}))
            self.assertEqual(violations(Path(tmp), exceptions={}, unresolved_exceptions={
                ('x.yml', 'build', 'vars.RUNNER'): 'reviewed: variable only ever names `light`'}), [])
            self.assertEqual(len(violations(Path(tmp), exceptions={}, unresolved_exceptions={
                ('x.yml', 'build', 'vars.RUNNER'): 'reviewed: variable only ever names `light`',
                ('x.yml', 'build', 'vars.OTHER'): 'reviewed: stale entry for a removed variable'})), 1)

    def test_github_workflows_never_target_homelab_runners(self):
        used = set()
        for path in workflow_files(GITHUB):
            workflow = yaml.safe_load(path.read_text()) or {}
            for name, job in (workflow.get('jobs') or {}).items():
                if not isinstance(job, dict) or 'runs-on' not in job:
                    continue
                for label in job_labels(job):
                    with self.subTest(workflow=path.name, job=name, label=label):
                        if is_unresolved(label):
                            key = (path.name, name, expression_of(label))
                            self.assertIn(key, GITHUB_UNRESOLVED_EXCEPTIONS)
                            used.add(key)
                            continue
                        self.assertNotIn(label, FORBIDDEN | ALLOWED | {'self-hosted', 'ci-interactive'})
        self.assertEqual(used, set(GITHUB_UNRESOLVED_EXCEPTIONS))


if __name__ == '__main__':
    unittest.main()
