import copy
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
