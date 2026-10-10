import contextlib
import unittest.mock
import io
import tempfile
import unittest
from pathlib import Path

import diff


class DiffTest(unittest.TestCase):
    def run_diff(self, before, after, *extra):
        with tempfile.TemporaryDirectory() as tmp:
            b, a = Path(tmp, 'before.txt'), Path(tmp, 'after.txt')
            b.write_text('\n'.join(before) + '\n', encoding='utf-8')
            a.write_text('\n'.join(after) + '\n', encoding='utf-8')
            out = io.StringIO()
            with contextlib.redirect_stdout(out), unittest.mock.patch(
                'sys.argv', ['diff.py', str(b), str(a), *extra]
            ):
                code = diff.main()
            return code, out.getvalue()

    def test_qa_home_named_after_okilum_is_not_residue(self):
        home = '/home/qa/okilum-night/home'
        code, out = self.run_diff(
            [f'# home {home}'],
            [f'# home {home}', f'F {home}/.cache/mesa_shader_cache', f'F {home}/.config'],
        )
        self.assertEqual(code, 0, out)
        self.assertIn('0 left by Okilum', out)

    def test_app_leftovers_below_the_home_still_fail(self):
        # Positive control: the same QA home still catches real residue.
        home = '/home/qa/okilum-night/home'
        code, out = self.run_diff(
            [f'# home {home}'],
            [f'# home {home}', f'F {home}/.local/state/okilum/reader-ui.json'],
        )
        self.assertEqual(code, 1, out)
        self.assertIn('OKILUM', out)

    def test_exported_drafts_and_vaults_may_remain(self):
        home = r'C:\Users\qa'
        code, out = self.run_diff(
            [],
            [
                rf'F {home}\Documents\Okilum unsaved drafts\note.md',
                rf'F {home}\Notes\okilum.md',
            ],
            '--home', home, '--vault', rf'{home}\Notes',
        )
        self.assertEqual(code, 0, out)
        self.assertIn('export', out)
        self.assertIn('vault', out)

    def test_without_a_home_the_whole_path_is_checked(self):
        code, _ = self.run_diff([], ['K HKEY_CURRENT_USER\\Software\\Classes\\okilum'])
        self.assertEqual(code, 1)


if __name__ == '__main__':
    unittest.main()
