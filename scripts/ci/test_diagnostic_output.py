"""Desktop diagnostics must tolerate pipes closed by a launcher (#2787)."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


class DiagnosticOutputTests(unittest.TestCase):
    def test_shipped_shell_and_core_do_not_use_panicking_print_macros(self):
        pattern = re.compile(r'\b(?:e?println|dbg)\s*!\s*\(')
        self.assertIsNotNone(pattern.search('eprint' + 'ln!("control")'))
        offenders = []
        for crate in ('okilum-shell', 'okilum-core'):
            for path in (ROOT / 'crates' / crate / 'src').rglob('*.rs'):
                if pattern.search(path.read_text()):
                    offenders.append(str(path.relative_to(ROOT)))
        self.assertEqual(offenders, [], 'use the best-effort diagnostic macros')

    def test_both_crates_install_the_shared_macros_before_other_modules(self):
        for relative in ('okilum-core/src/lib.rs', 'okilum-shell/src/main.rs', 'okilum-shell/src/lib.rs'):
            source = (ROOT / 'crates' / relative).read_text()
            self.assertIn('#[macro_use]', source[:source.index('mod diagnostic_io;')])
            self.assertLess(source.index('mod diagnostic_io;'), source.index('pub mod lucide;' if relative.endswith('shell/src/lib.rs') else 'mod about;' if 'shell' in relative else 'pub mod agent_imports;'))
