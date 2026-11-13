import copy
import json
from pathlib import Path
import tempfile
from unittest.mock import patch

from capture_guard import setup_proof
import unittest

from marker_setup_probe import MARKER, MESSAGE, TOKEN, command, validate_events


class MarkerSetupControls(unittest.TestCase):
    def test_exact_xperf_arguments_are_unbraced_and_default_clock(self):
        args = command('xperf.exe', TOKEN, 'proof.etl')
        self.assertEqual(args[args.index('-on') + 1], MARKER + ':0x1:4')
        self.assertEqual(args[args.index('-ClockType') + 1], 'PerfCounter')
        self.assertNotIn('{', TOKEN)
        self.assertNotIn('runner.py', args)

    def test_raw_marker_requires_exact_provider_identity_payload_and_qpc(self):
        emission = {'pid': 11, 'tid': 22, 'frequency': 10_000_000,
                    'before_qpc': 100, 'after_qpc': 110}
        meta = {'raw_timestamp_mode': 0x10001000, 'clock_type': 1, 'frequency': 10_000_000}
        event = {'provider': MARKER, 'pid': 11, 'tid': 22, 'qpc': 105,
                 'payload_hex': (MESSAGE + '\x00').encode('utf-16-le').hex()}
        self.assertEqual(validate_events([event], emission, meta), event)
        for key, bad in [('provider', 'another'), ('pid', 10), ('tid', 23),
                         ('qpc', 111), ('payload_hex', '0000')]:
            changed = dict(event, **{key: bad})
            with self.subTest(key=key), self.assertRaises(AssertionError):
                validate_events([changed], emission, meta)
        for events in [[], [event, copy.deepcopy(event)]]:
            with self.assertRaises(AssertionError):
                validate_events(events, emission, meta)
        for key, bad in [('raw_timestamp_mode', 0x10000000), ('clock_type', 2), ('frequency', 1)]:
            with self.subTest(key=key), self.assertRaises(AssertionError):
                validate_events([event], emission, dict(meta, **{key: bad}))

    def test_proof_requires_the_exact_previously_collected_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'proof.json'
            path.write_text(json.dumps({'complete': True, 'benchmark_launches': 0, 'xperf_sha256': 'pinned'}))
            with self.assertRaisesRegex(AssertionError, 'proof bytes differ'):
                setup_proof(path)
            import hashlib
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            with patch('capture_guard.PROOF_SHA256', digest):
                self.assertEqual(setup_proof(path)['xperf_sha256'], 'pinned')
                path.write_text(path.read_text() + ' ')
                with self.assertRaisesRegex(AssertionError, 'proof bytes differ'):
                    setup_proof(path)

    def test_census_command_uses_the_proven_token_and_requires_proof(self):
        script = (Path(__file__).parent / 'capture.ps1').read_text()
        self.assertIn("-on '" + TOKEN + "' -ClockType PerfCounter", script)
        self.assertNotIn("-on '{" + MARKER, script)
        self.assertLess(script.index('Separate reviewed marker setup proof is required'),
                        script.index('python scripts/ci/perf-2811-qpc/runner.py'))
        self.assertIn('WPT differs from the proven setup binary', script)
