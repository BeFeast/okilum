import copy
import json
from pathlib import Path
import tempfile
import unittest
from external_identity import INITIAL, UNKNOWN, STEPS, SCENARIO, digest, validate, blob


class IdentityEvidence(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.source = 'a' * 40
        self.identity = 'identity-1234'
        self.after = dict(UNKNOWN, version=1, id=self.identity)
        self.counter = 0
        self.receipt = self.fixture()

    def save(self, data):
        self.counter += 1
        name = f'blob-{self.counter}'
        (self.root / name).write_bytes(data)
        return {'path': name, 'sha256': digest(data)}

    def encoded(self, value):
        return self.save(json.dumps(value, ensure_ascii=False).encode())

    def fixture(self):
        steps = {name: {'source': self.source, 'pid': 42} for name in STEPS}
        for row in steps.values():
            frame = b'\x89PNG\r\n\x1a\nMOCK_UNIT_FIXTURE_NOT_NATIVE'
            row.update(frame=self.save(frame), inspected_sha256=digest(frame))
        first = steps['first-copy']
        first.update(before=self.save(INITIAL), after=self.encoded(self.after),
                     clipboard=f'okilum://v/IdentitySeed/Home.md?id={self.identity}')
        steps['write-failure'].update(before=self.save(INITIAL), after=self.save(INITIAL),
            probe={'read_succeeded': True, 'write_denied': True, 'replace_denied': True},
            temporary_files_after=[], clipboard='okilum://v/IdentityFailure/Home.md')
        steps['reopen'].update(pid=43, previous_pid=42, after=first['after'], clipboard=first['clipboard'])
        for name in ('valid-id', 'legacy'):
            steps[name].update(before=first['after'], after=first['after'],
                opened_note='Landing.md', expected_note='Landing.md',
                opened_root='OWNED/IdentityRenamed', expected_root='OWNED/IdentityRenamed',
                opened_folder='IdentityRenamed')
        steps['valid-id'].update(link_folder='IdentitySeed', resolved_id=self.identity,
            uri=f'okilum://v/IdentitySeed/Landing.md?id={self.identity}')
        steps['legacy'].update(uri='okilum://v/IdentityRenamed/Landing.md')
        steps['mismatch'].update(before=first['after'], after=first['after'],
            navigation_before={'root': 'OWNED/IdentityRenamed', 'note': 'Home.md'},
            navigation_after={'root': 'OWNED/IdentityRenamed', 'note': 'Home.md'},
            visible_refusal='Vault identity mismatch', losing_id='losing-5678', link_folder='IdentitySeed',
            uri='okilum://v/IdentitySeed/Landing.md?id=losing-5678')
        return dict(scenario=SCENARIO, native_driver=SCENARIO, source=self.source,
                    mode='copied-build', steps_order=list(STEPS), steps=steps)

    def check(self):
        return validate(self.root, self.receipt)

    def test_positive_control_is_consistency_only(self):
        self.assertEqual(self.check()['status'], 'EVIDENCE_CONSISTENT')
        self.assertNotEqual(self.check()['native_acceptance'], 'PASS')

    def test_unknown_field_mutation_detected(self):
        changed = dict(self.after, future={'revision': 99})
        self.receipt['steps']['first-copy']['after'] = self.encoded(changed)
        with self.assertRaises(ValueError): self.check()

    def test_unknown_json_types_cannot_be_coerced(self):
        changed = copy.deepcopy(self.after)
        changed['future']['flags'][0] = 1
        self.receipt['steps']['first-copy']['after'] = self.encoded(changed)
        with self.assertRaises(ValueError): self.check()

    def test_failed_write_bytes_cannot_change(self):
        self.receipt['steps']['write-failure']['after'] = self.save(INITIAL + b' ')
        with self.assertRaises(ValueError): self.check()

    def test_failed_write_requires_positive_controls(self):
        for key in ('read_succeeded', 'write_denied', 'replace_denied'):
            row = self.receipt['steps']['write-failure']
            row['probe'][key] = False
            with self.assertRaises(ValueError): self.check()
            row['probe'][key] = True

    def test_failed_write_has_no_temporary_overwrite(self):
        self.receipt['steps']['write-failure']['temporary_files_after'] = ['.vault.json.tmp']
        with self.assertRaises(ValueError): self.check()

    def test_reopen_requires_new_process_same_id(self):
        row = self.receipt['steps']['reopen']
        row['pid'] = row['previous_pid']
        with self.assertRaises(ValueError): self.check()
        row['pid'] = 43
        row['clipboard'] = 'okilum://v/IdentitySeed/Home.md?id=another-1234'
        with self.assertRaises(ValueError): self.check()

    def test_mismatch_navigation_mutation_is_detected(self):
        self.receipt['steps']['mismatch']['navigation_after'] = {'note': 'Landing.md'}
        with self.assertRaises(ValueError): self.check()

    def test_mismatch_metadata_overwrite_is_detected(self):
        self.receipt['steps']['mismatch']['after'] = self.save(INITIAL)
        with self.assertRaises(ValueError): self.check()

    def test_valid_and_legacy_controls_are_required(self):
        for name in ('valid-id', 'legacy'):
            row = self.receipt['steps'][name]
            row['opened_note'] = 'Home.md'
            with self.assertRaises(ValueError): self.check()
            row['opened_note'] = 'Landing.md'

    def test_valid_id_requires_different_folder(self):
        self.receipt['steps']['valid-id']['opened_folder'] = 'IdentitySeed'
        with self.assertRaises(ValueError): self.check()

    def test_exact_frame_and_head_guards(self):
        row = self.receipt['steps']['first-copy']
        row['source'] = 'b' * 40
        with self.assertRaises(ValueError): self.check()
        row['source'] = self.source
        row['inspected_sha256'] = '0' * 64
        with self.assertRaises(ValueError): self.check()

    def test_missing_steps_cannot_pass(self):
        self.receipt['steps'].pop('legacy')
        with self.assertRaises(ValueError): self.check()

    def test_duplicate_ids_in_link_are_rejected(self):
        self.receipt['steps']['first-copy']['clipboard'] += '&id=losing-5678'
        with self.assertRaises(ValueError): self.check()

    def test_duplicate_metadata_keys_are_rejected(self):
        self.receipt['steps']['first-copy']['after'] = self.save(b'{"id":"identity-1234","id":"identity-1234"}')
        with self.assertRaises(ValueError): self.check()

    def test_evidence_bytes_and_paths_are_checked(self):
        row = self.receipt['steps']['first-copy']['frame']
        row['sha256'] = '0' * 64
        with self.assertRaises(ValueError): self.check()
        for path in ('../foreign', '/etc/passwd', 'C:\\foreign', 'a\\..\\foreign'):
            with self.assertRaises(ValueError): blob(self.root, {'path': path, 'sha256': '0' * 64})

    def test_seed_updater_mode_cannot_masquerade_as_functional(self):
        self.receipt['mode'] = 'pr-seed-real-beta'
        with self.assertRaises(ValueError): self.check()


if __name__ == '__main__':
    unittest.main()
