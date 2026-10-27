"""Transient late writes must not mask real native or cleanup failures."""
import errno
from copy import deepcopy
import importlib.util
from pathlib import Path
import unittest
import shutil
import tempfile
from unittest.mock import Mock

spec = importlib.util.spec_from_file_location(
    "sparkle_fixture", Path(__file__).parent / "sparkle-freshness/run.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class SparkleRotationEvidenceTests(unittest.TestCase):
    def setUp(self):
        def snapshot(role, boundary):
            return dict(role=role, boundary=boundary, state='complete',
                        updaterPresent=True, hostIvarPresent=True, hostPresent=True,
                        defaultsIvarPresent=True, defaultsPresent=True, defaultsIsNSUserDefaults=True,
                        defaultsClass='NSUserDefaults', domain='isolated.test.updates',
                        groupType='NSNumber', group=6)
        self.receipt = dict(snapshots=[snapshot('probe', 'completion'), snapshot('source', 'rejection')],
                            process_exit=None, defaults_path='explicit-suite',
                            error_domain='OkilumUpdate', error_code=3, selected='2',
                            foreground=False, feed_hashes=['a' * 64, 'a' * 64], initial_feed_hash='a' * 64,
                            final_feed_hash='a' * 64, installed='1', relaunch_receipt=None,
                            guard_seen=False, termination_seen=False, admitted_download=True)

    def test_exact_original_refusal_is_reproduced(self):
        self.assertEqual(fixture.classify_rotation(self.receipt), 'reproduced')

    def native_trace(self, presence='true', missing_probe=False):
        # Literal native JSON, including the old plain-C NSNumber int boxing.
        template = '''{"role":"ROLE","boundary":"BOUNDARY","state":"complete",
"updaterPresent":PRESENCE,"hostIvarPresent":PRESENCE,"hostPresent":PRESENCE,
"defaultsIvarPresent":PRESENCE,"defaultsPresent":PRESENCE,"defaultsIsNSUserDefaults":true,
"defaultsClass":"NSUserDefaults","domain":"test.app.updates","groupType":"NSNumber","group":6}'''
        source = template.replace('ROLE', 'source').replace('BOUNDARY', 'rejection').replace('PRESENCE', presence)
        probe = template.replace('ROLE', 'probe').replace('BOUNDARY', 'completion').replace('PRESENCE', presence)
        if missing_probe:
            probe = (probe.replace('"state":"complete"', '"state":"defaults-missing"')
                     .replace('"defaultsPresent":true', '"defaultsPresent":false')
                     .replace('"defaultsIsNSUserDefaults":true', '"defaultsIsNSUserDefaults":false')
                     .replace('"defaultsClass":"NSUserDefaults"', '"defaultsClass":"unavailable"')
                     .replace('"groupType":"NSNumber","group":6', '"groupType":"unavailable","group":"unavailable"'))
        return ('rotation observation ' + probe.replace('\n', '') + '\nrotation observation '
                + source.replace('\n', '')
                + '\nrotation rejection error=OkilumUpdate/3 selected=2 source-group=6 probe-group=6\n')

    def test_native_json_booleans_and_exact_refusal_reproduce(self):
        receipt = {**self.receipt, **fixture.parse_rotation_trace(self.native_trace())}
        self.assertEqual(fixture.classify_rotation(receipt), 'reproduced')

    def test_native_integer_presence_flags_are_not_booleans(self):
        receipt = {**self.receipt, **fixture.parse_rotation_trace(self.native_trace(presence='1'))}
        self.assertEqual(fixture.classify_rotation(receipt), 'observer-incomplete')

    def test_standard_main_native_trace_cannot_use_cf_readback(self):
        trace = self.native_trace(missing_probe=True) + 'rollout probe group read=6 domain=test.app matches-source before-updater-start\n'
        receipt = {**self.receipt, **fixture.parse_rotation_trace(trace), 'defaults_path': 'standard-main'}
        self.assertEqual(fixture.classify_rotation(receipt), 'observer-incomplete')

    def test_standard_main_never_counts_as_reproduction(self):
        receipt = {**self.receipt, **fixture.parse_rotation_trace(self.native_trace()), 'defaults_path': 'standard-main'}
        self.assertEqual(fixture.classify_rotation(receipt), 'not-reproduced-observer-complete')

    def test_malformed_native_observation_is_incomplete(self):
        receipt = {**self.receipt, **fixture.parse_rotation_trace(self.native_trace() + 'rotation observation {bad-json}\n')}
        self.assertEqual(fixture.classify_rotation(receipt), 'observer-incomplete')

    def test_missing_nil_and_wrong_type_snapshots_are_incomplete(self):
        for changes in [None, {'state': 'defaults-missing', 'defaultsPresent': False},
                        {'groupType': 'NSString', 'group': '6'}, {'group': None},
                        {'group': 'unavailable'}, {'group': True}, {'hostIvarPresent': False}]:
            for role_index in (0, 1):
                receipt = deepcopy(self.receipt)
                if changes is None:
                    receipt['snapshots'][role_index] = None
                else:
                    receipt['snapshots'][role_index].update(changes)
                with self.subTest(changes=changes, role=role_index):
                    self.assertEqual(fixture.classify_rotation(receipt), 'observer-incomplete')

    def test_storage_readback_cannot_replace_probe_observation(self):
        receipt = deepcopy(self.receipt)
        receipt['snapshots'] = [receipt['snapshots'][1]]
        receipt['cf_probe_group'] = 6
        self.assertEqual(fixture.classify_rotation(receipt), 'observer-incomplete')

    def test_random_probe_group_is_not_reproduction_or_normalized_to_six(self):
        for group in (0, 1, 5, 13, 4294967294):
            receipt = deepcopy(self.receipt)
            receipt['snapshots'][0]['group'] = group
            with self.subTest(group=group):
                self.assertEqual(fixture.classify_rotation(receipt), 'not-reproduced-observer-complete')

    def test_complete_snapshots_alone_cannot_prove_refusal(self):
        for key, value in [('error_domain', 'SUSparkleErrorDomain'), ('error_code', 5),
                           ('selected', '4'), ('foreground', True), ('feed_hashes', ['a' * 64]),
                           ('final_feed_hash', 'b' * 64), ('installed', '4'), ('relaunch_receipt', '4'),
                           ('guard_seen', True), ('termination_seen', True), ('admitted_download', False),
                           ('process_exit', 56)]:
            receipt = deepcopy(self.receipt)
            receipt[key] = value
            with self.subTest(key=key):
                self.assertEqual(fixture.classify_rotation(receipt), 'not-reproduced-observer-complete')

    def test_incomplete_history_or_deferred_exit_cannot_look_successful(self):
        receipt = deepcopy(self.receipt)
        receipt['snapshots'].insert(0, dict(state='defaults-missing'))
        self.assertEqual(fixture.classify_rotation(receipt), 'observer-incomplete')
        receipt = deepcopy(self.receipt)
        receipt['process_exit'] = 62
        self.assertEqual(fixture.classify_rotation(receipt), 'observer-incomplete')


class SparkleRetainedAdmissionReceiptTests(unittest.TestCase):
    def setUp(self):
        def source(boundary, group):
            return dict(role='source', boundary=boundary, state='complete', groupType='NSNumber', group=group,
                        updaterPresent=True, hostIvarPresent=True, hostPresent=True, defaultsIvarPresent=True,
                        defaultsPresent=True, defaultsIsNSUserDefaults=True)
        self.receipt = dict(admitted_download=True, collection_mutations_rejected=True, foreground=False,
                            probe_selections=['2', '2'], retention_reached_ready=True,
                            installed='4', relaunch_receipt='4',
                            guard_seen=True, termination_seen=True, error_domain=None, process_exit=0,
                            feed_hashes=['a' * 64, 'a' * 64], initial_feed_hash='a' * 64,
                            final_feed_hash='a' * 64, defaults_path='explicit-suite',
                            snapshots=[source('admission', 0), source('download', 6), source('completion', 6),
                                       dict(source('completion', 6), role='probe')])

    def test_actual_install_with_guard_and_unchanged_feed_passes(self):
        fixture.assert_rotation_preserved(self.receipt)

    def test_original_refusal_receipt_does_not_pass(self):
        original = dict(self.receipt, installed='1', relaunch_receipt=None,
                        guard_seen=False, termination_seen=False, error_domain='OkilumUpdate')
        with self.assertRaises(AssertionError):
            fixture.assert_rotation_preserved(original)

    def test_no_install_guard_feed_or_observation_cannot_pass(self):
        for key, value in [('admitted_download', False), ('installed', '1'), ('relaunch_receipt', None),
                           ('guard_seen', False), ('termination_seen', False), ('scenario_error', 'failure'),
                           ('observer_exit_error', 'failure'), ('process_exit', 62),
                           ('final_feed_hash', 'b' * 64), ('snapshots', []),
                           ('collection_mutations_rejected', False), ('probe_selections', ['4']),
                           ('retention_reached_ready', False)]:
            with self.subTest(key=key), self.assertRaises(AssertionError):
                fixture.assert_rotation_preserved(dict(self.receipt, **{key: value}))

    def test_standard_main_installs_but_keeps_missing_probe_fact(self):
        receipt = deepcopy(self.receipt)
        receipt['defaults_path'] = 'standard-main'
        receipt['snapshots'][-1].update(state='defaults-missing', groupType='unavailable',
                                       group='unavailable', defaultsPresent=False)
        fixture.assert_rotation_preserved(receipt)
        receipt['snapshots'][-1]['group'] = 6
        with self.assertRaises(AssertionError):
            fixture.assert_rotation_preserved(receipt)

    def test_suite_cannot_replace_observed_group_by_storage_readback(self):
        for group in (None, 'unavailable', 0, 5, True):
            receipt = deepcopy(self.receipt)
            receipt['snapshots'][-1]['group'] = group
            receipt['cf_probe_group'] = 6
            with self.subTest(group=group), self.assertRaises(AssertionError):
                fixture.assert_rotation_preserved(receipt)


class SparkleProductionConfigReceiptTests(unittest.TestCase):
    def setUp(self):
        original = SparkleRotationEvidenceTests()
        original.setUp()
        self.receipt = deepcopy(original.receipt)
        self.receipt.update(production_signed_feed_opt_in_absent=True,
                            production_source_validation_skipped=True,
                            production_probe_validation_skipped=True,
                            production_no_admission_at_rejection=True,
                            retention_reached_ready=False)

    def test_shipped_no_opt_in_configuration_keeps_original_refusal(self):
        fixture.assert_production_config_refusal(self.receipt)

    def test_a_receipt_or_opt_in_or_install_cannot_be_reported_as_inert(self):
        for key, value in [('production_signed_feed_opt_in_absent', False),
                           ('production_source_validation_skipped', False),
                           ('production_probe_validation_skipped', False),
                           ('production_no_admission_at_rejection', False),
                           ('retention_reached_ready', True), ('installed', '4'),
                           ('selected', '4'), ('error_code', 5), ('guard_seen', True),
                           ('scenario_error', 'failed'), ('observer_exit_error', 'failed')]:
            with self.subTest(key=key), self.assertRaises(AssertionError):
                fixture.assert_production_config_refusal(dict(self.receipt, **{key: value}))

    def test_production_markers_require_real_native_receipts(self):
        callbacks = ('production-config SURequireSignedFeed absent\n'
                     'production-config source signing status=Skipped; no receipt after download\n'
                     'production-config probe signing status=Skipped\n'
                     'production-config no receipt at rejection\n')
        parsed = fixture.parse_rotation_trace(callbacks)
        for name in ('production_signed_feed_opt_in_absent', 'production_source_validation_skipped',
                     'production_probe_validation_skipped', 'production_no_admission_at_rejection'):
            self.assertIs(parsed[name], True)
            self.assertIs(fixture.parse_rotation_trace('')[name], False)


class SparkleRolloutOrderingTests(unittest.TestCase):
    def setUp(self):
        self.source = 'rollout group read=6 domain=standard-main-bundle before-updater-start'
        self.probe = 'rollout probe group read=6 domain=test.app matches-source before-updater-start'
        self.boundary = 'window probe positive control detected standard Sparkle check'
        self.cancel = 'foreground check started\nreport 7 \nordinary quit after cancellation\n'

    def check(self, callbacks):
        fixture.assert_rollout_ordering(callbacks, self.source, self.probe)

    def test_cancel_check_finishes_without_an_update_offer(self):
        self.check('\n'.join([self.source, self.probe, self.boundary, self.cancel]))

    def test_missing_or_wrong_group_is_rejected_even_after_cancellation(self):
        for source, probe in [('', self.probe), (self.source, ''),
                              (self.source.replace('read=6', 'read=0'), self.probe),
                              (self.source, self.probe.replace('read=6', 'read=0'))]:
            with self.subTest(source=source, probe=probe), self.assertRaises(AssertionError):
                self.check('\n'.join([source, probe, self.boundary, self.cancel]))

    def test_groups_after_start_boundary_are_rejected(self):
        for events in [(self.boundary, self.source, self.probe),
                       (self.source, self.boundary, self.probe)]:
            with self.subTest(events=events), self.assertRaises(AssertionError):
                self.check('\n'.join([*events, self.cancel]))

    def test_group_readbacks_without_positive_start_control_are_rejected(self):
        with self.assertRaises(AssertionError):
            self.check('\n'.join([self.source, self.probe, self.cancel]))


class SparkleCleanupTests(unittest.TestCase):
    def test_clean_case_is_removed_once(self):
        remove = Mock()
        sleep = Mock()
        fixture.remove_case("owned-case", remove=remove, clock=lambda: 0, sleep=sleep)
        remove.assert_called_once_with("owned-case")
        sleep.assert_not_called()

    def test_late_write_then_quiescence_really_removes_case(self):
        with tempfile.TemporaryDirectory() as root:
            case = Path(root) / "owned-case"
            case.mkdir()
            calls = []

            def remove(path):
                calls.append(path)
                if len(calls) == 1:
                    (path / "late-write").write_text("fixture helper")
                    raise OSError(errno.ENOTEMPTY, "late helper write")
                shutil.rmtree(path)

            sleep = Mock()
            fixture.remove_case(case, remove=remove, clock=lambda: 0, sleep=sleep)
            self.assertEqual(len(calls), 2)
            self.assertFalse(case.exists())
            sleep.assert_called_once_with(0.1)

    def test_persistent_writes_fail_at_deadline(self):
        error = OSError(errno.ENOTEMPTY, "helper still writing")
        remove = Mock(side_effect=error)
        ticks = iter([0, 0, 10])
        sleep = Mock()
        with self.assertRaises(OSError) as raised:
            fixture.remove_case("owned-case", remove=remove,
                                clock=lambda: next(ticks), sleep=sleep)
        self.assertIs(raised.exception, error)
        self.assertEqual(remove.call_count, 2)
        sleep.assert_called_once_with(0.1)

    def test_other_cleanup_errors_are_not_retried_or_ignored(self):
        for code in [errno.EACCES, errno.EIO, errno.ENOENT]:
            with self.subTest(code=code):
                error = OSError(code, "cleanup failed")
                remove = Mock(side_effect=error)
                sleep = Mock()
                with self.assertRaises(OSError) as raised:
                    fixture.remove_case("owned-case", remove=remove,
                                        clock=lambda: 0, sleep=sleep)
                self.assertIs(raised.exception, error)
                remove.assert_called_once_with("owned-case")
                sleep.assert_not_called()


if __name__ == "__main__":
    unittest.main()
