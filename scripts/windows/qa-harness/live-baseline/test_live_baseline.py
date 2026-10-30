import copy
import hashlib
import unittest
from live_baseline import PINS, select

class Admission(unittest.TestCase):
    def test_both_retained_build_mappings_and_feed_mismatch(self):
        for version in PINS:
            with self.subTest(version=version):
                pins=copy.deepcopy(PINS); pins[version]['setup_sha256']=hashlib.sha256(b'signed fixture').hexdigest()
                feed={'Assets':[{'Type':'Full','Version':version,'SHA256':pins[version]['package_sha256']}]}
                baseline, _=select(feed,pins,lambda _: b'signed fixture')
                self.assertEqual(baseline['package'],version)
                self.assertEqual(baseline['build'],17175 if version.endswith('17312') else 18194)
                self.assertEqual(baseline['source'],PINS[version]['source'])
                with self.assertRaises(ValueError):select(feed,pins,lambda _: b'changed Setup')
                feed['Assets'][0]['SHA256']='0'*64
                with self.assertRaises(ValueError):select(feed,pins,lambda _: b'signed fixture')
    def test_unknown_newer_feed_cannot_fall_back_to_retained_seed(self):
        feed={'Assets':[{'Type':'Full','Version':'0.1.99999','SHA256':'0'*64}]}
        with self.assertRaises(KeyError):select(feed)
if __name__=='__main__':unittest.main()
