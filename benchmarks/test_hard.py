import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from xml.etree import ElementTree

import run_hard


class HardBenchmarkTests(unittest.TestCase):
    def test_checksums_reject_failed_or_ambiguous_output(self):
        self.assertEqual(run_hard.parse_result("42\n"), 42)
        for output in ["", "42\n43\n", "nan", "inf", "-1", "log: 42"]:
            with self.subTest(output=output), self.assertRaises(ValueError):
                run_hard.parse_result(output)

    def test_integer_workloads_require_exact_checksums(self):
        run_hard.check_result(92, 92, "queens")
        with self.assertRaises(ValueError):
            run_hard.check_result(92.000001, 92, "queens")

    def test_floating_tolerances_are_limited_to_numeric_kernels(self):
        run_hard.check_result(100.00000001, 100, "fft")
        with self.assertRaises(ValueError):
            run_hard.check_result(101, 100, "fft")
        with self.assertRaises(ValueError):
            run_hard.check_result(math.nan, 100, "matmul")

    def test_reference_oracles_cover_boundary_inputs(self):
        self.assertEqual(run_hard.reference_result("queens", 8), 92)
        self.assertEqual(run_hard.reference_result("dijkstra", 1), 0)
        self.assertEqual(run_hard.reference_result("mandelbrot", 1), 1)
        self.assertAlmostEqual(run_hard.reference_result("matmul", 1), (3 / 97) * (5 / 89))
        self.assertAlmostEqual(run_hard.reference_result("fft", 1), (3 / 97 - 0.5) ** 2)
        values = sorted(run_hard.seeded_values(16))
        expected = sum(value * (index + 1) for index, value in enumerate(values)) % 1_000_000_007
        self.assertEqual(run_hard.reference_result("radix", 16), expected)

    def report(self):
        return {
            "status": "complete", "total_workloads": 2, "profile": "hard",
            "samples": 7, "measured_at": "2026-11-13T00:00:00Z",
            "system": {"cpu": "<script>alert(1)</script> & CPU"},
            "workloads": [
                {"id": workload, "size": 15, "results": {
                    language: {"median_seconds": value * factor / 1000}
                    for language, value in zip(run_hard.LANGUAGES, [2, 1, 4, 3, 5])
                }}
                for workload, factor in [("queens", 1), ("fft", 2)]
            ],
        }

    def test_table_is_valid_svg_with_timings_and_simple_bars(self):
        report = self.report()
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            path = directory / "results.svg"
            run_hard.write_results(report, directory)
            run_hard.render_table(report, path)
            svg = ElementTree.parse(path).getroot()
            namespace = {"svg": "http://www.w3.org/2000/svg"}
            texts = [node.text for node in svg.findall("svg:text", namespace)]
            self.assertIn("2.00", texts)
            self.assertIn("2.00×", texts)
            self.assertIn("N-Queens · 15", texts)
            self.assertEqual(len(svg.findall("svg:text[@class='number best']", namespace)), 2)
            self.assertEqual(len(svg.findall("svg:rect[@height='6']", namespace)), 5)
            self.assertEqual(svg.findall(".//svg:script", namespace), [])
            self.assertIn("&lt;script&gt;", path.read_text())
            self.assertEqual(json.loads((directory / "results.json").read_text()), report)

    def test_smoke_tables_are_visibly_labeled(self):
        report = self.report()
        report["profile"] = "smoke / not for publication"
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "results.svg"
            run_hard.render_table(report, path)
            self.assertIn("not publishable timings", path.read_text())

    def test_rendering_smoke_results_keeps_the_public_table_unchanged(self):
        report = self.report()
        report["profile"] = "smoke / not for publication"
        public_table = run_hard.ROOT / "benchmarks/results.svg"
        previous = public_table.read_bytes() if public_table.exists() else None
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            run_hard.write_results(report, directory)
            subprocess.run([
                sys.executable, "-B", str(run_hard.ROOT / "benchmarks/run_hard.py"),
                "--render-only", str(directory / "results.json"), "--output", str(directory),
            ], check=True, capture_output=True)
            self.assertTrue((directory / "results.svg").is_file())
            self.assertEqual(public_table.read_bytes() if public_table.exists() else None, previous)

    def test_incomplete_or_invalid_runs_do_not_replace_published_tables(self):
        for status, count, timing in [
            ("running", 2, 1), ("complete", 3, 1), ("complete", 2, math.nan),
            ("complete", 2, math.inf), ("complete", 2, 0), ("complete", 2, -1),
        ]:
            report = self.report()
            report["status"] = status
            report["total_workloads"] = count
            report["workloads"][0]["results"]["Neutron"]["median_seconds"] = timing
            with tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary) / "results.svg"
                path.write_text("previous results")
                with self.subTest(status=status, count=count, timing=timing):
                    with self.assertRaises(ValueError):
                        run_hard.render_table(report, path)
                    self.assertEqual(path.read_text(), "previous results")


if __name__ == "__main__":
    unittest.main()
