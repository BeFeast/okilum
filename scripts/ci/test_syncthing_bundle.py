"""Pinned provenance and cross-platform archive admission negative controls."""
import importlib.util
import json
import re
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
    def runtime_fixture(self, app, arch="arm64"):
        binary = b"\xcf\xfa\xed\xfe" + struct.pack("<I", bundle.CPU[arch]) + b"\0" * 40
        (app / bundle.RUNTIME).parent.mkdir(parents=True)
        (app / bundle.RUNTIME).write_bytes(binary)
        (app / bundle.PROVENANCE).parent.mkdir(parents=True)
        provenance = dict(schema=1, version=bundle.VERSION, architecture=arch,
                          archive_sha256=bundle.PINS[arch],
                          upstream_binary_sha256=bundle.digest(binary),
                          signed_binary_sha256=bundle.digest(binary),
                          code_identifier="uk.okilum.syncthing", signed=False)
        (app / bundle.PROVENANCE).write_text(json.dumps(provenance))
        return binary, provenance

    def test_verify_exact_post_signing_provenance_and_malformed_controls(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as tmp:
            app = Path(tmp)
            binary, provenance = self.runtime_fixture(app)
            details = "Identifier=uk.okilum.syncthing\nflags=runtime\n"
            with patch.object(bundle, "run", return_value=SimpleNamespace(stderr=details)) as run:
                bundle.verify(app, "uk.okilum")
                self.assertEqual(run.call_count, 2)
                for field, value in (("schema", True), ("version", "0"),
                                     ("architecture", []), ("architecture", "amd64"),
                                     ("archive_sha256", "0" * 64),
                                     ("signed_binary_sha256", "0" * 64),
                                     ("signed_binary_sha256", None),
                                     ("code_identifier", "other.syncthing")):
                    with self.subTest(field=field, value=value):
                        bad = dict(provenance, **{field: value})
                        (app / bundle.PROVENANCE).write_text(json.dumps(bad))
                        with self.assertRaises(ValueError):
                            bundle.verify(app, "uk.okilum")
                for malformed in ("[]", "null", "{"):
                    (app / bundle.PROVENANCE).write_text(malformed)
                    with self.assertRaises(ValueError):
                        bundle.verify(app, "uk.okilum")
                (app / bundle.PROVENANCE).write_text(json.dumps(provenance))
                (app / bundle.RUNTIME).write_bytes(binary + b"changed after signing")
                with self.assertRaisesRegex(ValueError, "SHA256"):
                    bundle.verify(app, "uk.okilum")
                for malformed in (b"", b"not Mach-O" * 10):
                    (app / bundle.RUNTIME).write_bytes(malformed)
                    with self.assertRaisesRegex(ValueError, "architecture"):
                        bundle.verify(app, "uk.okilum")
                (app / bundle.RUNTIME).write_bytes(binary)
            for bad_details in ("OtherIdentifier=uk.okilum.syncthing\n",
                                "Identifier=other\n", details + details):
                with patch.object(bundle, "run", return_value=SimpleNamespace(stderr=bad_details)):
                    with self.assertRaisesRegex(ValueError, "identifier"):
                        bundle.verify(app, "uk.okilum")

    def test_sign_validates_changed_bytes_before_publishing_provenance(self):
        from types import SimpleNamespace
        for wrong_arch in (False, True):
            with self.subTest(wrong_arch=wrong_arch), tempfile.TemporaryDirectory() as tmp:
                app = Path(tmp)
                binary, provenance = self.runtime_fixture(app)
                original = (app / bundle.PROVENANCE).read_bytes()
                signed = (b"\xcf\xfa\xed\xfe" + struct.pack("<I", bundle.CPU["amd64"])
                          + b"\0" * 40) if wrong_arch else binary + b"signature"

                def codesign(*args):
                    if "--sign" in args:
                        (app / bundle.RUNTIME).write_bytes(signed)
                    return SimpleNamespace(stderr="Identifier=uk.okilum.syncthing\nflags=runtime\n")

                with patch.object(bundle, "run", side_effect=codesign):
                    if wrong_arch:
                        with self.assertRaisesRegex(ValueError, "architecture"):
                            bundle.sign(app, "-", "uk.okilum")
                        self.assertEqual((app / bundle.PROVENANCE).read_bytes(), original)
                    else:
                        bundle.sign(app, "-", "uk.okilum")
                        retained = json.loads((app / bundle.PROVENANCE).read_text())
                        self.assertEqual(retained["signed_binary_sha256"], bundle.digest(signed))
                        self.assertNotEqual(retained["signed_binary_sha256"],
                                            retained["upstream_binary_sha256"])
                        bundle.verify(app, "uk.okilum")

    def test_packaging_version_matches_existing_runtime_contract(self):
        repo = Path(__file__).resolve().parents[2]
        source = (repo / "crates/okilum-sync-controller/src/daemon.rs").read_text()
        expected = re.search(r'pub const CLIENT_VERSION: &str = "v([0-9.]+)";', source)
        self.assertIsNotNone(expected, "runtime contract version declaration changed")
        self.assertEqual(bundle.VERSION, expected.group(1),
                         "update packaging archive pins and signed checksum list with runtime contract")

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
