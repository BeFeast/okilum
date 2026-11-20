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


def receipt(mode='production-release', platform='win32'):
    allowed, ceiling = probe.wall_allowance(platform)
    return {'real_native_platform': True, 'mounted': 'Reader', 'bytes': 3_500_000,
            'paths_keys': 414, 'initial_rasterizations': 40, 'measurement': mode, 'open_ms': 5, 'source_sha': 'a' * 40,
            'running_budget_ms': 16, 'wall_budget_ms': 16, 'wall_misses_allowed': allowed, 'wall_ceiling_ms': ceiling,
            'frames': [{'marks': marks, 'sample': sample, 'cpu_draw_ms': 4, 'running_ms': 3, 'present_ms': 1, 'rows_built': 24, 'text_work': {'work': {'glyph_bitmap': {'calls': 0}}}, 'scroll_y': -32 * (sample + 1)}
                       for marks in [False, True] for sample in range(48)]}


class NativeToolbarReceiptTests(unittest.TestCase):
    def run_probe(self, result, smoke=False, platform='win32'):
        def app(_args, *, env, **_kwargs):
            self.assertEqual('OKILUM_CODE_TOOLBAR_NATIVE_SMOKE' in env, smoke)
            root = Path(env['OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE'])
            (root / 'toolbar-receipt.json').write_text(json.dumps(result))
            return SimpleNamespace(returncode=0)
        with tempfile.TemporaryDirectory(prefix='toolbar-receipt-') as parent:
            with patch.dict(os.environ, {'RUNNER_TEMP': parent, 'OKILUM_CODE_TOOLBAR_NATIVE_SMOKE': 'inherited', 'OKILUM_SOURCE_COMMIT': 'a' * 40}), \
                    patch.object(sys, 'argv', ['probe', str(Path(parent) / 'app'), *(['--smoke'] if smoke else [])]), \
                    patch.object(probe.subprocess, 'run', side_effect=app), patch('builtins.print'), \
                    patch.object(sys, 'platform', platform):
                probe.main()

    def test_release_enforces_open_frame_and_actual_work_budgets(self):
        # Hosted Windows (WARP Ready time) allows 2 slow frames per pass up to
        # 50 ms; other platforms allow 1 up to 50 ms (#2845).
        self.assertEqual(probe.wall_allowance('win32'), (2, 50))
        self.assertEqual(probe.wall_allowance('darwin'), (1, 50))
        self.assertEqual(probe.wall_allowance('linux'), (1, 50))
        for platform in ['win32', 'darwin']:
            allowed, ceiling = probe.wall_allowance(platform)
            with self.subTest(platform=platform, case='clean'):
                self.run_probe(receipt(platform=platform), platform=platform)
            with self.subTest(platform=platform, case='tolerated'):
                tolerated = receipt(platform=platform)
                for f in tolerated['frames'][:allowed] + tolerated['frames'][48:48 + allowed]:
                    f['cpu_draw_ms'], f['running_ms'] = ceiling, 3
                self.run_probe(tolerated, platform=platform)
            with self.subTest(platform=platform, case='other-platform-receipt'):
                other = receipt(platform='darwin' if platform == 'win32' else 'win32')
                with self.assertRaises(AssertionError):
                    self.run_probe(other, platform=platform)
            self.mutations(platform, allowed, ceiling)

    def mutations(self, platform, allowed, ceiling):
        for mutation in ['open', 'running', 'wall-misses', 'wall-ceiling', 'meter-ahead', 'budget', 'rows', 'scroll', 'profile', 'source', 'present', 'raster', 'unfired-raster']:
            with self.subTest(platform=platform, mutation=mutation):
                result = copy.deepcopy(receipt(platform=platform))
                if mutation == 'open':
                    result['open_ms'] = 100.01
                elif mutation == 'running':
                    result['frames'][0]['cpu_draw_ms'], result['frames'][0]['running_ms'] = 17, 16.01
                elif mutation == 'wall-misses':
                    for f in result['frames'][48:48 + allowed + 1]:
                        f['cpu_draw_ms'] = 16.01
                elif mutation == 'wall-ceiling':
                    result['frames'][0]['cpu_draw_ms'] = ceiling + 0.01
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
                    self.run_probe(result, platform=platform)

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
