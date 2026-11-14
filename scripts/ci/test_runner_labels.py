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
"""
import re
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
FORGEJO = ROOT / '.forgejo/workflows'
GITHUB = ROOT / '.github/workflows'

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
HINT = ('use a GitHub-hosted lane through a `bridge` waiter '
        '(scripts/ci/github-pr.py); baldr and idunn are not Okilum runners (Oleg 14.11)')


def labels(runs_on):
    """Every label a runs-on value can resolve to, including expression literals."""
    values = runs_on if isinstance(runs_on, list) else [runs_on]
    found = set()
    for value in values:
        value = str(value)
        if '${{' not in value:
            found.add(value.strip())
            continue
        # Literals compared with ==/!= are conditions, not labels.
        expression = re.sub(r"[!=]=\s*'[^']*'", '', value)
        found.update(re.findall(r"'([^']*)'", expression))
        # A repository variable must name an ALLOWED label (MACOS_BRIDGE_RUNNER=bridge).
        found.update('vars.' + name for name in re.findall(r'vars\.(\w+)', expression))
    return found


def violations(directory=FORGEJO, exceptions=EXCEPTIONS):
    problems, used = [], set()
    for path in sorted(directory.glob('*.yml')):
        workflow = yaml.safe_load(path.read_text()) or {}
        for name, job in (workflow.get('jobs') or {}).items():
            if 'runs-on' not in job:
                continue
            for label in labels(job['runs-on']):
                if label.startswith('vars.'):
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
    return problems


class RunnerLabels(unittest.TestCase):
    def test_no_okilum_job_can_land_on_baldr_or_idunn(self):
        self.assertEqual(violations(), [])

    def test_exceptions_are_only_the_two_beta_builders(self):
        self.assertEqual(set(EXCEPTIONS), {('linux-release.yml', 'release'),
                                           ('windows-diagnostic.yml', 'release')})
        self.assertNotIn('ci-interactive', ALLOWED)
        self.assertFalse(ALLOWED & FORBIDDEN)

    def test_expressions_are_resolved_to_their_literal_labels(self):
        self.assertEqual(labels("${{ vars.MACOS_BRIDGE_RUNNER || 'light' }}"),
                         {'light', 'vars.MACOS_BRIDGE_RUNNER'})
        self.assertEqual(labels("${{ github.event_name == 'pull_request' && 'light' || 'publish' }}"),
                         {'light', 'publish'})
        self.assertEqual(labels(['bridge', 'heavy']), {'bridge', 'heavy'})

    def test_a_reintroduced_baldr_label_fails(self):
        import tempfile
        for label in sorted(FORBIDDEN) + ['ci-interactive', "${{ inputs.x && 'heavy' || 'light' }}"]:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as tmp:
                Path(tmp, 'x.yml').write_text(yaml.safe_dump(
                    {'jobs': {'build': {'runs-on': label, 'steps': [{'run': 'true'}]}}}))
                found = violations(Path(tmp), exceptions={})
                self.assertEqual(len(found), 1, found)
                self.assertIn('bridge', found[0])

    def test_github_workflows_never_target_homelab_runners(self):
        for path in sorted(GITHUB.glob('*.yml')):
            workflow = yaml.safe_load(path.read_text()) or {}
            for name, job in (workflow.get('jobs') or {}).items():
                for label in labels(job.get('runs-on', '')):
                    with self.subTest(workflow=path.name, job=name):
                        self.assertNotIn(label, FORBIDDEN | ALLOWED | {'self-hosted', 'ci-interactive'})


if __name__ == '__main__':
    unittest.main()
