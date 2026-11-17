"""Documentation that must name exactly what the code ships (#1943).

- docs/PROTOCOL.md lists every serialized `Backlink` field, with `?` exactly on
  the optional ones, and explains each of them.
- The commit-ci `lane` input description names exactly the lanes that
  scripts/ci/github-pr.py accepts.
"""
import ast
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


def documented_backlink_fields(protocol):
    row = next(line for line in protocol.splitlines()
               if line.startswith('| `{"id":4,"op":"backlinks"'))
    fields = re.search(r'backlinks: \[\{([^}]+)\}\]', row)[1]
    return {field.strip().rstrip('?'): field.strip().endswith('?')
            for field in fields.split(',')}


def explained_backlink_fields(protocol):
    section = protocol.split('### `backlinks` entries\n', 1)[1].split('\n## ', 1)[0]
    names = []
    for cell in re.findall(r'^\| (`[^|]*`(?:, `[^|]*`)*) \|', section, re.M):
        names += re.findall(r'`(\w+)`', cell)
    return names


def serialized_backlink_fields(source):
    record = re.search(r'pub struct Backlink \{(.*?)\n\}', source, re.S)[1]
    return {name: kind.startswith('Option<')
            for name, kind in re.findall(r'pub (\w+): ([^,\n]+),', record)}


def supported_lanes(source):
    for node in ast.parse(source).body:
        if isinstance(node, ast.Assign) and any(
                isinstance(target, ast.Name) and target.id == 'LANES'
                for target in node.targets):
            return {key.value for key in node.value.keys}
    raise AssertionError('Missing LANES in github-pr.py')


def documented_lanes(workflow):
    text = re.search(r"lane:\n\s+description: '([^']+)'", workflow)[1]
    listed = text.split('(', 1)[0]
    return set(re.findall(r'[a-z]+(?:-[a-z]+)*', listed)) - {'or'}


class ProtocolBacklinksDocTests(unittest.TestCase):
    def setUp(self):
        self.protocol = (ROOT / 'docs/PROTOCOL.md').read_text()
        self.actual = serialized_backlink_fields(
            (ROOT / 'crates/okilum-core/src/vault.rs').read_text())

    def test_serialized_type_has_the_fields_this_test_expects(self):
        # Positive control for the struct parser itself.
        self.assertEqual(self.actual, {'path': False, 'title': False, 'context': False,
                                       'link': False, 'ambiguous': False, 'property': True})

    def test_ops_row_matches_serialized_type(self):
        self.assertEqual(documented_backlink_fields(self.protocol), self.actual)

    def test_every_field_is_explained_once(self):
        explained = explained_backlink_fields(self.protocol)
        self.assertEqual(len(explained), len(set(explained)), explained)
        self.assertEqual(set(explained), set(self.actual))

    def test_previous_three_field_row_is_detected(self):
        damaged = re.sub(r'backlinks: \[\{[^}]+\}\]', 'backlinks: [{path, title, context}]',
                         self.protocol, count=1)
        self.assertNotEqual(documented_backlink_fields(damaged), self.actual)


class CommitCiLaneDescriptionTests(unittest.TestCase):
    def setUp(self):
        self.workflow = (ROOT / '.forgejo/workflows/commit-ci.yml').read_text()
        self.actual = supported_lanes((ROOT / 'scripts/ci/github-pr.py').read_text())

    def test_description_names_exactly_the_accepted_lanes(self):
        self.assertEqual(documented_lanes(self.workflow), self.actual)

    def test_previous_description_is_detected(self):
        previous = re.sub(
            r"(lane:\n\s+description: )'[^']+'",
            r"\1'linux, linux-binary, native-core, native-sync, windows-release, arch, brain, inbox, or perf-linux'",
            self.workflow, count=1)
        missing = self.actual - documented_lanes(previous)
        self.assertIn('shared', missing)
        self.assertIn('native-shell', missing)


if __name__ == '__main__':
    unittest.main()
