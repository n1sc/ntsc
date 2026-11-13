import json
import math
from pathlib import Path
import tempfile
import unittest

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

    def test_report_embeds_raw_data_without_a_script_injection(self):
        report = {"label": "</script><script>alert(1)</script>", "status": "running"}
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            run_hard.render_report(report, directory)
            page = (directory / "index.html").read_text()
            self.assertNotIn("__BENCHMARK_DATA__", page)
            self.assertNotIn("</script><script>alert(1)", page)
            self.assertEqual(json.loads((directory / "results.json").read_text()), report)


if __name__ == "__main__":
    unittest.main()
