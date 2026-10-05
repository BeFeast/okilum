#!/usr/bin/env python3
"""Small guard tests for maintenance selection/provenance; no artifact gate claim."""
import importlib.util
import hashlib
import json
from unittest import mock
import pathlib
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("matrix", pathlib.Path(__file__).with_name("maintenance-test-matrix.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class Guards(unittest.TestCase):
    def test_enabled_keeps_full_unfiltered_workspace_tests(self):
        self.assertEqual(m.test_command(True), ["cargo", "test", "--workspace"])

    def test_maintenance_filters_only_exact_classified_inventory(self):
        names = m.exclusions()
        command = m.test_command(False, names + ["unrelated::new_regression"])
        self.assertEqual(command.count("--skip"), 15)
        self.assertNotIn("unrelated::new_regression", command)
        with self.assertRaises(ValueError):
            m.test_command(False, names[:-1])
        with self.assertRaises(ValueError):
            m.test_command(False, names + [names[0] + "_new_regression"])

    def test_unknown_or_ambiguous_posture_refuses(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            path = root / "crates/tessera-brain/src/maestro_links.rs"
            path.parent.mkdir(parents=True)
            for text in ["", "pub const NEW_LINKS_ENABLED: bool = dynamic();", "pub const NEW_LINKS_ENABLED: bool = true;\npub const NEW_LINKS_ENABLED: bool = false;"]:
                path.write_text(text)
                with self.assertRaises(ValueError):
                    m.posture(root)
            path.write_text("pub const NEW_LINKS_ENABLED: bool = false;")
            self.assertFalse(m.posture(root))

    def test_missing_provenance_cannot_pass(self):
        with self.assertRaises(ValueError):
            m.artifact({"binary": "/not-a-receipt"}, pathlib.Path("/"))


    def test_maintenance_requires_artifacts_before_cargo(self):
        with mock.patch.object(m, "posture", return_value=False), mock.patch.object(m.sys, "argv", ["matrix"]), mock.patch.object(m.subprocess, "run") as run:
            with self.assertRaises(SystemExit) as error:
                m.main()
            self.assertEqual(error.exception.code, 2)
            run.assert_not_called()

    def test_frozen_receipt_and_gui_binding_reject_tampering(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root / "tessera-cored").write_bytes(b"backend")
            (root / "tessera").write_bytes(b"gui")
            sha = lambda data: hashlib.sha256(data).hexdigest()
            receipt = {"commit": "a" * 40, "files": {"tessera-cored": sha(b"backend"), "tessera": sha(b"gui")}, "compiler_artifacts": {"tessera": {}, "tessera-cored": {}}}
            freeze = root / "freeze.json"
            freeze.write_text(json.dumps(receipt))
            entry = {"binary": "tessera-cored", "sha256": sha(b"backend"), "source": "a" * 40, "freeze": "freeze.json", "freeze_sha256": sha(freeze.read_bytes()), "feature_graph": "gui+cored"}
            self.assertEqual(m.artifact(entry, root), root / "tessera-cored")
            (root / "tessera").write_bytes(b"another gui")
            with self.assertRaises(ValueError):
                m.artifact(entry, root)
            (root / "tessera").write_bytes(b"gui")
            freeze.write_text(json.dumps({**receipt, "commit": "b" * 40}))
            with self.assertRaises(ValueError):
                m.artifact(entry, root)
            changed = {**entry, "freeze_sha256": sha(freeze.read_bytes())}
            with self.assertRaises(ValueError):
                m.artifact(changed, root)


if __name__ == "__main__":
    unittest.main()
