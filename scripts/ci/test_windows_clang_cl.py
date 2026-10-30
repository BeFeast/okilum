"""Windows cross-build keeps ISA flags on libwebp's dispatched units only."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "windows-clang-cl.py"
SPEC = importlib.util.spec_from_file_location("windows_clang_cl", SCRIPT)
adapter = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(adapter)


class WindowsWebpCompilerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name) / "libwebp-sys-0.9.6"
        self.dsp = self.root / "vendor/src/dsp"
        self.dsp.mkdir(parents=True)
        self.env = {**os.environ, "CARGO_PKG_NAME": "libwebp-sys",
                    "CARGO_PKG_VERSION": "0.9.6", "CARGO_CFG_TARGET_OS": "windows",
                    "CARGO_CFG_TARGET_ARCH": "x86_64", "CARGO_MANIFEST_DIR": str(self.root)}

    def test_only_upstream_sse41_units_receive_flags(self):
        for stem in ("alpha_processing", "dec", "enc", "lossless_enc",
                     "lossless", "upsampling", "yuv"):
            args = ["-c", "--", str(self.dsp / (stem + "_sse41.c"))]
            self.assertEqual(adapter.compiler_args(args, self.env),
                             ["/clang:-msse4.1", *args])
        for source in (self.dsp / "cpu.c", self.dsp / "enc_sse2.c",
                       self.root / "vendor/src/enc/enc_sse41.c"):
            args = ["-c", "--", str(source)]
            self.assertEqual(adapter.compiler_args(args, self.env), args)

    def test_other_packages_targets_and_probes_unchanged(self):
        args = ["-c", str(self.dsp / "enc_sse41.c")]
        for key, value in (("CARGO_PKG_NAME", "other"), ("CARGO_PKG_VERSION", "0.9.7"),
                           ("CARGO_CFG_TARGET_OS", "linux"),
                           ("CARGO_CFG_TARGET_OS", "macos"),
                           ("CARGO_CFG_TARGET_ARCH", "aarch64"),
                           ("CARGO_MANIFEST_DIR", "")):
            self.assertEqual(adapter.compiler_args(args, {**self.env, key: value}), args)
        self.assertEqual(adapter.compiler_args(["--version"], self.env), ["--version"])
        multiple = [*args, str(self.dsp / "cpu.c")]
        self.assertEqual(adapter.compiler_args(multiple, self.env), multiple)

    @unittest.skipUnless(shutil.which("clang-cl"), "clang-cl required for real ISA control")
    def test_real_compiler_ssse3_failure_fixed_without_raising_generic_baseline(self):
        source = self.dsp / "enc_sse41.c"
        # Reproduce the upstream always_inline target mismatch without an SDK.
        source.write_text('typedef short v8hi __attribute__((vector_size(16)));\n'
                          'static __inline__ __attribute__((always_inline,target("ssse3"))) '
                          'v8hi abs_ssse3(v8hi x) { return __builtin_elementwise_abs(x); }\n'
                          'v8hi probe(v8hi x) { return abs_ssse3(x); }\n')
        args = ["/c", "/Fo" + str(self.root / "probe.obj"), "--", str(source)]
        control = subprocess.run(["clang-cl", *args], capture_output=True, text=True)
        self.assertNotEqual(control.returncode, 0)
        self.assertIn("requires target feature 'ssse3'", control.stderr)
        fixed = subprocess.run([str(SCRIPT), *args], env=self.env,
                               capture_output=True, text=True)
        self.assertEqual(fixed.returncode, 0, fixed.stderr)
        generic = self.dsp / "cpu.c"
        generic.write_text('#if defined(__SSSE3__) || defined(__SSE4_1__)\n'
                           '#error generic baseline was raised\n#endif\n'
                           'int generic(void) { return 1; }\n')
        baseline = subprocess.run([str(SCRIPT), "/c", "/Fo" + str(self.root / "base.obj"),
                                   "--", str(generic)], env=self.env,
                                  capture_output=True, text=True)
        self.assertEqual(baseline.returncode, 0, baseline.stderr)


if __name__ == "__main__":
    unittest.main()
