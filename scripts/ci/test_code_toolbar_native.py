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
            'paths_keys': 414, 'initial_rasterizations': 40, 'measurement': mode, 'open_ms': 5, 'source_sha': 'a' * 40,
            'running_budget_ms': 16, 'wall_budget_ms': 16, 'wall_misses_allowed': 5, 'wall_ceiling_ms': 100,
            'frames': [{'marks': marks, 'sample': sample, 'cpu_draw_ms': 4, 'running_ms': 3, 'present_ms': 1, 'rows_built': 24, 'text_work': {'work': {'glyph_bitmap': {'calls': 0}}}, 'scroll_y': -32 * (sample + 1)}
                       for marks in [False, True] for sample in range(48)]}


class NativeToolbarReceiptTests(unittest.TestCase):
    def run_probe(self, result, smoke=False):
        def app(_args, *, env, **_kwargs):
            self.assertEqual('OKILUM_CODE_TOOLBAR_NATIVE_SMOKE' in env, smoke)
            root = Path(env['OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE'])
            (root / 'toolbar-receipt.json').write_text(json.dumps(result))
            return SimpleNamespace(returncode=0)
        with tempfile.TemporaryDirectory(prefix='toolbar-receipt-') as parent:
            with patch.dict(os.environ, {'RUNNER_TEMP': parent, 'OKILUM_CODE_TOOLBAR_NATIVE_SMOKE': 'inherited', 'OKILUM_SOURCE_COMMIT': 'a' * 40}), \
                    patch.object(sys, 'argv', ['probe', str(Path(parent) / 'app'), *(['--smoke'] if smoke else [])]), \
                    patch.object(probe.subprocess, 'run', side_effect=app), patch('builtins.print'):
                probe.main()

    def test_release_enforces_open_frame_and_actual_work_budgets(self):
        self.run_probe(receipt())
        # Hosted WARP: up to 5 frames per pass may wait Ready past 16 ms wall.
        tolerated = receipt()
        for f in tolerated['frames'][:5] + tolerated['frames'][48:53]:
            f['cpu_draw_ms'], f['running_ms'] = 41.7, 3
        self.run_probe(tolerated)
        for mutation in ['open', 'running', 'wall-misses', 'wall-ceiling', 'meter-ahead', 'budget', 'rows', 'scroll', 'profile', 'source', 'present', 'raster', 'unfired-raster']:
            with self.subTest(mutation=mutation):
                result = copy.deepcopy(receipt())
                if mutation == 'open':
                    result['open_ms'] = 100.01
                elif mutation == 'running':
                    result['frames'][0]['cpu_draw_ms'], result['frames'][0]['running_ms'] = 17, 16.01
                elif mutation == 'wall-misses':
                    for f in result['frames'][48:54]:
                        f['cpu_draw_ms'] = 16.01
                elif mutation == 'wall-ceiling':
                    result['frames'][0]['cpu_draw_ms'] = 100.01
                elif mutation == 'meter-ahead':
                    result['frames'][0]['running_ms'] = 5.3
                elif mutation == 'budget':
                    result['wall_misses_allowed'] = 48
                elif mutation == 'rows':
                    result['frames'][0]['rows_built'] = 0
                elif mutation == 'scroll':
                    result['frames'][1]['scroll_y'] = result['frames'][0]['scroll_y']
                elif mutation == 'profile':
                    result['measurement'] = 'debug-smoke'
                elif mutation == 'source':
                    result['source_sha'] = 'b' * 40
                elif mutation == 'present':
                    del result['frames'][0]['present_ms']
                elif mutation == 'raster':
                    result['frames'][0]['text_work']['work']['glyph_bitmap']['calls'] = 2
                else:
                    result['initial_rasterizations'] = 0
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
