"""The native Reader gate must not hide a file viewer opening in Edit."""
import importlib.util
from pathlib import Path
import unittest


SPEC = importlib.util.spec_from_file_location(
    'reader_viewer_native_app', Path(__file__).with_name('reader-viewer-native-app.py'))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


def restored(name):
    return f'READER_NATIVE file={name} mode=read stage=leave-restored-edit\n'


class RestoredSourceScope(unittest.TestCase):
    def test_markdown_only_passes(self):
        RUNNER.assert_restored_source_scope(restored('note.md'))

    def test_missing_or_repeated_markdown_fails(self):
        for output in ['', restored('note.md') * 2]:
            with self.subTest(output=output), self.assertRaises(AssertionError):
                RUNNER.assert_restored_source_scope(output)

    def test_every_file_edit_fixture_is_rejected(self):
        for name in ['data.json', 'array.json', 'data.csv', 'source.rs']:
            with self.subTest(name=name), self.assertRaises(AssertionError):
                RUNNER.assert_restored_source_scope(restored(name) + restored('note.md'))


class FileEditStages(unittest.TestCase):
    def test_complete_real_reader_flow_is_required(self):
        receipts = [f'FILE_EDIT_NATIVE stage={i} mounted=Reader\n' for i in range(6)]
        RUNNER.assert_file_edit_stages(''.join(receipts))
        for output in ['', ''.join(receipts[:-1]), ''.join(receipts[1:]),
                       ''.join(receipts + [receipts[-1]]), ''.join(reversed(receipts))]:
            with self.subTest(output=output), self.assertRaises(AssertionError):
                RUNNER.assert_file_edit_stages(output)


if __name__ == '__main__':
    unittest.main()
