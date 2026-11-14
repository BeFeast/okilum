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
`matrix.*` references (expanded with include/exclude). Job-level `uses:` is
followed into local reusable workflows with the caller's `with:` values;
external callees need an EXTERNAL_CALLEE_EXCEPTIONS entry. Labels compare
case-insensitively.

The guard fails closed: a value it cannot resolve (`inputs.*`, `vars.*`,
function calls, ...) is a violation unless UNRESOLVED_EXCEPTIONS names it with
a reason, and text mixed with an expression, a `${{` it cannot parse, a
comparison used as a label, or any runner `group` is always a violation.
"""
import itertools
import re
from collections import namedtuple
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

# External reusable workflows, each reviewed: (workflow, job, uses) -> reason. None today.
EXTERNAL_CALLEE_EXCEPTIONS = {}
GITHUB_DENIED = {'self-hosted', 'ci-interactive'}

# A resolved label is a plain str; everything else is a Marker, never a label.
Marker = namedtuple('Marker', 'kind text')


def unresolved(text):
    """A value the guard cannot evaluate; allowed only through UNRESOLVED_EXCEPTIONS."""
    return Marker('unresolved', text)


def invalid(text):
    """A runs-on form the guard refuses outright; no allowlist applies."""
    return Marker('invalid', text)


def external(uses):
    """A reusable workflow outside this repository."""
    return Marker('callee', uses)


def is_unresolved(label):
    return isinstance(label, Marker) and label.kind == 'unresolved'


def render(label):
    if not isinstance(label, Marker):
        return label
    return {'unresolved': f'${{{{ {label.text} }}}}', 'invalid': label.text,
            'callee': f'uses: {label.text}'}[label.kind]


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

def expression_values(text, combos, inputs=None):
    try:
        outcomes = Parser(text).parse()
    except ExpressionError:
        return {invalid(f'unparseable runs-on expression {text.strip()!r}')}
    found = set()
    for kind, value in outcomes:
        if kind == 'lit':
            found.add(value.strip())
        elif kind == 'ref' and value.startswith('matrix.'):
            found |= matrix_values(value, combos)
        elif kind == 'ref' and value.startswith('inputs.') and inputs is not None \
                and value[len('inputs.'):] in inputs:
            found |= inputs[value[len('inputs.'):]]
        elif kind == 'bool':
            found.add(invalid(f'runs-on expression {text.strip()!r} yields a boolean'))
        else:
            found.add(unresolved(value))
    return found


WHOLE_EXPRESSION = re.compile(r'\s*\$\{\{(.*)\}\}\s*', re.S)


def string_labels(value, combos, inputs=None):
    if '${{' not in value and '}}' not in value:
        return {value.strip()}
    match = WHOLE_EXPRESSION.fullmatch(value)
    if not match or '${{' in match.group(1) or '}}' in match.group(1):
        # Literal text next to an expression, or a brace the guard cannot pair.
        return {invalid(f'runs-on {value.strip()!r} mixes text with an expression')}
    return expression_values(match.group(1), combos, inputs)


def labels(runs_on, matrix=None, inputs=None):
    """Every label a runs-on value can resolve to; anything else as a Marker."""
    combos = matrix_combinations(matrix)
    if isinstance(runs_on, dict):
        found = set()
        if set(runs_on) - {'labels'}:
            # Okilum uses no runner groups; a group can route anywhere.
            found.add(invalid(f'runs-on keys {sorted(map(str, runs_on))}: only `labels` is allowed'))
        if 'labels' in runs_on:
            found |= labels(runs_on['labels'], matrix, inputs)
        return found or {invalid('runs-on object without labels')}
    values = runs_on if isinstance(runs_on, list) else [runs_on]
    found = set()
    for value in values:
        if isinstance(value, (dict, list)):
            found.add(invalid(f'nested runs-on {value!r}'))
            continue
        found |= string_labels(str(value), combos, inputs)
    return found


def workflow_files(directory):
    return sorted({p for pattern in WORKFLOW_GLOBS for p in directory.glob(pattern)})


def load(path):
    return yaml.safe_load(path.read_text()) or {}


def triggers(workflow):
    on = workflow.get('on', workflow.get(True))
    if isinstance(on, dict):
        return set(map(str, on))
    return set(map(str, on)) if isinstance(on, list) else {str(on)}


def call_input_specs(workflow):
    on = workflow.get('on', workflow.get(True))
    call = on.get('workflow_call') if isinstance(on, dict) else None
    specs = call.get('inputs') if isinstance(call, dict) else None
    return specs if isinstance(specs, dict) else {}


def job_matrix(job):
    strategy = job.get('strategy')
    if strategy is None:
        return None
    if isinstance(strategy, dict):
        return strategy.get('matrix')
    return '${{ dynamic strategy }}'  # not a dict: matrix references stay unresolved


MAX_CALL_DEPTH = 4


def job_labels(root, path, name, job, inputs=None, seen=(), via=''):
    """[(where, label)] for every runner this job can use, following local `uses:`."""
    where = f'{via}{path.name} {name}'
    if not isinstance(job, dict):
        return []
    if 'uses' in job:
        return call_labels(root, path, job, inputs, seen + (path,), where)
    if 'runs-on' not in job:
        return []
    return [(where, label) for label in labels(job['runs-on'], job_matrix(job), inputs)]


def call_labels(root, path, job, inputs, seen, where):
    uses = str(job['uses']).strip()
    if not uses.startswith('./'):
        return [(where, external(uses))]
    target = (root / uses[2:]).resolve()
    if not target.is_relative_to(root.resolve()) or not target.is_file():
        return [(where, invalid(f'reusable workflow {uses} is not a file in this repository'))]
    if target in seen or len(seen) > MAX_CALL_DEPTH:
        return [(where, invalid(f'reusable workflow {uses} recurses or nests too deep'))]
    callee = load(target)
    combos = matrix_combinations(job_matrix(job))
    passed = {}
    for key, spec in call_input_specs(callee).items():
        if isinstance(spec, dict) and 'default' in spec:
            passed[str(key)] = string_labels(str(spec['default']), [], None)
    for key, value in (job.get('with') or {}).items():
        passed[str(key)] = string_labels(str(value), combos, inputs)
    found = []
    for name, callee_job in (callee.get('jobs') or {}).items():
        found += job_labels(root, target, name, callee_job, passed, seen, f'{where} -> ')
    return found


def workflow_labels(directory, root=None):
    """(workflow, job, where, label) for every runner a scheduled job can use."""
    root = root or directory.parent.parent
    for path in workflow_files(directory):
        workflow = load(path)
        if triggers(workflow) == {'workflow_call'}:
            continue  # checked through each caller, with the caller's inputs
        for name, job in (workflow.get('jobs') or {}).items():
            for where, label in job_labels(root, path, name, job):
                yield path.name, str(name), where, label


def violations(directory=FORGEJO, exceptions=EXCEPTIONS, unresolved_exceptions=UNRESOLVED_EXCEPTIONS,
               callee_exceptions=EXTERNAL_CALLEE_EXCEPTIONS, root=None, github=False):
    """Forgejo: every label must be ALLOWED. GitHub (`github=True`): no homelab label."""
    forbidden = {label.casefold() for label in FORBIDDEN}
    allowed = {label.casefold() for label in ALLOWED}
    problems, used, used_unresolved, used_callees = [], set(), set(), set()
    for file, name, where, label in sorted(workflow_labels(directory, root), key=str):
        if isinstance(label, Marker):
            table, seen = {'unresolved': (unresolved_exceptions, used_unresolved),
                           'callee': (callee_exceptions, used_callees)}.get(label.kind, ({}, set()))
            key = (file, name, label.text)
            if label.kind == 'invalid':
                problems.append(f'{where}: {label.text}; use literal runner labels; {HINT}')
            elif str(table.get(key, '')).strip():
                seen.add(key)
            else:
                problems.append(f'{where}: runs-on `{render(label)}` cannot be checked statically; '
                                f'use a literal label (or a reviewed exception entry); {HINT}')
            continue
        folded = label.casefold()
        if str(exceptions.get((file, name), '')).casefold() == folded:
            used.add((file, name))
        elif github:
            if folded in forbidden | allowed | {d.casefold() for d in GITHUB_DENIED}:
                problems.append(f'{where}: GitHub job targets homelab runner label `{label}`; {HINT}')
        elif folded in forbidden:
            problems.append(f'{where}: `{label}` can land on baldr/idunn; {HINT}')
        elif folded not in allowed:
            problems.append(f'{where}: unreviewed runner label `{label}`; {HINT}')
    problems += [f'{file} {job}: exception no longer needed; remove it'
                 for file, job in sorted(set(exceptions) - used)]
    problems += [f'{file} {job}: exception `{text}` no longer needed; remove it'
                 for file, job, text in sorted((set(unresolved_exceptions) - used_unresolved)
                                               | (set(callee_exceptions) - used_callees))]
    return problems


def github_violations(directory=GITHUB, unresolved_exceptions=GITHUB_UNRESOLVED_EXCEPTIONS, root=None):
    return violations(directory, exceptions={}, unresolved_exceptions=unresolved_exceptions,
                      callee_exceptions={}, root=root, github=True)


class RunnerLabels(unittest.TestCase):
    def test_no_okilum_job_can_land_on_baldr_or_idunn(self):
        self.assertEqual(violations(), [])

    def test_github_workflows_never_target_homelab_runners(self):
        self.assertEqual(github_violations(), [])

    def test_exceptions_are_only_the_two_beta_builders(self):
        self.assertEqual(set(EXCEPTIONS), {('linux-release.yml', 'release'),
                                           ('windows-diagnostic.yml', 'release')})
        self.assertEqual(EXTERNAL_CALLEE_EXCEPTIONS, {})
        self.assertNotIn('ci-interactive', ALLOWED)
        self.assertFalse(ALLOWED & FORBIDDEN)

    def test_unresolved_exceptions_carry_a_reason(self):
        for table in (UNRESOLVED_EXCEPTIONS, GITHUB_UNRESOLVED_EXCEPTIONS, EXTERNAL_CALLEE_EXCEPTIONS):
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
        self.assertEqual(labels("${{\n  github.event_name == 'push'\n  && 'heavy' || 'light'\n}}"),
                         {'heavy', 'light'})
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

    def check(self, files, expected, count=1, github=False, **tables):
        """Write {repo-relative path: workflow} into a fresh repo and run the guard."""
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for relative, workflow in files.items():
                target = root / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(workflow if isinstance(workflow, str) else yaml.safe_dump(workflow))
            directory = root / ('.github/workflows' if github else '.forgejo/workflows')
            directory.mkdir(parents=True, exist_ok=True)
            if github:
                found = github_violations(directory, tables.get('unresolved_exceptions', {}), root)
            else:
                found = violations(directory, exceptions={}, root=root,
                                   unresolved_exceptions=tables.get('unresolved_exceptions', {}),
                                   callee_exceptions=tables.get('callee_exceptions', {}))
            if 'no longer needed' not in expected:
                found = [p for p in found if 'no longer needed' not in p]
            self.assertEqual(len(found), count, found)
            for problem in found:
                self.assertIn(expected, problem)
                if 'no longer needed' not in expected:
                    self.assertIn('bridge', problem)
            return found

    def assert_one_violation(self, workflow, name='x.yml', expected='bridge', **tables):
        self.check({f'.forgejo/workflows/{name}': workflow}, expected, **tables)

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
                        "${{ vars.RUNNER || 'light' }}"):
            with self.subTest(runs_on=runs_on):
                self.assert_one_violation({'jobs': {'build': {'runs-on': runs_on}}},
                                          expected='cannot be checked statically')
        with self.subTest('dynamic matrix'):
            self.assert_one_violation(
                {'jobs': {'build': {'runs-on': '${{ matrix.runner }}',
                                    'strategy': {'matrix': '${{ fromJSON(inputs.m) }}'}}}},
                expected='cannot be checked statically')
        reason = 'reviewed: variable only ever names `light`'
        workflow = {'jobs': {'build': {'runs-on': "${{ vars.RUNNER || 'light' }}"}}}
        self.assert_one_violation(workflow, count=0,
                                  unresolved_exceptions={('x.yml', 'build', 'vars.RUNNER'): reason})
        self.assert_one_violation(workflow, expected='no longer needed', unresolved_exceptions={
            ('x.yml', 'build', 'vars.RUNNER'): reason,
            ('x.yml', 'build', 'vars.OTHER'): 'reviewed: stale entry for a removed variable'})

    # Re-review of 787218c4: each case below passed that guard.
    ALLOW_VAR = {'unresolved_exceptions': {('x.yml', 'build', 'vars.RUNNER'): 'reviewed: names `bridge` only'}}

    def test_allowlisted_expression_does_not_hide_surrounding_text(self):
        for runs_on in ("${{ '' && vars.RUNNER }}heavy", 'heavy${{ vars.RUNNER }}',
                        "${{ vars.RUNNER }}\nheavy", "${{ vars.RUNNER }} ${{ 'heavy' }}"):
            with self.subTest(runs_on=runs_on):
                self.assert_one_violation({'jobs': {'build': {'runs-on': runs_on}}},
                                          expected='mixes text with an expression', **self.ALLOW_VAR)

    def test_reusable_workflows_are_followed(self):
        callee = {'on': {'workflow_call': {'inputs': {'runner': {'type': 'string', 'default': 'bridge'}}}},
                  'jobs': {'run': {'runs-on': '${{ inputs.runner }}'}}}
        call = lambda uses, **extra: {'.forgejo/workflows/x.yml': {
            'on': 'push', 'jobs': {'build': {'uses': uses, **extra}}}}
        with self.subTest('local callee outside the scanned directory'):
            self.check({**call('./.github/workflows/c.yml'),
                        '.github/workflows/c.yml': {'jobs': {'run': {'runs-on': 'heavy'}}}}, '`heavy`')
        with self.subTest('caller passes a baldr label'):
            self.check({**call('./.forgejo/workflows/c.yml', **{'with': {'runner': 'heavy'}}),
                        '.forgejo/workflows/c.yml': callee}, '`heavy`')
        with self.subTest('caller passes a matrix of labels'):
            self.check({**call('./.forgejo/workflows/c.yml', strategy={'matrix': {'r': ['bridge', 'docker']}},
                               **{'with': {'runner': '${{ matrix.r }}'}}),
                        '.forgejo/workflows/c.yml': callee}, '`docker`')
        with self.subTest('callee default applies'):
            self.check({**call('./.forgejo/workflows/c.yml'), '.forgejo/workflows/c.yml': callee}, '', count=0)
        with self.subTest('missing callee'):
            self.check(call('./.forgejo/workflows/missing.yml'), 'not a file in this repository')
        with self.subTest('callee calls itself'):
            self.check({**call('./.forgejo/workflows/c.yml'), '.forgejo/workflows/c.yml': {
                'on': 'workflow_call', 'jobs': {'again': {'uses': './.forgejo/workflows/c.yml'}}}}, 'recurses')
        with self.subTest('external callee'):
            self.check(call('other/repo/.forgejo/workflows/build.yml@v1'),
                       'uses: other/repo/.forgejo/workflows/build.yml@v1')
        with self.subTest('external callee allowlisted'):
            self.check(call('other/repo/.forgejo/workflows/build.yml@v1'), '', count=0, callee_exceptions={
                ('x.yml', 'build', 'other/repo/.forgejo/workflows/build.yml@v1'): 'reviewed: runs on bridge'})

    def test_runner_groups_are_rejected(self):
        for github, fine in ((False, 'bridge'), (True, 'macos-15')):
            for runs_on in ({'group': 'baldr', 'labels': [fine]}, {'group': 'baldr'},
                            {'group': '${{ vars.RUNNER }}', 'labels': fine}):
                with self.subTest(github=github, runs_on=runs_on):
                    root = '.github' if github else '.forgejo'
                    self.check({f'{root}/workflows/x.yml': {'jobs': {'build': {'runs-on': runs_on}}}},
                               'only `labels` is allowed', github=github, **self.ALLOW_VAR)

    def test_multiline_expressions_are_parsed(self):
        for runs_on in ("${{ vars.RUNNER\n  || 'heavy' }}", "${{\n  vars.RUNNER ||\n  'heavy'\n}}"):
            with self.subTest(runs_on=runs_on):
                self.assert_one_violation({'jobs': {'build': {'runs-on': runs_on}}},
                                          expected='can land on baldr/idunn', **self.ALLOW_VAR)
        for runs_on in ("${{ 'light'", "light }}", "${{ vars.RUNNER ${{ 'x' }} }}"):
            with self.subTest(runs_on=runs_on):
                self.assert_one_violation({'jobs': {'build': {'runs-on': runs_on}}},
                                          expected='mixes text with an expression', **self.ALLOW_VAR)

    def test_labels_compare_case_insensitively(self):
        for label in ('Heavy', 'UBUNTU-LATEST', 'Docker'):
            with self.subTest(label=label):
                self.assert_one_violation({'jobs': {'build': {'runs-on': label}}},
                                          expected='can land on baldr/idunn')
            for denied in (label, 'Bridge', 'Self-Hosted'):
                with self.subTest(github=True, label=denied):
                    self.check({'.github/workflows/x.yml': {'jobs': {'build': {'runs-on': denied}}}},
                               'homelab runner label', github=True)
        self.assert_one_violation({'jobs': {'build': {'runs-on': 'Bridge'}}}, count=0)


if __name__ == '__main__':
    unittest.main()
