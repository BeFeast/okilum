"""Release budgets cannot be satisfied by a debug smoke or cached draws."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('toolbar_probe', Path(__file__).with_name('code-toolbar-native-app.py'))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


def receipt(mode='production-release'):
    return {'real_native_platform': True, 'mounted': 'Reader', 'bytes': 3_500_000,
            'paths_keys': 414, 'measurement': mode, 'open_ms': 5,
            'frames': [{'marks': marks, 'cpu_draw_ms': 4, 'rows_built': 24, 'scroll_y': -32 * (sample + 1)}
                       for marks in [False, True] for sample in range(48)]}


class NativeToolbarReceiptTests(unittest.TestCase):
    def run_probe(self, result, smoke=False):
        def app(_args, *, env, **_kwargs):
            self.assertEqual('OKILUM_CODE_TOOLBAR_NATIVE_SMOKE' in env, smoke)
            root = Path(env['OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE'])
            (root / 'toolbar-receipt.json').write_text(json.dumps(result))
            return SimpleNamespace(returncode=0)
        with tempfile.TemporaryDirectory(prefix='toolbar-receipt-') as parent:
            with patch.dict(os.environ, {'RUNNER_TEMP': parent, 'OKILUM_CODE_TOOLBAR_NATIVE_SMOKE': 'inherited'}), \
                    patch.object(sys, 'argv', ['probe', str(Path(parent) / 'app'), *(['--smoke'] if smoke else [])]), \
                    patch.object(probe.subprocess, 'run', side_effect=app), patch('builtins.print'):
                probe.main()

    def test_release_enforces_open_frame_and_actual_work_budgets(self):
        self.run_probe(receipt())
        for mutation in ['open', 'frame', 'rows', 'scroll', 'profile']:
            with self.subTest(mutation=mutation):
                result = copy.deepcopy(receipt())
                if mutation == 'open':
                    result['open_ms'] = 100.01
                elif mutation == 'frame':
                    result['frames'][0]['cpu_draw_ms'] = 16.01
                elif mutation == 'rows':
                    result['frames'][0]['rows_built'] = 0
                elif mutation == 'scroll':
                    result['frames'][1]['scroll_y'] = result['frames'][0]['scroll_y']
                else:
                    result['measurement'] = 'debug-smoke'
                with self.assertRaises(AssertionError):
                    self.run_probe(result)

    def test_debug_smoke_has_a_distinct_receipt_and_keeps_work_controls(self):
        result = receipt('debug-smoke')
        result['open_ms'] = 200
        result['frames'][0]['cpu_draw_ms'] = 30
        self.run_probe(result, smoke=True)
        result['frames'][0]['rows_built'] = 0
        with self.assertRaises(AssertionError):
            self.run_probe(result, smoke=True)
        with self.assertRaises(AssertionError):
            self.run_probe(receipt(), smoke=True)


if __name__ == '__main__':
    unittest.main()
