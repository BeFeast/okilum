import copy
import hashlib
import json
import os
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

from analyze import (EvidenceError, correlate_markers, decode, frame_analysis, loss_check,
                     overlap_components, scheduler_segments)
from export_raw import MARKER, RAW_MODE, verify_layouts
from runner import postflight, preflight, run_case, validate_receipt
from provenance import binary_metadata, build_environment, native_environment, reference, validate_build_environment
from mac_cpu import frame_cpu
from capsule import SOURCES


def frame():
    return {'marks': False, 'sample': 0, 'cpu_draw_ms': 39.998,
            'subtree_work': {'diagnostic_qpc': {
                'version': 1, 'start_qpc_bounds': [10000, 10002], 'end_qpc_bounds': [49998, 50000],
                'before': {'marker_id': 0, 'pid': 100, 'tid': 200, 'frequency': 1000000,
                           'event_qpc_bounds': [9000, 9004]},
                'after': {'marker_id': 1, 'pid': 100, 'tid': 200, 'frequency': 1000000,
                          'event_qpc_bounds': [51000, 51004]}}}}


def markers():
    return [{'id': 0, 'pid': 100, 'tid': 200, 'qpc': 9002},
            {'id': 1, 'pid': 100, 'tid': 200, 'qpc': 51002}]


def decoded():
    switches = []
    for stamp, old, new, state in [(100, 1, 200, 1), (1000, 200, 1, 5),
                                  (2000, 1, 200, 1), (12000, 200, 1, 5),
                                  (40000, 1, 200, 1), (60000, 200, 1, 5),
                                  (65000, 1, 200, 1), (69000, 200, 1, 5)]:
        switches.append({'qpc': stamp, 'old': old, 'new': new, 'state': state, 'reason': 13, 'cpu': 0})
    return {'switches': switches,
            'ready': [{'qpc': qpc, 'tid': 200, 'cpu': 0, 'waker_pid': 300, 'waker_tid': 400}
                      for qpc in [1500, 20000, 62000]],
            'lifetime': [{'qpc': 0, 'pid': 100, 'tid': 200, 'opcode': 1},
                         {'qpc': 70000, 'pid': 100, 'tid': 200, 'opcode': 2},
                         {'qpc': 0, 'pid': 300, 'tid': 400, 'opcode': 3},
                         {'qpc': 70000, 'pid': 300, 'tid': 400, 'opcode': 4}],
            'stacks': [{'event_qpc': qpc, 'pid': 100, 'tid': 200, 'cpu': 0, 'ips': ['0xaaa']}
                       for qpc in [1000, 12000, 60000, 69000]] +
                      [{'event_qpc': qpc, 'pid': 300, 'tid': 400, 'cpu': 0, 'ips': ['0xbbb']}
                       for qpc in [1500, 20000, 62000]]}


def receipt():
    frames = [{'marks': marked, 'sample': n, 'cpu_draw_ms': 5., 'rows_built': 1,
               'scroll_y': n, 'present_ms': 35., 'text_work': {'work': {'glyph_bitmap': {'calls': 0}}}}
              for marked in [False, True] for n in range(48)]
    return {'source_sha': 'd' * 40, 'diagnostic_only': True, 'real_native_platform': True,
            'mounted': 'Reader', 'measurement': 'production-release', 'bytes': 3500000,
            'paths_keys': 414, 'initial_rasterizations': 1, 'frames': frames,
            'open_ms': 8., 'budget_violations': [], 'strict_pass': True}


