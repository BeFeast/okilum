"""A missing mutation, a caret redraw or an empty burst cannot pass the collector."""
import json
from pathlib import Path
import tempfile
import unittest

import probe


class ProbeTests(unittest.TestCase):
    def test_only_post_mutation_matching_painted_frames_are_samples(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "diagnostics.log"
            rows = [
                {"phase": "editor_layout_sample", "details": {}},
                {"phase": "editor_latency_frame", "details": {
                    "purpose": "edit", "outcome": "superseded", "monotonic_ns": 100}},
                {"phase": "editor_latency_frame", "details": {
                    "purpose": "open", "outcome": "painted", "monotonic_ns": 200}},
                {"phase": "editor_latency_frame", "details": {
                    "purpose": "edit", "outcome": "painted", "monotonic_ns": 300}},
            ]
            log.write_text("\n".join(map(json.dumps, rows)) + '\n{"partial":')
            self.assertEqual(probe.frame_event(log, "edit", 250), rows[-1]["details"])
            self.assertIsNone(probe.frame_event(log, "edit", 301))
            self.assertIsNone(probe.frame_event(log, "caret"))

    def test_scroll_needs_actual_redraws_inside_the_burst(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "trace.log"
            log.write_text("[frame] t=50 draws=1 draw=999000\n" +
                           "[frame] t=150 draws=0 draw=999000\n")
            with self.assertRaises(probe.NoEvidence):
                probe.draw_samples(log, 100, 200)
            with log.open("a") as stream:
                stream.write("[frame] t=150 draws=1 draw=1500\n" * 5)
            self.assertEqual(probe.draw_samples(log, 100, 200), [1.5] * 5)


if __name__ == "__main__":
    unittest.main()
