"""Pinned provenance and cross-platform archive admission negative controls."""
import importlib.util
import json
from pathlib import Path
import shutil
import struct
import tempfile
import unittest
from unittest.mock import patch
import zipfile

SPEC = importlib.util.spec_from_file_location("syncthing_bundle",
    Path(__file__).resolve().parents[1] / "syncthing/bundle.py")
bundle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundle)


class SyncthingBundleTests(unittest.TestCase):
    def test_pinned_signature_required_bad_signature_never_accepted(self):
        good = "[GNUPG:] VALIDSIG " + bundle.FINGERPRINT + " 2026 0 0\n"
        legacy = "[GNUPG:] NO_PUBKEY " + bundle.LEGACY + "\n"
        self.assertEqual(bundle.signature_status(good + legacy)["validsig"], bundle.FINGERPRINT)
        for status in (legacy, good.replace(bundle.FINGERPRINT, "A" * 40),
                       good + "[GNUPG:] BADSIG legacy\n",
                       good + "[GNUPG:] NO_PUBKEY ABCDEF\n"):
            with self.assertRaises(ValueError):
                bundle.signature_status(status)

    @unittest.skipUnless(shutil.which("gpg"), "GPG required for signed-list verification")
    def test_committed_key_signature_and_both_archive_pins(self):
        proof = bundle.verify_upstream()
        self.assertEqual(proof["validsig"], bundle.FINGERPRINT)
        self.assertIn(bundle.LEGACY, proof["expected_legacy_no_pubkey"])

    def test_macho_architecture_and_archive_checksum_before_any_staging(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for arch in bundle.PINS:
                archive = root / (arch + ".zip")
                binary = b"\xcf\xfa\xed\xfe" + struct.pack("<I", bundle.CPU[arch]) + b"\0" * 40
                with zipfile.ZipFile(archive, "w") as z:
                    prefix = "syncthing-macos-" + arch + "-v2.1.6/"
                    for name, data in {"syncthing": binary, "LICENSE.txt": b"MPL-2.0",
                                       "AUTHORS.txt": b"Authors", "README.txt": b"Readme"}.items():
                        z.writestr(prefix + name, data)
                pin = bundle.digest(archive.read_bytes())
                with patch.dict(bundle.PINS, {arch: pin}), patch.object(bundle, "verify_upstream", return_value={}):
                    app = root / (arch + ".app")
                    bundle.stage(app, arch, archive)
                    self.assertEqual((app / bundle.RUNTIME).read_bytes(), binary)
                    provenance = json.loads((app / bundle.PROVENANCE).read_text())
                    self.assertEqual(provenance["upstream_binary_sha256"], bundle.digest(binary))
                    self.assertFalse(provenance["signed"])
                    # A failed digest must not even create an app destination.
                    archive.write_bytes(archive.read_bytes() + b"tampered")
                    rejected = root / (arch + "-reject.app")
                    with self.assertRaisesRegex(ValueError, "SHA256"):
                        bundle.stage(rejected, arch, archive)
                    self.assertFalse(rejected.exists())


if __name__ == "__main__":
    unittest.main()
