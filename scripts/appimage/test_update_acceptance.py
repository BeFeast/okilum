"""Hosted instrument regressions: identity-only receipts must never pass."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import fcntl

spec = importlib.util.spec_from_file_location('driver', Path(__file__).with_name('update-acceptance.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class CompletionControl(unittest.TestCase):
    def test_identity_before_confirmation_commit_and_lock_release_is_insufficient(self):
        with tempfile.TemporaryDirectory() as temporary:
            case = Path(temporary)
            target = case / 'Original with spaces.AppImage'
            target.write_bytes(b'actual replacement control')
            target.chmod(0o755)
            stage = case / '.okilum-update-control'
            stage.mkdir()
            (stage / 'previous.AppImage').write_bytes(b'old control')
            receipt = case / 'receipt.json'
            identity = dict(build='5011', source='b' * 40, path=str(target), sha256=driver.digest(target))
            receipt.write_text(json.dumps(identity))
            expected = dict(build_version='5011', source_sha='b' * 40, sha256=identity['sha256'],
                            _old_sha256=driver.digest(stage / 'previous.AppImage'))
            log = stage / 'apply.log'
            log.write_text('Applied\nAPPIMAGE_NATIVE_RESTART_IDENTITY_PASS\n')
            lock = (case / f'.{target.name}.update-lock').open('w+b')
            self.addCleanup(lock.close)
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.assertIsNone(driver.transaction_complete(case, target, receipt, expected))
            with self.assertRaisesRegex(AssertionError, 'never succeeded'):
                driver.wait_for_transaction(case, target, receipt, expected, timeout=0.01)
            (stage / 'started').write_text('startup confirmed\n')
            self.assertIsNone(driver.transaction_complete(case, target, receipt, expected))
            with log.open('a') as stream:
                stream.write('Startup confirmed; transaction committed and update lock released\n')
            self.assertIsNone(driver.transaction_complete(case, target, receipt, expected), 'helper still owns lock')
            fcntl.flock(lock, fcntl.LOCK_UN)
            self.assertEqual(driver.transaction_complete(case, target, receipt, expected), (identity, stage),
                             'positive control must detect the complete transaction')
            with log.open('a') as stream:
                stream.write('Updated application exited; restoring previous AppImage\n')
            with self.assertRaisesRegex(AssertionError, 'replacement failed'):
                driver.transaction_complete(case, target, receipt, expected)


if __name__ == '__main__':
    unittest.main()