class ParserControls(unittest.TestCase):
    def test_nonzero_inherited_nice_is_preserved_and_failure_receipt_survives(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inherited = {'nice': 7, 'inherited_stack_limit': [8388608, -1]}
            def fail_after_saved_defaults(inputs, out, result):
                saved = json.loads((out / 'preflight.json').read_text())
                self.assertEqual(saved['nice'], 7)
                self.assertEqual(saved['inherited_stack_limit'], inherited['inherited_stack_limit'])
                raise AssertionError('deliberate preflight failure after durable host defaults')
            with patch('runner.sys.platform', 'darwin'), patch('runner.git', return_value=b'controller'), \
                    patch('runner.inherited_process_state', return_value=inherited), \
                    patch('runner._preflight_binaries', side_effect=fail_after_saved_defaults):
                with self.assertRaisesRegex(AssertionError, 'deliberate preflight failure'):
                    preflight(root / 'inputs', root / 'failure')
            saved = json.loads((root / 'failure/preflight.json').read_text())
            self.assertEqual(saved['nice'], 7)
            self.assertIn('deliberate preflight failure', saved['preflight_error'])
            with patch('runner.sys.platform', 'darwin'), patch('runner.git', return_value=b'controller'), \
                    patch('runner.inherited_process_state', return_value=inherited), \
                    patch('runner._preflight_binaries'):
                preflight(root / 'inputs', root / 'success')
            saved = json.loads((root / 'success/preflight.json').read_text())
            self.assertEqual(saved['nice'], 7)
            self.assertIsNotNone(saved['completed_unix_ns'])

    def test_macho_default_stack_and_override_control(self):
        raw = bytearray(56)
        struct.pack_into('<8I', raw, 0, 0xfeedfacf, 0x0100000c, 0, 2, 1, 24, 0, 0)
        struct.pack_into('<IIQQ', raw, 32, 0x80000028, 24, 4096, 0)
        self.assertEqual(binary_metadata(raw)['lc_main_stacksize'], 0)
        struct.pack_into('<Q', raw, 48, 16 * 1024 * 1024)
        with self.assertRaisesRegex(AssertionError, 'default LC_MAIN stack'):
            binary_metadata(raw)

    def test_mac_thread_cpu_identity_and_clock_controls(self):
        value = {'marks': False, 'sample': 19, 'cpu_draw_ms': 20., 'thread_cpu_ms': 6.,
            'subtree_work': {'diagnostic_macos_cpu': {'clock': {
                'api': 'CLOCK_THREAD_CPUTIME_ID', 'clock_id': 16, 'pid': 100, 'native_tid': 200,
                'main_thread': True, 'resolution_ns': 1, 'pthread_stack_bytes': 8388608},
                'before_cpu_ms': 10., 'after_cpu_ms': 16., 'cpu_bracket_wall_ms': 22.,
                'cpu_bracket_margin_ms': 2.}}}
        result = frame_cpu(value, 100, 200)
        self.assertEqual(result['thread_cpu_query_delta_ms'], 6.)
        self.assertTrue(result['wall_miss'])
        self.assertNotIn('Ready', result)
        self.assertNotIn('Waiting', result)
        for key, bad in [('clock_id', 12), ('main_thread', False), ('pid', 999), ('native_tid', 999)]:
            mutant = copy.deepcopy(value)
            mutant['subtree_work']['diagnostic_macos_cpu']['clock'][key] = bad
            with self.subTest(key=key), self.assertRaises(AssertionError):
                frame_cpu(mutant, 100, 200)
        mutant = copy.deepcopy(value)
        mutant['thread_cpu_ms'] = 0.
        with self.assertRaises(AssertionError):
            frame_cpu(mutant, 100, 200)

    def test_all_release_and_target_rustflags_are_recorded_and_tuning_rejected(self):
        normal = {'CARGO_BUILD_JOBS': '4', 'CARGO_INCREMENTAL': '0', 'UNRELATED_TOKEN': 'not-recorded'}
        self.assertEqual(validate_build_environment(normal), {'CARGO_BUILD_JOBS': '4', 'CARGO_INCREMENTAL': '0'})
        for key, value in [('CARGO_PROFILE_RELEASE_OPT_LEVEL', '1'),
                           ('CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_DEBUG', 'true'),
                           ('CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS', '-Clink-arg=/STACK:8388608'),
                           ('CARGO_ENCODED_RUSTFLAGS', '-C\x1fopt-level=1')]:
            environment = dict(normal, **{key: value})
            self.assertEqual(build_environment(environment)['environment'][key], value)
            with self.subTest(key=key), self.assertRaisesRegex(AssertionError, 'unexpected release profile/target Rustflags'):
                validate_build_environment(environment)

    def test_native_environment_cannot_inherit_another_harness_or_control(self):
        self.assertEqual(native_environment({'PATH': 'keep', 'OKILUM_UNKNOWN_CONTROL': 'bad',
                                             'OKILUM_DATA_VIEW_NATIVE_FIXTURE': 'other'}), {'PATH': 'keep'})

    def test_only_two_build_info_probes_before_pairs_and_no_probe_per_case(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inputs, out = root / 'inputs', root / 'out'
            raw = bytearray(256)
            struct.pack_into('<I', raw, 0x3c, 0x80)
            struct.pack_into('<H', raw, 0x98, 0x20b)
            struct.pack_into('<QQ', raw, 0x98 + 72, 1048576, 4096)
            digest = hashlib.sha256(raw).hexdigest()
            input_hashes = dict.fromkeys(['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml',
                'scripts/gpui-pre-windows/manifest.json',
                'scripts/patches/gpui-pre-windows/0001-directx-atlas-upload.diff'], 'same')
            for kind in SOURCES:
                folder = inputs / kind
                folder.mkdir(parents=True)
                (folder / 'okilum.exe').write_bytes(raw)
                (folder / 'build-manifest.json').write_text(json.dumps({'kind': kind,
                    'base_source': SOURCES[kind], 'diagnostic_commit': 'd' * 40,
                    'binary_sha256': digest, 'input_sha256': input_hashes}))

            def probe(args, **kwargs):
                self.assertEqual(args[1:], ['--build-info'])
                self.assertFalse(any(k.startswith('OKILUM_') for k in kwargs['env']))
                return 'source=' + 'd' * 40 + '\ngpui-test-support: no\n'

            with patch('runner.git', return_value=b'controller'), \
                    patch('runner.subprocess.check_output', side_effect=probe) as probes, \
                    patch.dict(os.environ, {'OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE': 'other'}):
                preflight(inputs, out)
                self.assertEqual(probes.call_count, 2)
            pinned = json.loads((out / 'preflight.json').read_text())

            def native(binary, env, log_path, evidence=None):
                self.assertEqual(sorted(k for k in env if k.startswith('OKILUM_')),
                                 ['OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE', 'OKILUM_DIAG_COLLECT_ALL'])
                value = receipt()
                for f in value['frames']:
                    f['subtree_work'] = {'diagnostic_qpc': {'before': {'pid': 100, 'tid': 200}}}
                (Path(env['OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE']) / 'toolbar-diagnostic-receipt.json').write_text(json.dumps(value))
                log_path.write_text('CODE2811_QPC_CENSUS_COMPLETE frames=96 strict_pass=true')
                return 100, 200, 0, False

            text = b'synthetic fixture for controller process-count test'
            with patch('runner.launch', side_effect=native), \
                    patch('runner.subprocess.check_output', side_effect=AssertionError('extra build-info inside trace')), \
                    patch('runner.FIXTURE_SHA', hashlib.sha256(text).hexdigest()):
                result = run_case(pinned, 'main', out / 'case', text)
                self.assertTrue(result['complete'])
                postflight(inputs, out)

    def test_pending_source_pin_cannot_dispatch_a_build(self):
        with self.assertRaisesRegex(AssertionError, 'not ready'):
            reference('toolbar', {'status': 'WAITING_FOR_FINAL_SOURCE'})

    def test_windows_x64_layouts(self):
        verify_layouts()

    def test_real_string_only_decode_and_identity_control(self):
        event = {'provider': MARKER, 'opcode': 0, 'version': 0, 'qpc': 1234,
                 'pid': 100, 'tid': 200, 'flags': 4,
                 'payload_hex': 'OKILUM_QPC_V1 id=7 pid=100 tid=200\0'.encode('utf-16-le').hex()}
        self.assertEqual(decode([event])['markers'][0], {'id': 7, 'pid': 100, 'tid': 200, 'qpc': 1234})
        event['pid'] = 999
        with self.assertRaisesRegex(EvidenceError, 'marker header identity'):
            decode([event])

    def test_marker_positive_control(self):
        clock = correlate_markers([frame()], markers(), 100, 200, 1000000)
        self.assertEqual(clock['offset_interval_qpc'], [-2, 2])
        self.assertEqual(clock['applied_offset_qpc'], 0)

    def test_missing_marker_is_rejected(self):
        with self.assertRaisesRegex(EvidenceError, 'missing/unexpected ETW markers'):
            correlate_markers([frame()], markers()[:1], 100, 200, 1000000)

    def test_duplicate_marker_is_rejected(self):
        rows = markers()
        rows.append(copy.deepcopy(rows[0]))
        with self.assertRaisesRegex(EvidenceError, 'duplicate ETW marker'):
            correlate_markers([frame()], rows, 100, 200, 1000000)

    def test_wrong_pid_marker_is_rejected(self):
        rows = markers()
        rows[0]['pid'] = 999
        with self.assertRaisesRegex(EvidenceError, 'wrong PID'):
            correlate_markers([frame()], rows, 100, 200, 1000000)

    def test_wrong_tid_marker_is_rejected(self):
        rows = markers()
        rows[0]['tid'] = 999
        with self.assertRaisesRegex(EvidenceError, 'wrong TID'):
            correlate_markers([frame()], rows, 100, 200, 1000000)

    def test_empty_clock_intersection_is_rejected(self):
        rows = markers()
        rows[0]['qpc'] += 100
        with self.assertRaisesRegex(EvidenceError, 'empty clock offset intersection'):
            correlate_markers([frame()], rows, 100, 200, 1000000)

    def test_nonzero_raw_clock_offset_is_rejected(self):
        rows = markers()
        for row in rows:
            row['qpc'] += 100
        with self.assertRaisesRegex(EvidenceError, 'excludes zero'):
            correlate_markers([frame()], rows, 100, 200, 1000000)

    def test_frame_must_stay_between_its_own_markers(self):
        value = frame()
        value['subtree_work']['diagnostic_qpc']['start_qpc_bounds'] = [1, 2]
        with self.assertRaisesRegex(EvidenceError, 'outside surrounding markers'):
            correlate_markers([value], markers(), 100, 200, 1000000)

    def test_loss_controls(self):
        meta = {'raw_timestamp_mode': RAW_MODE, 'clock_type': 1, 'frequency': 1000000,
                'process_trace_status': 0, 'close_trace_status': 0, 'callback_errors': [],
                'events_lost': 0, 'buffers_lost': 0, 'events_lost_after': 0, 'buffer_loss_samples': [0, 0]}
        loss_check(meta)
        for field in ['events_lost', 'buffers_lost', 'events_lost_after']:
            mutant = dict(meta, **{field: 1})
            with self.subTest(field=field), self.assertRaisesRegex(EvidenceError, 'ETW loss'):
                loss_check(mutant)
        with self.assertRaisesRegex(EvidenceError, 'ETW buffer loss'):
            loss_check(dict(meta, buffer_loss_samples=[0, 1]))

    def test_waiting_and_ready_with_real_waker_ownership(self):
        segments = scheduler_segments(decoded(), 100, 200, 10000, 50000)
        result = frame_analysis(frame(), segments, 1000000)
        self.assertTrue(result['wall_miss'])
        self.assertEqual(result['component_ranges_ms']['Waiting'], [8., 8.])
        self.assertEqual(result['component_ranges_ms']['Ready'], [20., 20.])
        self.assertAlmostEqual(result['boundary_uncertainty_ms'], .004)
        waiting = next(row for row in result['off_cpu_intervals'] if row['state'] == 'Waiting')
        self.assertEqual(waiting['ready_stack_owners'], [{'pid': 300, 'tid': 400}])
        self.assertEqual(waiting['reason'], 13)

    def test_missing_ready_transition_is_rejected(self):
        data = decoded()
        data['ready'] = [r for r in data['ready'] if r['qpc'] != 20000]
        with self.assertRaisesRegex(EvidenceError, 'missing ReadyThread transition'):
            scheduler_segments(data, 100, 200, 10000, 50000)

    def test_uncovered_states_are_rejected(self):
        segments = scheduler_segments(decoded(), 100, 200, 10000, 50000)
        segments = [s for s in segments if not (s['start'] == 12000 and s['end'] == 20000)]
        with self.assertRaisesRegex(EvidenceError, 'uncovered'):
            frame_analysis(frame(), segments, 1000000)

    def test_corrupt_frame_order_is_rejected(self):
        value = frame()
        value['subtree_work']['diagnostic_qpc']['end_qpc_bounds'] = [1, 2]
        with self.assertRaisesRegex(EvidenceError, 'corrupt frame QPC ordering'):
            frame_analysis(value, [], 1000000)

    def test_missing_waiting_waker_stack_is_rejected(self):
        data = decoded()
        data['stacks'] = [s for s in data['stacks'] if s['event_qpc'] != 20000]
        segments = scheduler_segments(data, 100, 200, 10000, 50000)
        result = frame_analysis(frame(), segments, 1000000)
        self.assertEqual(result['component_ranges_ms']['Waiting'], [8., 8.])
        self.assertEqual(result['causal_association'], 'PARTIAL_NO_EVIDENCE')

    def test_wrong_known_waker_owner_is_rejected(self):
        data = decoded()
        stack = next(s for s in data['stacks'] if s['event_qpc'] == 20000)
        stack['pid'], stack['tid'] = 999, 999
        result = frame_analysis(frame(), scheduler_segments(data, 100, 200, 10000, 50000), 1000000)
        waiting = next(s for s in result['off_cpu_intervals'] if s['state'] == 'Waiting')
        self.assertEqual(waiting['ready_stack'], [])
        self.assertEqual(waiting['ready_stack_owners'], [])
        self.assertIn('wrong StackWalk PID owner', waiting['ready_causal']['reason'])
        self.assertEqual(result['component_ranges_ms']['Waiting'], [8., 8.])

    def test_wrong_switch_cpu_cannot_supply_required_stack(self):
        data = decoded()
        next(s for s in data['stacks'] if s['event_qpc'] == 12000)['cpu'] = 9
        segments = scheduler_segments(data, 100, 200, 10000, 50000)
        result = frame_analysis(frame(), segments, 1000000)
        waiting = next(s for s in result['off_cpu_intervals'] if s['state'] == 'Waiting')
        self.assertEqual(waiting['switch_out_stack'], [])
        self.assertEqual(waiting['switch_out_causal']['evidence'], 'NO_EVIDENCE')

    def test_unknown_waker_header_uses_only_unique_exact_event_identity(self):
        data = decoded()
        target = next(r for r in data['ready'] if r['qpc'] == 20000)
        target['waker_pid'] = target['waker_tid'] = 0xffffffff
        segments = scheduler_segments(data, 100, 200, 10000, 50000)
        result = frame_analysis(frame(), segments, 1000000)
        waiting = next(s for s in result['off_cpu_intervals'] if s['state'] == 'Waiting')
        self.assertFalse(waiting['waker_header_identity_available'])
        self.assertEqual(waiting['ready_stack_owners'], [{'pid': 300, 'tid': 400}])
        # A second stackwalk-eligible event at that exact tick/CPU makes the
        # association ambiguous even when only one StackWalk payload is present.
        data['ready'].append(dict(target, tid=999))
        result = frame_analysis(frame(), scheduler_segments(data, 100, 200, 10000, 50000), 1000000)
        waiting = next(s for s in result['off_cpu_intervals'] if s['state'] == 'Waiting')
        self.assertEqual(waiting['ready_stack_owners'], [])
        self.assertIn('ambiguous StackWalk origin', waiting['ready_causal']['reason'])

    def test_inferred_waker_without_lifetime_is_rejected(self):
        data = decoded()
        target = next(r for r in data['ready'] if r['qpc'] == 20000)
        target['waker_pid'] = target['waker_tid'] = 0xffffffff
        data['lifetime'] = [e for e in data['lifetime'] if e['pid'] != 300]
        result = frame_analysis(frame(), scheduler_segments(data, 100, 200, 10000, 50000), 1000000)
        waiting = next(s for s in result['off_cpu_intervals'] if s['state'] == 'Waiting')
        self.assertEqual(waiting['ready_stack_owners'], [])
        self.assertIn('missing live StackWalk owner lifetime', waiting['ready_causal']['reason'])
        self.assertEqual(result['component_ranges_ms']['Ready'], [20., 20.])

    def test_same_pid_tid_reuse_gap_has_no_causal_owner(self):
        data = decoded()
        data['lifetime'].extend([{'qpc': 10000, 'pid': 300, 'tid': 400, 'opcode': 2},
                                 {'qpc': 30000, 'pid': 300, 'tid': 400, 'opcode': 1}])
        result = frame_analysis(frame(), scheduler_segments(data, 100, 200, 10000, 50000), 1000000)
        waiting = next(s for s in result['off_cpu_intervals'] if s['state'] == 'Waiting')
        self.assertEqual(waiting['ready_stack_owners'], [])
        self.assertEqual(waiting['ready_causal']['evidence'], 'NO_EVIDENCE')

    def test_primary_lifetime_gap_is_fatal_for_state_attribution(self):
        data = decoded()
        data['lifetime'].extend([{'qpc': 15000, 'pid': 100, 'tid': 200, 'opcode': 2},
                                 {'qpc': 25000, 'pid': 100, 'tid': 200, 'opcode': 1}])
        with self.assertRaisesRegex(EvidenceError, 'primary PID/TID lifetime coverage'):
            scheduler_segments(data, 100, 200, 10000, 50000)

    def test_impossible_cpu_migration_is_fatal_for_state_attribution(self):
        data = decoded()
        next(e for e in data['switches'] if e['qpc'] == 12000)['cpu'] = 9
        with self.assertRaisesRegex(EvidenceError, 'impossible running CPU migration'):
            scheduler_segments(data, 100, 200, 10000, 50000)

    def test_incomplete_thread_lifetime_is_rejected(self):
        data = decoded()
        data['lifetime'] = data['lifetime'][:1]
        with self.assertRaisesRegex(EvidenceError, 'lifetime coverage'):
            scheduler_segments(data, 100, 200, 10000, 50000)

    def test_collect_all_keeps_both_passes_but_fails_strict_verdict(self):
        value = receipt()
        validate_receipt(value, 'd' * 40, 0, 'CODE2811_QPC_CENSUS_COMPLETE frames=96 strict_pass=true')
        for index in [22, 63]:
            target = value['frames'][index]
            target['cpu_draw_ms'] = 39.5
            value['budget_violations'].append({'kind': 'frame', 'marks': target['marks'],
                'sample': target['sample'], 'wall_ms': 39.5, 'limit_ms': 16})
        value['strict_pass'] = False
        log = 'CODE2811_QPC_CENSUS_COMPLETE frames=96 strict_pass=false\noriginal native budgets failed after complete diagnostic census'
        self.assertEqual(len(validate_receipt(value, 'd' * 40, 1, log)), 96)
        with self.assertRaisesRegex(AssertionError, 'must exit nonzero'):
            validate_receipt(value, 'd' * 40, 0, log)
        value['budget_violations'].pop()
        with self.assertRaisesRegex(AssertionError, 'every original violation'):
            validate_receipt(value, 'd' * 40, 1, log)

    def test_functional_abort_is_not_a_complete_census(self):
        value = receipt()
        value['frames'] = value['frames'][:23]
        with self.assertRaises(AssertionError):
            validate_receipt(value, 'd' * 40, 1, '')


if __name__ == '__main__':
    unittest.main()
