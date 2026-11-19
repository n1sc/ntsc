import importlib.util
import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from xml.etree import ElementTree

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import run


def measured_report():
    suites = []
    for identifier, languages, count in [
        ("native", run.LANGUAGES, 6),
        ("concurrency", ["Neutron", "Go"], 11),
        ("legacy", ["Neutron", "Rust"], 4),
    ]:
        suites.append({
            "id": identifier, "status": "complete", "profile": "full",
            "samples": 3, "warmup": 1, "total_workloads": count, "languages": languages,
            "workloads": [{
                "id": f"{identifier}_{index}", "title": f"Fixture {index}",
                "oracle_verified": True,
                "results": {language: {
                    "seconds": [0.001 * factor, 0.002 * factor, 0.003 * factor],
                    "median_seconds": 0.002 * factor,
                } for factor, language in enumerate(languages, 1)},
            } for index in range(count)],
        })
    return {
        "status": "complete", "profile": "full", "samples": 3, "warmup": 1,
        "selected_suites": list(run.SUITES), "suites": suites,
        "workers": 4, "system": {"cpu": "Test CPU"},
        "measured_at": "2026-11-18T00:00:00Z", "git_revision": "12345678",
    }


class CombinedRunnerTests(unittest.TestCase):
    def test_suite_selection_is_deduplicated_and_ordered(self):
        self.assertEqual(run.selected_suites(None), list(run.SUITES))
        self.assertEqual(run.selected_suites(["legacy", "all"]), list(run.SUITES))
        self.assertEqual(run.selected_suites(["legacy", "native", "legacy"]), ["native", "legacy"])

    def test_legacy_oracles_match_the_original_workload_inputs(self):
        expected = {name: checksum for name, _, _, checksum in run.legacy_cases()}
        self.assertEqual(expected["fibonacci"], 9227465)
        self.assertEqual(expected["primes"], 168)
        self.assertEqual(expected["matrix"], 326922000)
        self.assertEqual(expected["loops"], 9999925000)

    def test_prime_oracle_boundary_inputs(self):
        self.assertEqual([run.prime_count(n) for n in range(6)], [0, 0, 0, 1, 2, 2])
        self.assertEqual(run.prime_count(400000), 33860)

    def test_spawn_checksums_match_the_arithmetic_sent_by_both_languages(self):
        cases = run.concurrency_cases(4, False)
        self.assertEqual(len(cases), 11)
        self.assertEqual(cases[0][3], 10000 * sum(range(64)))
        self.assertEqual(cases[1][3], 100000 * sum(range(64)))
        self.assertEqual(cases[-1][3], 512 * 505)
        self.assertEqual(len(run.concurrency_cases(1, False)), 10)
        self.assertEqual(len(run.concurrency_cases(4, True)), 11)

    def test_checksums_are_full_integers_not_truncated_exit_codes(self):
        self.assertEqual(run.parse_checksum("201600000\n"), 201600000)
        for output in ["", "123\n456", "nan", "12.5"]:
            with self.subTest(output=output), self.assertRaises(ValueError):
                run.parse_checksum(output)

    def test_wall_clock_runs_require_success_and_matching_checksum(self):
        with mock.patch.object(run.subprocess, "run") as execute:
            execute.return_value = subprocess.CompletedProcess(["fixture"], 0, "2016\n", "")
            self.assertGreater(run.run_checked(Path("fixture"), {}, 2016, 10), 0)
            self.assertTrue(execute.call_args.kwargs["check"])
            with self.assertRaises(ValueError):
                run.run_checked(Path("fixture"), {}, 1, 10)
            execute.side_effect = subprocess.CalledProcessError(1, "fixture", output="2016\n")
            with self.assertRaises(subprocess.CalledProcessError):
                run.run_checked(Path("fixture"), {}, 2016, 10)

    @unittest.skipUnless(hasattr(run.os, "sched_getaffinity"), "CPU affinity is Linux-only")
    def test_affinity_is_restored_even_when_a_benchmark_fails(self):
        with mock.patch.object(run.os, "sched_getaffinity", return_value={2, 3}), \
                mock.patch.object(run.os, "sched_setaffinity") as set_affinity:
            with self.assertRaises(RuntimeError):
                with run.affinity([2]):
                    raise RuntimeError("fixture failed")
            self.assertEqual(set_affinity.call_args_list, [mock.call(0, {2}), mock.call(0, {2, 3})])

    def test_report_json_round_trips_and_disallows_nonfinite_values(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            run.write_json(measured_report(), directory)
            self.assertEqual(json.loads((directory / "results.json").read_text()), measured_report())
            with self.assertRaises(ValueError):
                run.write_json({"value": math.nan}, directory)
            self.assertEqual(json.loads((directory / "results.json").read_text()), measured_report())

    def test_one_runner_exposes_all_suite_and_report_options(self):
        self.assertEqual(sorted(path.name for path in (run.ROOT / "benchmarks").glob("*.py")),
                         ["run.py"])
        result = subprocess.run(
            [sys.executable, "-B", str(run.ROOT / "benchmarks/run.py"), "--help"],
            text=True, capture_output=True, check=True,
        )
        for option in ["--suite", "--iterations", "--render-only", "--pdf"]:
            self.assertIn(option, result.stdout)


class ScorecardTests(unittest.TestCase):
    def test_default_pdf_is_published_inside_benchmarks(self):
        self.assertEqual(run.PUBLIC_PDF, run.ROOT / "benchmarks/neutron-benchmarks.pdf")
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "results.json"
            measured = measured_report()
            source.write_text(json.dumps(measured))
            with mock.patch.object(sys, "argv", ["run.py", "--render-only", str(source)]), \
                    mock.patch.object(run, "render_pdf") as render:
                run.main()
            render.assert_called_once_with(measured, run.PUBLIC_PDF)

    def test_smoke_render_uses_a_local_pdf_and_cannot_replace_published_results(self):
        measured = measured_report()
        measured["profile"] = "smoke / not for publication"
        for suite in measured["suites"]:
            suite["profile"] = measured["profile"]
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "results.json"
            source.write_text(json.dumps(measured))
            arguments = ["run.py", "--render-only", str(source)]
            with mock.patch.object(sys, "argv", arguments), \
                    mock.patch.object(run, "render_pdf") as render:
                run.main()
            render.assert_called_once_with(measured, source.parent / "scorecard.pdf")
            with mock.patch.object(sys, "argv", [*arguments, "--pdf", str(run.PUBLIC_PDF)]), \
                    mock.patch.object(run, "render_pdf") as render:
                with self.assertRaises(SystemExit) as error:
                    run.main()
            self.assertEqual(error.exception.code, 2)
            render.assert_not_called()

    def test_validated_complete_report_and_geometric_mean(self):
        measured = measured_report()
        run.validate_report(measured)
        ratios = run.relative_times(measured["suites"][0])
        for factor, language in enumerate(run.LANGUAGES, 1):
            self.assertAlmostEqual(ratios[language], factor)

    def test_incomplete_mixed_or_unvalidated_reports_are_rejected(self):
        modifications = [
            lambda value: value.update(status="running"),
            lambda value: value["suites"].pop(),
            lambda value: value["suites"][0].update(status="running"),
            lambda value: value["suites"][0].update(profile="smoke / not for publication"),
            lambda value: value["suites"][0].update(total_workloads=7),
            lambda value: value["suites"][0]["workloads"][0].update(oracle_verified=False),
            lambda value: value["suites"][0]["workloads"][1].update(id="native_0"),
            lambda value: value["suites"][0]["workloads"][0]["results"].pop("Go"),
        ]
        for modify in modifications:
            measured = measured_report()
            modify(measured)
            with self.subTest(modify=modify), self.assertRaises(ValueError):
                run.validate_report(measured)

    def test_invalid_samples_and_invented_medians_are_rejected(self):
        for values, median in [
            ([0, 1, 2], 1), ([1, 2], 1.5), ([1, 2, math.inf], 2),
            ([1, math.nan, 3], 2), ([-1, 2, 3], 2), ([1, 2, 3], 3),
            ([1, 2, 3], math.nan),
        ]:
            measured = measured_report()
            measured["suites"][0]["workloads"][0]["results"]["Neutron"].update(
                seconds=values, median_seconds=median)
            with self.subTest(values=values, median=median), self.assertRaises(ValueError):
                run.validate_report(measured)

    @unittest.skipUnless(importlib.util.find_spec("reportlab") and importlib.util.find_spec("pypdf"),
                         "install benchmarks/requirements.txt for PDF tests")
    def test_all_suites_fit_two_pages_and_smoke_is_visibly_labeled(self):
        from pypdf import PdfReader

        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "scorecard.pdf"
            for smoke in [False, True]:
                measured = measured_report()
                if smoke:
                    measured["profile"] = "smoke / not for publication"
                    for suite in measured["suites"]:
                        suite["profile"] = measured["profile"]
                run.render_pdf(measured, output)
                reader = PdfReader(output)
                self.assertEqual(len(reader.pages), 2)
                text = "\n".join(page.extract_text() for page in reader.pages)
                self.assertIn("2.00", text)
                self.assertIn("1.00x", text)
                self.assertIn("Fixed inputs".upper(), text)
                self.assertEqual("SMOKE / NOT PUBLICATION" in text, smoke)
                self.assertNotIn("nan", text)

    def test_invalid_runs_never_replace_an_existing_pdf(self):
        measured = measured_report()
        measured["status"] = "running"
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "scorecard.pdf"
            output.write_bytes(b"previous valid PDF")
            with self.assertRaises(ValueError):
                run.render_pdf(measured, output)
            self.assertEqual(output.read_bytes(), b"previous valid PDF")



class HardBenchmarkTests(unittest.TestCase):
    def test_checksums_reject_failed_or_ambiguous_output(self):
        self.assertEqual(run.parse_result("42\n"), 42)
        for output in ["", "42\n43\n", "nan", "inf", "-1", "log: 42"]:
            with self.subTest(output=output), self.assertRaises(ValueError):
                run.parse_result(output)

    def test_integer_workloads_require_exact_checksums(self):
        run.check_result(92, 92, "queens")
        with self.assertRaises(ValueError):
            run.check_result(92.000001, 92, "queens")

    def test_floating_tolerances_are_limited_to_numeric_kernels(self):
        run.check_result(100.00000001, 100, "fft")
        with self.assertRaises(ValueError):
            run.check_result(101, 100, "fft")
        with self.assertRaises(ValueError):
            run.check_result(math.nan, 100, "matmul")

    def test_reference_oracles_cover_boundary_inputs(self):
        self.assertEqual(run.reference_result("queens", 8), 92)
        self.assertEqual(run.reference_result("dijkstra", 1), 0)
        self.assertEqual(run.reference_result("mandelbrot", 1), 1)
        self.assertAlmostEqual(run.reference_result("matmul", 1), (3 / 97) * (5 / 89))
        self.assertAlmostEqual(run.reference_result("fft", 1), (3 / 97 - 0.5) ** 2)
        values = sorted(run.seeded_values(16))
        expected = sum(value * (index + 1) for index, value in enumerate(values)) % 1_000_000_007
        self.assertEqual(run.reference_result("radix", 16), expected)

    def report(self):
        return {
            "status": "complete", "total_workloads": 2, "profile": "hard",
            "samples": 7, "measured_at": "2026-11-13T00:00:00Z",
            "system": {"cpu": "<script>alert(1)</script> & CPU"},
            "workloads": [
                {"id": workload, "size": 15, "results": {
                    language: {"median_seconds": value * factor / 1000}
                    for language, value in zip(run.LANGUAGES, [2, 1, 4, 3, 5])
                }}
                for workload, factor in [("queens", 1), ("fft", 2)]
            ],
        }

    def test_table_is_valid_svg_with_timings_and_simple_bars(self):
        report = self.report()
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            path = directory / "results.svg"
            run.write_json(report, directory)
            run.render_table(report, path)
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
            run.render_table(report, path)
            self.assertIn("not publishable timings", path.read_text())

    def test_rendering_smoke_results_keeps_the_public_table_unchanged(self):
        report = self.report()
        report["profile"] = "smoke / not for publication"
        public_table = run.ROOT / "benchmarks/results.svg"
        previous = public_table.read_bytes() if public_table.exists() else None
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            run.write_json(report, directory)
            subprocess.run([
                sys.executable, "-B", str(run.ROOT / "benchmarks/run.py"),
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
                        run.render_table(report, path)
                    self.assertEqual(path.read_text(), "previous results")


if __name__ == "__main__":
    unittest.main()
