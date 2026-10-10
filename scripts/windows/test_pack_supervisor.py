"""The supervisor in the Windows package (#1013): the PE reader, and scripts/windows/pack.sh
refusing a payload or a package without a GUI-subsystem helper. vpk is replaced by a
stand-in that zips the payload the way Velopack lays it out (`lib/app/...`), so these run
anywhere; the real vpk layout was checked once by hand and is what the stand-in copies."""
import os
import pathlib
import stat
import struct
import subprocess
import sys
import tempfile
import unittest
import zipfile

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(HERE))
import pe  # noqa: E402

HELPER = "okilum-sync-supervisor.exe"


def image(subsystem=pe.SUBSYSTEM_GUI, machine=pe.MACHINE_AMD64, magic=pe.PE32_PLUS):
    dos = bytearray(64)
    dos[0:2] = b"MZ"
    struct.pack_into("<I", dos, 0x3C, 64)
    coff = struct.pack("<HHIIIHH", machine, 0, 0, 0, 0, 240, 0x22)
    optional = bytearray(240)
    struct.pack_into("<H", optional, 0, magic)
    struct.pack_into("<H", optional, 68, subsystem)
    return bytes(dos) + b"PE\0\0" + coff + bytes(optional)


class Reader(unittest.TestCase):
    def test_reads_machine_and_subsystem(self):
        self.assertEqual(pe.read_header(image()), (pe.MACHINE_AMD64, pe.SUBSYSTEM_GUI))
        self.assertEqual(pe.read_header(image(pe.SUBSYSTEM_CONSOLE))[1], pe.SUBSYSTEM_CONSOLE)

    def test_rejects_what_is_not_a_64_bit_pe(self):
        for data in (b"", b"MZ", b"\x7fELF" + bytes(100), image()[:100], image(magic=0x10B)):
            with self.assertRaises(ValueError):
                pe.read_header(data)
        broken = bytearray(image())
        broken[64:68] = b"XX\0\0"
        with self.assertRaises(ValueError):
            pe.read_header(bytes(broken))

    def test_the_gui_check_reports_the_file_and_the_reason(self):
        with tempfile.TemporaryDirectory() as directory:
            ok, console, arm, text = (os.path.join(directory, n) for n in "abcd")
            for path, data in ((ok, image()), (console, image(pe.SUBSYSTEM_CONSOLE)),
                               (arm, image(machine=0xAA64)), (text, b"hello")):
                pathlib.Path(path).write_bytes(data)
            pe.require_gui_x64(ok)
            for path, reason in ((console, "not Windows GUI"), (arm, "not x86-64"), (text, "not a PE")):
                with self.assertRaises(ValueError) as caught:
                    pe.require_gui_x64(path)
                self.assertIn(reason, str(caught.exception))
            self.assertEqual(pe.main(["pe.py", "gui", ok]), 0)
            self.assertEqual(pe.main(["pe.py", "gui", console]), 1)
            self.assertEqual(pe.main(["pe.py", "gui", os.path.join(directory, "missing")]), 1)
            self.assertEqual(pe.main(["pe.py"]), 2)


FAKE_DOTNET = """#!/usr/bin/env bash
# Stand-in for `dotnet vpk.dll pack`: records the call and lays the payload out like
# Velopack's full package (lib/app/...). FAKE_DROP names a file to leave out.
set -eu
echo called >> "$FAKE_LOG"
while [ $# -gt 0 ]; do
  case "$1" in
    --packDir) dir="$2"; shift 2;;
    --outputDir) out="$2"; shift 2;;
    --packVersion) version="$2"; shift 2;;
    --channel) channel="$2"; shift 2;;
    *) shift;;
  esac
done
python3 - "$dir" "$out" "$version" "$channel" "${FAKE_DROP:-}" <<'PY'
import sys, zipfile, pathlib
directory, out, version, channel, drop = sys.argv[1:6]
with zipfile.ZipFile(f"{out}/BeFeast.Okilum-{version}-{channel}-full.nupkg", "w") as package:
    package.writestr("BeFeast.Okilum.nuspec", "<package/>")
    for path in sorted(pathlib.Path(directory).iterdir()):
        if path.is_file() and path.name != drop:
            package.write(path, "lib/app/" + path.name)
PY
"""


class Pack(unittest.TestCase):
    def run_pack(self, payload_files, drop=""):
        with tempfile.TemporaryDirectory() as root:
            root = pathlib.Path(root)
            tools = root / "tools"
            (tools / "dotnet").mkdir(parents=True)
            (tools / "vpk/tools/net8.0/any").mkdir(parents=True)
            dotnet = tools / "dotnet" / "dotnet"
            dotnet.write_text(FAKE_DOTNET)
            dotnet.chmod(dotnet.stat().st_mode | stat.S_IEXEC)
            payload, output = root / "payload", root / "out"
            payload.mkdir()
            output.mkdir()
            for name, data in payload_files.items():
                (payload / name).write_bytes(data)
            log = root / "calls.log"
            log.write_text("")
            done = subprocess.run(
                ["bash", str(REPO / "scripts/windows/pack.sh"), str(tools), str(payload), str(output)],
                cwd=REPO,
                env={"PATH": os.environ["PATH"], "OKILUM_RELEASE_VERSION": "0.1.7",
                     "FAKE_LOG": str(log), "FAKE_DROP": drop},
                capture_output=True, text=True,
            )
            packages = sorted(p.name for p in output.glob("*.nupkg"))
            return done, log.read_text().count("called"), packages

    def payload(self, **overrides):
        files = {"okilum.exe": image() + b"app", "okilum.ico": b"ico", HELPER: image() + b"helper"}
        files.update(overrides)
        return {name: data for name, data in files.items() if data is not None}

    def test_a_complete_payload_is_packed_and_the_package_is_checked(self):
        done, calls, packages = self.run_pack(self.payload())
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual((calls, packages), (1, ["BeFeast.Okilum-0.1.7-beta-full.nupkg"]))

    def test_a_payload_without_the_helper_is_refused_before_packing(self):
        done, calls, packages = self.run_pack(self.payload(**{HELPER: None}))
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("supervisor", done.stderr)
        self.assertEqual((calls, packages), (0, []))

    def test_a_console_subsystem_helper_is_refused_before_packing(self):
        done, calls, _ = self.run_pack(self.payload(**{HELPER: image(pe.SUBSYSTEM_CONSOLE)}))
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("not Windows GUI", done.stderr)
        self.assertEqual(calls, 0)

    def test_a_package_that_lost_the_helper_fails_the_build(self):
        done, calls, _ = self.run_pack(self.payload(), drop=HELPER)
        self.assertNotEqual(done.returncode, 0)
        self.assertIn(HELPER, done.stderr)
        self.assertEqual(calls, 1)


if __name__ == "__main__":
    unittest.main()
