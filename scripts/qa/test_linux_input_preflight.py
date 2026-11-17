#!/usr/bin/env python3
"""Hermetic tests for linux_input_preflight.py: fake ydotool, real Unix sockets."""
import contextlib
import io
import json
import os
from pathlib import Path
import shutil
import socket
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import linux_input_preflight as pre  # noqa: E402


class SocketFixture(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="ydo-"))
        self.addCleanup(shutil.rmtree, self.root, True)
        self.bin = self.root
        self.calls = self.root / "calls"
        fake = self.root / "ydotool"
        fake.write_text(f"#!/bin/sh\necho \"$@\" >> '{self.calls}'\n")
        fake.chmod(0o755)
        path = os.environ.get("PATH", "")
        patcher = mock.patch.dict(os.environ, {"PATH": f"{self.bin}{os.pathsep}{path}"})
        patcher.start()
        self.addCleanup(patcher.stop)
        self.env = {"XDG_RUNTIME_DIR": str(self.root)}

    def listen(self, path):
        server = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        server.bind(str(path))
        self.addCleanup(server.close)
        return server

    def stale(self, path):
        self.listen(path).close()


class ResolveSocket(unittest.TestCase):
    def test_matches_ydotool_client_order(self):
        self.assertEqual(pre.resolve_socket({"YDOTOOL_SOCKET": "/x", "XDG_RUNTIME_DIR": "/r"}),
                         ("/x", "YDOTOOL_SOCKET"))
        self.assertEqual(pre.resolve_socket({"XDG_RUNTIME_DIR": "/run/user/1000"}),
                         ("/run/user/1000/.ydotool_socket", "XDG_RUNTIME_DIR"))
        self.assertEqual(pre.resolve_socket({}), ("/tmp/.ydotool_socket", "default"))


class ProbeSocket(SocketFixture):
    def test_missing_socket(self):
        self.assertEqual(pre.probe_socket(str(self.root / "none"))[0], "no-socket")

    def test_regular_file_is_not_a_socket(self):
        target = self.root / "file"
        target.write_text("")
        self.assertEqual(pre.probe_socket(str(target))[0], "not-a-socket")

    def test_stale_socket_is_refused(self):
        target = self.root / ".ydotool_socket"
        self.stale(target)
        reason, message = pre.probe_socket(str(target))
        self.assertEqual(reason, "stale-socket")
        self.assertIn("refuses connections", message)

    def test_listening_socket_passes(self):
        target = self.root / ".ydotool_socket"
        self.listen(target)
        self.assertIsNone(pre.probe_socket(str(target)))


class Preflight(SocketFixture):
    def pointer(self, *positions):
        values = iter(positions)
        return lambda: next(values)

    def test_missing_ydotool_blocks(self):
        with mock.patch.object(pre.shutil, "which", return_value=None):
            with self.assertRaises(pre.Blocked) as blocked:
                pre.preflight(self.env)
        self.assertEqual(blocked.exception.reason, "ydotool-missing")

    def test_stale_socket_blocks_before_any_input(self):
        self.stale(self.root / ".ydotool_socket")
        with mock.patch.object(pre, "candidate_sockets", return_value=[str(self.root / ".ydotool_socket")]):
            with self.assertRaises(pre.Blocked) as blocked:
                pre.preflight(self.env)
        self.assertEqual(blocked.exception.reason, "stale-socket")
        self.assertFalse(self.calls.exists(), "no ydotool command may run when the socket is dead")

    def test_socket_path_mismatch_names_the_live_socket(self):
        live = self.root / "other.sock"
        self.listen(live)
        env = {"YDOTOOL_SOCKET": str(self.root / "wanted.sock")}
        with mock.patch.object(pre, "candidate_sockets", return_value=[env["YDOTOOL_SOCKET"], str(live)]):
            with self.assertRaises(pre.Blocked) as blocked:
                pre.preflight(env)
        self.assertEqual(blocked.exception.reason, "socket-path-mismatch")
        self.assertIn(f"YDOTOOL_SOCKET={live}", blocked.exception.message)

    def test_positive_control_moves_and_restores(self):
        self.listen(self.root / ".ydotool_socket")
        reader = self.pointer((500, 300), (470, 300))
        with mock.patch.object(pre, "select_probe", return_value=("gnome", reader)):
            result = pre.preflight(self.env, settle=0)
        self.assertEqual(result["control"], "gnome")
        self.assertEqual(result["before"], [500, 300])
        self.assertEqual(self.calls.read_text().split("\n")[:2],
                         [f"mousemove -x {-pre.STEP} -y 0", f"mousemove -x {pre.STEP} -y 0"])

    def test_unmoved_pointer_blocks_and_still_restores(self):
        self.listen(self.root / ".ydotool_socket")
        reader = self.pointer((10, 10), (10, 10))
        with mock.patch.object(pre, "select_probe", return_value=("gnome", reader)):
            with self.assertRaises(pre.Blocked) as blocked:
                pre.preflight(self.env, settle=0)
        self.assertEqual(blocked.exception.reason, "input-not-delivered")
        self.assertEqual(len(self.calls.read_text().splitlines()), 2)

    def test_no_probe_blocks_unless_socket_only(self):
        self.listen(self.root / ".ydotool_socket")
        with mock.patch.object(pre, "select_probe", return_value=(None, None)):
            with self.assertRaises(pre.Blocked) as blocked:
                pre.preflight(self.env)
            self.assertEqual(blocked.exception.reason, "no-pointer-probe")
            self.assertEqual(pre.preflight(self.env, socket_only=True)["control"], "skipped")

    def test_main_exit_code_and_summary_line(self):
        env = {"XDG_RUNTIME_DIR": str(self.root), "PATH": os.environ["PATH"]}
        out = io.StringIO()
        with mock.patch.dict(os.environ, env, clear=True), \
                mock.patch.object(pre, "candidate_sockets", return_value=[]), \
                contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            code = pre.main(["--json"])
        self.assertEqual(code, pre.BLOCKED)
        payload = json.loads(out.getvalue())
        self.assertEqual((payload["status"], payload["reason"]), ("BLOCKED_HARNESS", "no-socket"))


if __name__ == "__main__":
    unittest.main()
