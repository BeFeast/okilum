"""The desktop entry must never claim log MIME types (#602 slice 9)."""
import configparser
import unittest
from pathlib import Path

DESKTOP = Path(__file__).with_name('tessera.desktop')
LOG_TYPES = {'text/x-log', 'application/x-ndjson', 'application/jsonl', 'text/x-logfmt'}


class DesktopEntryTest(unittest.TestCase):
    def test_log_types_are_not_claimed(self):
        entry = configparser.ConfigParser(interpolation=None)
        entry.optionxform = str
        entry.read_string(DESKTOP.read_text())
        mime = set(filter(None, entry['Desktop Entry']['MimeType'].split(';')))
        # Positive control: the parsed list is the real one.
        self.assertIn('text/markdown', mime)
        self.assertFalse(mime & LOG_TYPES, mime & LOG_TYPES)
        self.assertIn('%f', entry['Desktop Entry']['Exec'])


if __name__ == '__main__':
    unittest.main()
