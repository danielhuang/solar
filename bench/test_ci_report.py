"""Exercise report generation with real successful and failing subprocesses."""

import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import ci_report


class ReportTests(unittest.TestCase):
    def run_report(self, failing):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            success = root / "success.py"
            success.write_text(
                "import pathlib, sys\n"
                "pathlib.Path(sys.argv[2]).write_text('[{\"wall\": 1.5}]')\n"
                "print('| Runtime | Wall (s) |')\n"
                "print('| --- | ---: |')\n"
                "print('| Sample | 1.5 |')\n"
            )
            groups = {}
            if failing:
                failure = root / "failure.py"
                failure.write_text(
                    "import sys\nprint('measurement failed', file=sys.stderr)\nsys.exit(7)\n"
                )
                groups["failure"] = ("Failure", str(failure), ["--json", "failed-samples.json"])
            groups["success"] = ("Success", str(success), ["--json", "samples.json"])
            output = root / "results"
            output.mkdir()
            (output / "failed-samples.json").write_text('[{"wall": 0.1}]')
            with patch.object(ci_report, "GROUPS", groups), \
                    patch.object(ci_report, "environment", return_value="Test environment\n"), \
                    patch.object(sys, "argv", ["ci_report.py", "--output-dir", str(output)]), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = ci_report.main()
            report = (output / "report.md").read_text()
            results = json.loads((output / "results.json").read_text())
            self.assertIn("| Sample | 1.5 |", report)
            self.assertIn("[Raw measurements](samples.json)", report)
            self.assertEqual(json.loads((output / "samples.json").read_text()), [{"wall": 1.5}])
            self.assertEqual((output / "environment.txt").read_text(), "Test environment\n")
            self.assertIn("finished_at", results)
            self.assertEqual(results["groups"][-1]["exit_code"], 0)
            if failing:
                self.assertEqual(code, 1)
                self.assertIn("FAILED (7)", report)
                self.assertEqual(results["groups"][0]["exit_code"], 7)
                self.assertEqual(results["groups"][0]["measurements"], [])
                self.assertFalse((output / "failed-samples.json").exists())
                self.assertIn("measurement failed", (output / "failure.log").read_text())
            else:
                self.assertEqual(code, 0)
                self.assertIn("| Success | passed |", report)

    def test_successful_measurements_are_reported(self):
        self.run_report(False)

    def test_failure_keeps_artifacts_and_runs_remaining_groups(self):
        self.run_report(True)


if __name__ == "__main__":
    unittest.main()
