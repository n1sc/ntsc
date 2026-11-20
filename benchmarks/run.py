#!/usr/bin/env python3
"""One runner for native kernels, concurrency, and legacy process fixtures."""

import argparse
from contextlib import contextmanager
import datetime
import hashlib
import heapq
import importlib.util
import json
import math
import os
from pathlib import Path
import platform
import random
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from xml.etree import ElementTree


ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "target/benchmarks"
NATIVE_BUILD = BUILD / "native-build"
NATIVE_SOURCES = ROOT / "benchmarks/hard"
PUBLIC_PDF = ROOT / "benchmarks/neutron-benchmarks.pdf"
SUITES = ("native", "concurrency", "legacy")
LEGACY = ("fibonacci", "primes", "matrix", "loops")
CONCURRENCY = ("goroutine_spawn", "cpu_fanout", "io_mixed", "http_fanout")

LANGUAGES = ["Neutron", "C · GCC", "C · Clang", "Go", "Rust"]
WORKLOADS = [
    ("queens", "Exact N-Queens", "Combinatorial search", "Exhaustive bitboard backtracking; every solution, no symmetry shortcuts.", 15, 8),
    ("mandelbrot", "Mandelbrot renderer", "Branch-heavy floating point", "A complete square image, up to 256 escape iterations per pixel.", 1024, 32),
    ("matmul", "Blocked matrix product", "Dense numerical computing", "Row-major f64 A×B, 32×32 tiles; actual buffers, no BLAS or formula shortcut.", 192, 16),
    ("radix", "Million-key radix sort", "Memory / data processing", "Stable four-pass radix sort of seeded 31-bit keys, including buffers and sortedness verification.", 1_000_000, 1024),
    ("dijkstra", "Sparse-graph shortest paths", "Graph / priority queue", "Materialized directed graph with eight weighted edges per vertex; binary min-heap with stale-entry rejection.", 60_000, 128),
    ("fft", "Radix-2 complex FFT", "Signal processing", "In-place Cooley–Tukey transform, bit reversal and complex butterflies; energy checksum.", 262_144, 1024),
]



def command_output(command, **kwargs):
    result = subprocess.run(command, text=True, capture_output=True, check=True, **kwargs)
    return result.stdout.strip()


def find_prebuilt_binary(compiler, name):
    exe = ".exe" if sys.platform == "win32" else ""
    filename_map = {
        "C · GCC": "gcc",
        "C · Clang": "clang",
        "Go": "go",
        "Rust": "rust",
    }
    base = filename_map.get(name)
    if not base:
        return None
    comp = compiler.resolve() if isinstance(compiler, Path) else Path(compiler).resolve()
    candidates = [
        ROOT / "benchmarks/prebuilt" / f"{base}{exe}",
        ROOT / "prebuilt" / f"{base}{exe}",
        comp.parent / "prebuilt" / f"{base}{exe}",
        comp.parent.parent / "prebuilt" / f"{base}{exe}",
    ]
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    return None


def build_binaries(compiler, languages=None):
    if languages is None:
        languages = LANGUAGES
    NATIVE_BUILD.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(GOCACHE=str(NATIVE_BUILD / "go-cache"), GO111MODULE="off")
    exe = ".exe" if sys.platform == "win32" else ""
    builds = {
        "C · GCC": ["gcc", "-O3", "-std=c11", "-ffp-contract=off", str(NATIVE_SOURCES / "kernels.c"), "-lm", "-o", str(NATIVE_BUILD / f"gcc{exe}")],
        "C · Clang": ["clang", "-O3", "-std=c11", "-ffp-contract=off", str(NATIVE_SOURCES / "kernels.c"), "-lm", "-o", str(NATIVE_BUILD / f"clang{exe}")],
        "Go": ["go", "build", "-trimpath", "-o", str(NATIVE_BUILD / f"go{exe}"), str(NATIVE_SOURCES / "kernels.go")],
        "Rust": ["rustc", "--edition=2024", "-C", "opt-level=3", "-C", "codegen-units=1", "-C", "panic=abort", str(NATIVE_SOURCES / "kernels.rs"), "-o", str(NATIVE_BUILD / f"rust{exe}")],
    }
    project = NATIVE_BUILD / "neutron"
    (project / "src").mkdir(parents=True, exist_ok=True)
    shutil.copyfile(NATIVE_SOURCES / "kernels.nt", project / "src/main.nt")
    try:
        triple = command_output(["rustc", "-vV"]).split("host: ", 1)[1].splitlines()[0]
    except (subprocess.SubprocessError, FileNotFoundError):
        triple = "x86_64-pc-windows-gnu" if sys.platform == "win32" else "x86_64-unknown-linux-gnu"
    ffi_config = '\n[ffi]\nlibraries = ["synchronization", "gcc_eh", "gcc_s"]\n' if sys.platform == "win32" else ""
    (project / "neutron.toml").write_text(
        f'[package]\ntarget = "{triple}"\nentry = "src/main.nt"\noutput = "kernels"\n{ffi_config}'
    )
    builds["Neutron"] = [str(compiler), "build", "--release"]
    paths = {name: NATIVE_BUILD / f"{filename}{exe}" for name, filename in [
        ("C · GCC", "gcc"), ("C · Clang", "clang"), ("Go", "go"), ("Rust", "rust"),
    ]}
    paths["Neutron"] = project / f"build/release/kernels{exe}"
    records = {}
    tool_map = {"C · GCC": "gcc", "C · Clang": "clang", "Go": "go", "Rust": "rustc"}
    for name in languages:
        prebuilt = find_prebuilt_binary(compiler, name) if name != "Neutron" else None
        tool = tool_map.get(name)
        if name != "Neutron" and prebuilt and (not tool or not shutil.which(tool)):
            print(f"Using prebuilt {name} binary ({prebuilt.name})...", flush=True)
            shutil.copyfile(prebuilt, paths[name])
            records[name] = {
                "command": ["prebuilt", str(prebuilt)],
                "build_seconds": 0.0,
                "binary_bytes": paths[name].stat().st_size,
            }
            continue

        print(f"Building {name}...", flush=True)
        started = time.perf_counter()
        cargo_toml = ROOT / "Cargo.toml"
        renamed_cargo = False
        if name == "Neutron" and cargo_toml.is_file() and shutil.which("cargo") is None:
            try:
                cargo_toml.rename(ROOT / "Cargo.toml.tmp")
                renamed_cargo = True
            except OSError:
                pass
        try:
            result = subprocess.run(
                builds[name], cwd=project if name == "Neutron" else ROOT,
                env=environment, capture_output=True, text=True, timeout=600,
            )
        finally:
            if renamed_cargo:
                try:
                    (ROOT / "Cargo.toml.tmp").rename(cargo_toml)
                except OSError:
                    pass
        (NATIVE_BUILD / f"{paths[name].name}-build.log").write_text(result.stdout + result.stderr)
        if result.returncode:
            raise RuntimeError(f"{name} compilation failed:\n{result.stdout}\n{result.stderr}")
        records[name] = {
            "command": builds[name], "build_seconds": time.perf_counter() - started,
            "binary_bytes": paths[name].stat().st_size,
        }
    return paths, records, environment


def parse_result(output):
    lines = output.strip().splitlines()
    if len(lines) != 1:
        raise ValueError(f"expected one numeric checksum, got {output!r}")
    value = float(lines[0])
    if not math.isfinite(value) or value < 0:
        raise ValueError(f"invalid checksum: {output!r}")
    return value


def check_result(actual, expected, workload):
    tolerance = 1e-8 if workload == "fft" else 1e-10 if workload == "matmul" else 0
    if not math.isclose(actual, expected, rel_tol=tolerance, abs_tol=tolerance):
        raise ValueError(f"{workload}: checksum mismatch: {actual} != {expected}")


def seeded_values(n):
    state = 123456789
    for _ in range(n):
        state = (state * 1664525 + 1013904223) & 2147483647
        yield state


def reference_result(workload, n):
    if workload == "queens":
        return {8: 92, 15: 2_279_184}[n]
    if workload == "radix":
        return sum(value * (i + 1) for i, value in enumerate(sorted(seeded_values(n)))) % 1_000_000_007
    if workload == "fft":
        return n * sum((((i * 17 + 3) % 97) / 97.0 - 0.5) ** 2 for i in range(n))
    if workload == "matmul":
        # Sum(A×B) = sum_k(column_sum(A,k) * row_sum(B,k)).
        return sum(
            sum(((row * n + k) * 17 + 3) % 97 / 97.0 for row in range(n))
            * sum(((k * n + col) * 7 + 5) % 89 / 89.0 for col in range(n))
            for k in range(n)
        )
    if workload == "mandelbrot":
        total = 0
        for y in range(n):
            cy = y * 2.4 / n - 1.2
            for x in range(n):
                cx = x * 3.2 / n - 2.2
                real = imag = 0.0
                iteration = 0
                while iteration < 256 and real * real + imag * imag <= 4.0:
                    next_real = real * real - imag * imag + cx
                    imag = 2.0 * real * imag + cy
                    real = next_real
                    iteration += 1
                total += iteration
        return total
    if workload == "dijkstra":
        state = 123456789
        graph = []
        for vertex in range(n):
            edges = []
            for edge in range(8):
                state = (state * 1664525 + 1013904223) & 2147483647
                destination = (vertex + 1) % n if edge == 0 else state % n
                edges.append((destination, 1 + ((state >> 8) & 31)))
            graph.append(edges)
        distances = [10**12] * n
        distances[0] = 0
        heap = [(0, 0)]
        while heap:
            cost, node = heapq.heappop(heap)
            if cost != distances[node]:
                continue
            for destination, weight in graph[node]:
                candidate = cost + weight
                if candidate < distances[destination]:
                    distances[destination] = candidate
                    heapq.heappush(heap, (candidate, destination))
        return sum(distance * (vertex + 1) for vertex, distance in enumerate(distances)) % 1_000_000_007
    raise ValueError(f"unknown workload: {workload}")


def run_binary(binary, workload, size, environment, timeout):
    started = time.perf_counter_ns()
    result = subprocess.run(
        [str(binary), workload, str(size)], env=environment,
        capture_output=True, text=True, timeout=timeout, check=True,
    )
    elapsed = (time.perf_counter_ns() - started) / 1e9
    return elapsed, parse_result(result.stdout)


def render_table(report, output):
    workloads = report["workloads"]
    if report["status"] != "complete" or not workloads or len(workloads) != report["total_workloads"]:
        raise ValueError("only complete benchmark runs can be published")
    languages = report.get("languages", LANGUAGES)
    medians = [[workload["results"][language]["median_seconds"] for language in languages]
               for workload in workloads]
    if any(not math.isfinite(value) or value <= 0 for row in medians for value in row):
        raise ValueError("table timings must be finite and positive")
    height = 172 + 30 * len(workloads)
    svg = ElementTree.Element("svg", {
        "xmlns": "http://www.w3.org/2000/svg", "width": "760", "height": str(height),
        "viewBox": f"0 0 760 {height}", "role": "img", "aria-labelledby": "title description",
    })
    smoke = "smoke" in report["profile"]
    title = "Benchmark smoke test" if smoke else "Native kernel benchmarks"
    ElementTree.SubElement(svg, "title", id="title").text = title
    ElementTree.SubElement(svg, "desc", id="description").text = (
        "Median wall time in milliseconds; lower is better. "
        "Green cells mark the fastest result. Bars show geometric-mean slowdown "
        "relative to the fastest toolchain in each workload; all workloads are included."
    )
    ElementTree.SubElement(svg, "style").text = (
        "text{font-family:system-ui,-apple-system,Segoe UI,sans-serif;fill:#334155;font-size:13px}"
        ".heading{font-size:19px;font-weight:700;fill:#0f172a}"
        ".muted{font-size:11px;fill:#64748b}.header{font-weight:600}"
        ".best{fill:#047857;font-weight:700}.number{font-variant-numeric:tabular-nums}"
    )
    ElementTree.SubElement(svg, "rect", width="760", height=str(height), rx="12", fill="#ffffff")

    def text(x, y, value, style="", anchor="start"):
        ElementTree.SubElement(svg, "text", {
            "x": str(x), "y": str(y), "class": style, "text-anchor": anchor,
        }).text = str(value)

    text(24, 29, title, "heading")
    text(24, 49, "Small correctness inputs · not publishable timings" if smoke
         else "Median milliseconds · lower is better · green = fastest", "muted")
    aliases = {"C · GCC": "C / GCC", "C · Clang": "C / Clang"}
    labels = [aliases.get(l, l) for l in languages]
    col_width = min(104, 480 // max(len(languages), 1))
    centers = [266 + col_width * index for index in range(len(languages))]
    text(24, 79, "Workload / size", "header")
    for center, label in zip(centers, labels):
        text(center, 79, label, "header", "middle")
    short_titles = {
        "queens": "N-Queens", "mandelbrot": "Mandelbrot", "matmul": "Matrix product",
        "radix": "Radix sort", "dijkstra": "Dijkstra", "fft": "FFT",
    }
    for index, (workload, row) in enumerate(zip(workloads, medians)):
        y = 92 + 30 * index
        ElementTree.SubElement(svg, "rect", x="16", y=str(y), width="728", height="30",
                               fill="#f8fafc" if index % 2 == 0 else "#ffffff")
        text(24, y + 20, f'{short_titles[workload["id"]]} · {workload["size"]:,}')
        fastest = min(row)
        for center, value in zip(centers, row):
            text(center, y + 20, f"{value * 1000:,.2f}",
                 "number best" if value == fastest else "number", "middle")
    slowdowns = [
        math.exp(statistics.mean(math.log(row[column] / min(row)) for row in medians))
        for column in range(len(languages))
    ]
    y = 108 + 30 * len(workloads)
    text(24, y, "Overall slowdown", "header")
    text(24, y + 17, "Fastest per workload = 1×", "muted")
    for index, (center, slowdown) in enumerate(zip(centers, slowdowns)):
        text(center, y, f"{slowdown:.2f}×", "number", "middle")
        bar_w = min(col_width - 20, 84)
        ElementTree.SubElement(svg, "rect", x=str(center - bar_w / 2), y=str(y + 9),
                               width=f"{bar_w * slowdown / max(slowdowns):.2f}", height="6", rx="3",
                               fill="#2563eb" if index == 0 else "#94a3b8")
    cpu = report["system"]["cpu"].replace("(R)", "").replace("(TM)", "")
    cpu = cpu.split(" CPU", 1)[0]
    text(24, height - 16,
         f'{cpu} · single core · {report["samples"]} samples · {report["measured_at"][:10]}', "muted")
    ElementTree.indent(svg, space="  ")
    output.parent.mkdir(parents=True, exist_ok=True)
    ElementTree.ElementTree(svg).write(output, encoding="utf-8", xml_declaration=True)


def validate_report(report):
    if report.get("status") != "complete" or not report.get("suites"):
        raise ValueError("only complete benchmark runs can be rendered")
    if report.get("samples", 0) < 3 or report.get("warmup", -1) < 0:
        raise ValueError("invalid sampling configuration")
    if set(report.get("selected_suites", [])) != {suite["id"] for suite in report["suites"]}:
        raise ValueError("the requested suites did not all complete")
    identifiers = set()
    for suite in report["suites"]:
        identifier = suite["id"]
        if identifier not in {"native", "concurrency", "legacy"} or identifier in identifiers:
            raise ValueError("unknown or duplicate suite")
        identifiers.add(identifier)
        languages = suite["languages"]
        workloads = suite["workloads"]
        if (
            suite["status"] != "complete"
            or not languages
            or len(languages) != len(set(languages))
            or not workloads
            or len(workloads) != suite["total_workloads"]
            or suite["samples"] != report["samples"]
            or suite["warmup"] != report["warmup"]
            or ("smoke" in suite["profile"]) != ("smoke" in report["profile"])
        ):
            raise ValueError("incomplete or inconsistent suite")
        seen = set()
        for workload in workloads:
            if workload["id"] in seen or not workload.get("oracle_verified"):
                raise ValueError("duplicate or unvalidated workload")
            seen.add(workload["id"])
            if set(workload["results"]) != set(languages):
                raise ValueError("missing or unexpected toolchain")
            for result in workload["results"].values():
                samples = result["seconds"]
                median = result["median_seconds"]
                if (
                    len(samples) != report["samples"]
                    or any(not math.isfinite(value) or value <= 0 for value in samples)
                    or not math.isfinite(median)
                    or median <= 0
                    or not math.isclose(median, statistics.median(samples), rel_tol=1e-12)
                ):
                    raise ValueError("invalid timings or median")


def relative_times(suite):
    """Geometric-mean time relative to the fastest toolchain in each row."""
    return {
        language: math.exp(statistics.mean(
            math.log(workload["results"][language]["median_seconds"]
                     / min(result["median_seconds"] for result in workload["results"].values()))
            for workload in suite["workloads"]
        ))
        for language in suite["languages"]
    }


def render_pdf(report, output):
    validate_report(report)
    try:
        from reportlab.lib import colors
        from reportlab.lib.pagesizes import A4
        from reportlab.pdfbase import pdfmetrics
        from reportlab.pdfbase.ttfonts import TTFont
        from reportlab.pdfgen import canvas
        import reportlab
    except ImportError as error:
        raise RuntimeError(
            "PDF output requires: python3 -m pip install -r benchmarks/requirements.txt"
        ) from error

    fonts = Path(reportlab.__file__).parent / "fonts"
    pdfmetrics.registerFont(TTFont("Sheet", str(fonts / "Vera.ttf")))
    pdfmetrics.registerFont(TTFont("SheetBold", str(fonts / "VeraBd.ttf")))
    mono = next((path for path in [
        Path("/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf"),
        Path("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"),
    ] if path.is_file()), None)
    if mono:
        pdfmetrics.registerFont(TTFont("SheetMono", str(mono)))
    number_font = "SheetMono" if mono else "Courier"

    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=output.parent, suffix=".pdf", delete=False) as temporary:
        temporary_path = Path(temporary.name)
    width, height = A4
    margin = 40
    content_width = width - 2 * margin
    ink = colors.HexColor("#20221F")
    muted = colors.HexColor("#777970")
    accent = colors.HexColor("#B83B21")
    paper = colors.HexColor("#F9F8F4")
    tint = colors.HexColor("#F0ECE4")
    rule = colors.HexColor("#DCDDD4")
    aliases = {"C · GCC": "GCC / C", "C · Clang": "Clang / C"}

    try:
        document = canvas.Canvas(str(temporary_path), pagesize=A4, pageCompression=1)
        document.setTitle("Neutron | Benchmark report")
        document.setAuthor("Neutron benchmark suite")
        document.setSubject("Validated median wall-time measurements")

        def text(x, top, value, size=9, color=ink, font="Helvetica", align="left", limit=None):
            value = str(value)
            if limit and pdfmetrics.stringWidth(value, font, size) > limit:
                raise ValueError(f"scorecard label is too wide: {value}")
            document.setFont(font, size)
            document.setFillColor(color)
            draw = {"left": document.drawString, "right": document.drawRightString,
                    "center": document.drawCentredString}[align]
            draw(x, height - top, value)

        def rectangle(x, top, rectangle_width, rectangle_height, color):
            document.setFillColor(color)
            document.rect(x, height - top - rectangle_height, rectangle_width,
                          rectangle_height, stroke=0, fill=1)

        def line(top, left=margin, right=width - margin, color=rule, weight=0.6):
            document.setStrokeColor(color)
            document.setLineWidth(weight)
            document.line(left, height - top, right, height - top)

        def label(x, top, value, align="left", color=muted):
            text(x, top, value, 7.5, color, "Helvetica-Bold", align)

        def page(title, subtitle, page_number, page_count, facts):
            rectangle(0, 0, width, height, paper)
            text(margin, 43, "N /", 17, accent, "Helvetica-Bold")
            label(margin + 42, 42, "NEUTRON    /    BENCHMARK REPORT", color=ink)
            label(width - margin, 42, report["measured_at"][:10], "right")
            line(62, color=ink, weight=0.8)
            text(margin, 112, title[0], 46, ink, "Helvetica-Bold")
            text(margin, 155, title[1], 46, ink, "Helvetica-Bold")
            for index, (number, caption) in enumerate(facts):
                top = 96 + index * 25
                text(width - margin - 78, top, number, 17, accent, number_font)
                label(width - margin, top - 1, caption, "right")
            text(margin, 181, subtitle, 9, muted)
            smoke = "smoke" in report["profile"]
            badge = "SMOKE / NOT PUBLICATION" if smoke else "CHECKSUM VERIFIED"
            label(margin, 201, f'{report["samples"]} SAMPLES   /   {report["warmup"]} WARMUPS   /   RELEASE')
            label(width - margin, 201, badge, "right", accent)
            line(height - 57, color=ink, weight=0.8)
            cpu = report["system"]["cpu"].replace("(R)", "").replace("(TM)", "")
            text(margin, height - 39, cpu, 7.5, muted, limit=content_width - 100)
            text(width - margin, height - 36, f"{page_number:02d} / {page_count:02d}",
                 12, ink, number_font, "right")
            revision = report["git_revision"][:8]
            if report.get("git_worktree_dirty"):
                revision += " + local changes"
            text(margin, height - 24, revision + "   /   compilation excluded", 7, muted)

        def table(suite, top, heading, note, row_height=28):
            languages = suite["languages"]
            workloads = suite["workloads"]
            label_width = 150 if len(languages) > 2 else 265
            column_width = (content_width - label_width) / len(languages)
            start = margin + label_width
            text(margin, top + 12, heading, 11, ink, "Helvetica-Bold")
            label(width - margin, top + 11, note, "right")
            row_start = top + 48
            if "Neutron" in languages:
                neutron_index = languages.index("Neutron")
                rectangle(start + neutron_index * column_width, top + 25,
                          column_width, 23 + row_height * len(workloads), tint)
            label(margin, top + 40, "WORKLOAD / INPUT")
            for index, language in enumerate(languages):
                x = start + column_width * (index + 1) - 9
                text(x, top + 40, aliases.get(language, language), 8.5,
                     ink, "Helvetica-Bold", "right")
            line(row_start, color=ink, weight=0.8)
            for row, workload in enumerate(workloads):
                row_top = row_start + row * row_height
                baseline = row_top + row_height / 2 + 3.5
                title = workload.get("sheet_label", workload["title"])
                if row_height >= 38 and " / " in title:
                    name, input_size = title.rsplit(" / ", 1)
                    text(margin, row_top + 17, name, 10, ink, "Helvetica-Bold", limit=label_width - 10)
                    text(margin, row_top + 31, "INPUT  " + input_size, 7.5, muted)
                else:
                    title = title.replace(" / 1 tasks", " / 1 task")
                    text(margin, baseline, title, 9, ink, limit=label_width - 10)
                fastest = min(value["median_seconds"] for value in workload["results"].values())
                for index, language in enumerate(languages):
                    value = workload["results"][language]["median_seconds"]
                    milliseconds = value * 1000
                    formatted = f"{milliseconds:,.2f}"
                    x = start + column_width * (index + 1) - 9
                    text(x, baseline, formatted, 9.5, accent if value == fastest else ink,
                         number_font, "right")
                line(row_top + row_height)
            return row_start + row_height * len(workloads)

        suites = {suite["id"]: suite for suite in report["suites"]}
        page_count = int("native" in suites) + int(bool(suites.keys() - {"native"}))
        page_number = 1
        if "native" in suites:
            suite = suites["native"]
            page(("Native", "performance"), "Median wall time in milliseconds. Lower is better.",
                 page_number, page_count,
                 [(f'{len(suite["workloads"]):02d}', "KERNELS"),
                  (f'{len(suite["languages"]):02d}', "TOOLS"), ("01", "CPU")])
            bottom = table(suite, 225, "01    /    Kernel timings", "COLOR = FASTEST", 40)
            top = bottom + 38
            text(margin, top, "02    /    Relative to Neutron", 11, ink, "Helvetica-Bold")
            label(width - margin, top - 1, "GEOMETRIC MEAN / ALL SIX KERNELS", "right")
            text(margin, top + 19, "Neutron = 1.00x. Shorter bars mean less time.", 8.5, muted)
            raw_ratios = relative_times(suite)
            ratios = {language: ratio / raw_ratios["Neutron"] for language, ratio in raw_ratios.items()}
            maximum = max(ratios.values()) * 1.1
            bar_x, bar_width = margin + 112, content_width - 187
            reference_x = bar_x + bar_width / maximum
            line(top + 34, bar_x, bar_x + bar_width)
            for index, (language, ratio) in enumerate(ratios.items()):
                y = top + 51 + index * 27
                text(margin, y + 4, aliases.get(language, language), 9, ink,
                     "Helvetica-Bold" if language == "Neutron" else "Helvetica")
                rectangle(bar_x, y - 5, bar_width * ratio / maximum, 10,
                          accent if language == "Neutron" else colors.HexColor("#777F72"))
                text(width - margin, y + 4, f"{ratio:.2f}x", 10, ink, number_font, "right")
            document.setStrokeColor(ink)
            document.setLineWidth(0.6)
            document.setDash(2, 3)
            document.line(reference_x, height - top - 34, reference_x, height - top - 164)
            document.setDash()
            text(margin, top + 189, "One machine. Standard safety checks. No universal language ranking.",
                 8, muted)
            document.showPage()
            page_number += 1
        if suites.keys() - {"native"}:
            has_concurrency = "concurrency" in suites
            has_legacy = "legacy" in suites
            title = ("Runtime", "& startup") if has_concurrency and has_legacy else (
                ("Concurrent", "workloads") if has_concurrency else ("Process", "fixtures")
            )
            count = sum(len(suite["workloads"]) for name, suite in suites.items() if name != "native")
            cpu_count = report["workers"] if has_concurrency else 1
            page(title, "Median wall time in milliseconds. Lower is better.", page_number, page_count,
                 [(f"{count:02d}", "TESTS"), ("02", "TOOLS"), (f"{cpu_count:02d}", "CPUs")])
            top = 225
            if has_concurrency:
                top = table(suites["concurrency"], top, "01    /    Concurrency",
                            f'{report["workers"]} CPU AFFINITY / COLOR = FASTEST', 26) + 24
            if has_legacy:
                top = table(suites["legacy"], top,
                            "02    /    Process fixtures" if has_concurrency else "01    /    Process fixtures",
                            "FIXED INPUTS / 1 CPU", 29) + 14
                text(margin, top, "Includes startup. Fixed inputs may be constant-folded; not a kernel ranking.",
                     7.5, muted)
            if top > height - 70:
                raise ValueError("too many workloads for the compact scorecard")
            document.showPage()
        document.save()
        temporary_path.chmod(0o644)
        temporary_path.replace(output)
    finally:
        temporary_path.unlink(missing_ok=True)


def write_json(report, directory):
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "results.json"
    temporary = path.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")
    temporary.replace(path)


@contextmanager
def affinity(cpus):
    previous = os.sched_getaffinity(0) if hasattr(os, "sched_getaffinity") else None
    try:
        if previous is not None:
            os.sched_setaffinity(0, set(cpus))
        yield
    finally:
        if previous is not None:
            os.sched_setaffinity(0, previous)


class GoServer:
    def __init__(self, binary, environment):
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]
        self.process = subprocess.Popen(
            [str(binary), "--port", str(self.port)], env=environment,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    raise RuntimeError("local HTTP server exited before becoming ready")
                try:
                    with socket.create_connection(("127.0.0.1", self.port), timeout=0.2):
                        return
                except OSError:
                    time.sleep(0.05)
            raise RuntimeError("local HTTP server did not become ready")
        except BaseException:
            self.close()
            raise

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()


def compile_fixture(language, name, compiler, environment):
    directory = BUILD / "build"
    directory.mkdir(parents=True, exist_ok=True)
    source_directory, extension = {
        "Neutron": ("ntsc", "nt"), "Go": ("go", "go"), "Rust": ("rust", "rs"),
    }[language]
    source = ROOT / "benchmarks" / source_directory / f"{name}.{extension}"
    exe = ".exe" if sys.platform == "win32" else ""
    binary = directory / f"{language.lower()}_{name}{exe}"
    cwd = ROOT
    if language == "Neutron":
        cwd = directory / name
        (cwd / "src").mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, cwd / "src/main.nt")
        try:
            triple = command_output(["rustc", "-vV"]).split("host: ", 1)[1].splitlines()[0]
        except (subprocess.SubprocessError, FileNotFoundError):
            triple = "x86_64-pc-windows-gnu" if sys.platform == "win32" else "x86_64-unknown-linux-gnu"
        ffi_config = '\n[ffi]\nlibraries = ["synchronization", "gcc_eh", "gcc_s"]\n' if sys.platform == "win32" else ""
        (cwd / "neutron.toml").write_text(
            f'[package]\ntarget = "{triple}"\nentry = "src/main.nt"\noutput = "{name}"\n{ffi_config}'
        )
        command = [str(compiler), "build", "--release"]
        binary = cwd / f"build/release/{name}{exe}"
    elif language == "Go":
        command = ["go", "build", "-trimpath", "-o", str(binary), str(source)]
    else:
        command = ["rustc", "--edition=2024", "-C", "opt-level=3", "-C", "codegen-units=1",
                   "-C", "panic=abort", str(source), "-o", str(binary)]
    print(f"Building {language} / {name}...", flush=True)
    started = time.perf_counter()
    result = subprocess.run(command, cwd=cwd, env=environment, text=True,
                            capture_output=True, timeout=600)
    log = directory / f"{language.lower()}_{name}.log"
    log.write_text(result.stdout + result.stderr)
    if result.returncode:
        raise RuntimeError(f"{language} / {name} build failed; see {log}")
    return binary, {
        "command": command, "binary_bytes": binary.stat().st_size,
        "build_seconds": time.perf_counter() - started,
        "source": str(source.relative_to(ROOT)),
        "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
    }


def parse_checksum(output):
    lines = output.strip().splitlines()
    if len(lines) != 1:
        raise ValueError(f"expected one integer checksum, got {output!r}")
    return int(lines[0])


def run_checked(binary, environment, expected, timeout):
    started = time.perf_counter_ns()
    result = subprocess.run([str(binary)], env=environment, text=True,
                            capture_output=True, timeout=timeout, check=True)
    elapsed = (time.perf_counter_ns() - started) / 1e9
    if parse_checksum(result.stdout) != expected:
        raise ValueError(f"{binary.name}: checksum mismatch; expected {expected}, got {result.stdout!r}")
    return elapsed


def sample_resources(binary, environment, expected, timeout):
    """A separate, untimed run so /proc sampling cannot bias the wall times."""
    if not hasattr(os, "wait4") or not Path("/proc/self/status").is_file():
        return {"peak_rss_kb": None, "peak_threads": None}
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        process = subprocess.Popen([str(binary)], env=environment, stdout=stdout, stderr=stderr)
        deadline = time.monotonic() + timeout
        threads = 0
        try:
            while True:
                pid, status, usage = os.wait4(process.pid, os.WNOHANG)
                if pid:
                    process.returncode = os.waitstatus_to_exitcode(status)
                    break
                try:
                    contents = Path(f"/proc/{process.pid}/status").read_text()
                    for entry in contents.splitlines():
                        if entry.startswith("Threads:"):
                            threads = max(threads, int(entry.split()[1]))
                except (FileNotFoundError, ProcessLookupError):
                    pass
                if time.monotonic() >= deadline:
                    raise subprocess.TimeoutExpired(str(binary), timeout)
                time.sleep(0.003)
            stdout.seek(0)
            stderr.seek(0)
            output = stdout.read().decode()
            if process.returncode:
                raise subprocess.CalledProcessError(process.returncode, str(binary),
                                                    output=output, stderr=stderr.read().decode())
            if parse_checksum(output) != expected:
                raise ValueError(f"{binary.name}: resource-run checksum mismatch")
            return {"peak_rss_kb": usage.ru_maxrss, "peak_threads": threads or None}
        finally:
            if process.returncode is None:
                process.kill()
                process.wait()


def prime_count(limit):
    sieve = bytearray(b"\x01") * limit
    sieve[:min(2, limit)] = b"\x00" * min(2, limit)
    for candidate in range(2, int(limit ** 0.5) + 1):
        if sieve[candidate]:
            start = candidate * candidate
            sieve[start:limit:candidate] = b"\x00" * len(range(start, limit, candidate))
    return sum(sieve)


def legacy_cases():
    a, b = 0, 1
    for _ in range(2, 36):
        a, b = b, a + b
    matrix = sum((i * 20 + k + 1) * (k * 20 + j + 1)
                 for i in range(20) for j in range(20) for k in range(20))
    return [
        ("fibonacci", "Fibonacci / n = 35", {}, b),
        ("primes", "Prime count / 1,000", {}, prime_count(1001)),
        ("matrix", "Integer matrix / 20 x 20", {}, matrix),
        ("loops", "Loop operations / 100k", {}, 2 * sum(range(100000)) + 25000),
    ]


def concurrency_cases(workers, quick):
    spawn_sizes = (100, 1000) if quick else (10000, 100000)
    cases = [
        ("goroutine_spawn", f"Spawn / {size:,} tasks", {"NTS_BENCH_N": str(size)}, size * 2016)
        for size in spawn_sizes
    ]
    for count in dict.fromkeys((1, workers, workers * 4)):
        cases.append(("cpu_fanout", f"CPU fanout / {count} tasks",
                      {"NTS_BENCH_N": str(count)}, prime_count(400000 // count * count)))
    for count, sleepers in (
        (workers, 2 if quick else 64), (workers, 8 if quick else 512),
        (workers * 4, 8 if quick else 512),
    ):
        cases.append(("io_mixed", f"Mixed / {count} CPU + {sleepers} timers",
                      {"NTS_BENCH_N": str(count), "NTS_BENCH_IO": str(sleepers)},
                      prime_count(400000 // count * count) + sleepers))
    for count in ((2, 4, 8) if quick else (16, 128, 512)):
        cases.append(("http_fanout", f"HTTP / {count} requests",
                      {"NTS_BENCH_N": str(count)}, count * 505))
    return cases


def measure_fixtures(identifier, compiler, args, environment, cpus, output):
    languages = ["Neutron", "Go" if identifier == "concurrency" else "Rust"]
    names = CONCURRENCY if identifier == "concurrency" else LEGACY
    binaries, builds = {}, {}
    for name in names:
        for language in languages:
            binaries[language, name], builds[f"{language}/{name}"] = compile_fixture(
                language, name, compiler, environment)
    server_binary = None
    if identifier == "concurrency":
        server_binary, builds["Go/httpserver"] = compile_fixture("Go", "httpserver", compiler, environment)
    cases = concurrency_cases(args.workers, args.quick) if identifier == "concurrency" else legacy_cases()
    suite = {
        "id": identifier, "status": "running", "total_workloads": len(cases),
        "languages": languages, "profile": "smoke / not for publication" if args.quick else "full",
        "samples": args.samples, "warmup": args.warmup, "builds": builds, "workloads": [],
        "cpus": cpus,
    }
    generator = random.Random(20261118)
    environment = {**environment, "GOMAXPROCS": str(len(cpus))}
    with affinity(cpus):
        server = GoServer(server_binary, environment) if server_binary else None
        try:
            for index, (name, label, overrides, expected) in enumerate(cases):
                runtime_environment = {**environment, **overrides, "GOMAXPROCS": str(len(cpus))}
                if server:
                    runtime_environment["NTS_BENCH_PORT"] = str(server.port)
                samples = {language: [] for language in languages}
                for round_number in range(args.warmup + args.samples):
                    order = languages.copy()
                    generator.shuffle(order)
                    for language in order:
                        elapsed = run_checked(binaries[language, name], runtime_environment,
                                              expected, args.timeout)
                        if round_number >= args.warmup:
                            samples[language].append(elapsed)
                results = {
                    language: {
                        "seconds": times, "median_seconds": statistics.median(times),
                        "min_seconds": min(times), "max_seconds": max(times),
                        "stdev_seconds": statistics.stdev(times),
                    } for language, times in samples.items()
                }
                if identifier == "concurrency":
                    for language in languages:
                        results[language].update(sample_resources(
                            binaries[language, name], runtime_environment, expected, args.timeout))
                suite["workloads"].append({
                    "id": f"{name}_{index}", "title": label, "sheet_label": label,
                    "size": overrides, "checksum": expected, "oracle_verified": True,
                    "results": results,
                })
                write_json(suite, output)
                print(f"{label}: " + " | ".join(
                    f'{language} {results[language]["median_seconds"] * 1000:.2f} ms'
                    for language in languages), flush=True)
        finally:
            if server:
                server.close()
    suite["status"] = "complete"
    write_json(suite, output)
    return suite


def measure_native(compiler, args, output, cpu, languages=None):
    if languages is None:
        languages = LANGUAGES
    binaries, builds, environment = build_binaries(compiler, languages)
    environment["GOMAXPROCS"] = "1"
    cpu_info = Path("/proc/cpuinfo").read_text() if Path("/proc/cpuinfo").is_file() else ""
    model = next((line.split(":", 1)[1].strip() for line in cpu_info.splitlines()
                  if line.startswith("model name")), platform.processor() or "Unknown CPU")
    versions = {"Neutron": command_output([str(compiler), "--version"])}
    prebuilt_versions = {}
    comp = compiler.resolve() if isinstance(compiler, Path) else Path(compiler).resolve()
    for cand in (ROOT / "benchmarks/prebuilt/versions.json", ROOT / "prebuilt/versions.json", comp.parent / "prebuilt/versions.json", comp.parent.parent / "prebuilt/versions.json"):
        if cand.is_file():
            try:
                prebuilt_versions = json.loads(cand.read_text())
                break
            except Exception:
                pass

    if "C · GCC" in languages:
        if shutil.which("gcc"):
            versions["C · GCC"] = command_output(["gcc", "--version"]).splitlines()[0]
        else:
            versions["C · GCC"] = prebuilt_versions.get("C · GCC", "prebuilt release binary")
    if "C · Clang" in languages:
        if shutil.which("clang"):
            versions["C · Clang"] = command_output(["clang", "--version"]).splitlines()[0]
        else:
            versions["C · Clang"] = prebuilt_versions.get("C · Clang", "prebuilt release binary")
    if "Go" in languages:
        if shutil.which("go"):
            versions["Go"] = command_output(["go", "version"])
        else:
            versions["Go"] = prebuilt_versions.get("Go", "prebuilt release binary")
    if "Rust" in languages:
        if shutil.which("rustc"):
            versions["Rust"] = command_output(["rustc", "--version"])
        else:
            versions["Rust"] = prebuilt_versions.get("Rust", "prebuilt release binary")
    suite = {
        "id": "native", "schema_version": 1, "status": "running",
        "total_workloads": len(WORKLOADS), "languages": languages,
        "measured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "profile": "smoke / not for publication" if args.quick else "hard",
        "samples": args.samples, "warmup": args.warmup, "random_seed": 20261105,
        "system": {"cpu": model, "pinned_cpu": cpu, "os": platform.platform()},
        "versions": versions,
        "source_sha256": {
            path.name: hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(NATIVE_SOURCES.glob("kernels.*"))
        },
        "builds": builds, "workloads": [],
        "methodology": [
            "Identical seeded inputs and independent Python checksum oracles at smoke and full sizes.",
            "Generic host ISA, optimized native binaries, no fast-math, BLAS, unsafe Rust or parallel kernels.",
            "C -O3 with floating-point contraction disabled; Rust opt-level=3; Neutron --release.",
            "C arrays have no bounds checks; other languages retain their normal safety checks.",
            "Integer checksums are exact; FFT and matrix checks use declared numerical tolerances.",
        ],
    }
    labels = {"queens": "N-Queens", "mandelbrot": "Mandelbrot", "matmul": "Blocked matrix",
              "radix": "Radix sort", "dijkstra": "Shortest paths", "fft": "Complex FFT"}
    generator = random.Random(suite["random_seed"])
    with affinity([cpu]):
        for workload, title, category, description, hard_size, smoke_size in WORKLOADS:
            print(f"Validating {title} against independent oracle...", flush=True)
            smoke_expected = reference_result(workload, smoke_size)
            for language in languages:
                _, actual = run_binary(binaries[language], workload, smoke_size, environment, args.timeout)
                check_result(actual, smoke_expected, workload)
            size = smoke_size if args.quick else hard_size
            expected = smoke_expected if args.quick else reference_result(workload, size)
            samples = {language: [] for language in languages}
            for round_number in range(args.warmup + args.samples):
                order = languages.copy()
                generator.shuffle(order)
                for language in order:
                    elapsed, actual = run_binary(binaries[language], workload, size, environment, args.timeout)
                    check_result(actual, expected, workload)
                    if round_number >= args.warmup:
                        samples[language].append(elapsed)
                print(f"  {title}: round {round_number + 1}/{args.warmup + args.samples}", flush=True)
            suite["workloads"].append({
                "id": workload, "title": title, "category": category, "description": description,
                "sheet_label": f"{labels[workload]} / {size:,}", "size": size,
                "checksum": expected, "oracle_verified": True,
                "results": {
                    language: {
                        "seconds": times, "median_seconds": statistics.median(times),
                        "min_seconds": min(times), "max_seconds": max(times),
                        "stdev_seconds": statistics.stdev(times),
                    } for language, times in samples.items()
                },
            })
            write_json(suite, output)
    suite["status"] = "complete"
    write_json(suite, output)
    svg = args.svg.resolve() if args.svg else output / "results.svg"
    render_table(suite, svg)
    return suite


def selected_suites(values):
    if not values or "all" in values:
        return list(SUITES)
    return [suite for suite in SUITES if suite in values]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", choices=(*SUITES, "all"), action="append")
    parser.add_argument("--samples", "--iterations", type=int, default=7)
    parser.add_argument("--warmup", type=int, default=2)
    parser.add_argument("--quick", action="store_true", help="smoke inputs, never publication timings")
    parser.add_argument("--timeout", type=float, default=120)
    parser.add_argument("--workers", type=int, default=None, help="concurrency CPUs; default up to four")
    parser.add_argument("--cpu", type=int, help="first logical CPU for the selected CPU set")
    parser.add_argument("--compiler", type=Path, help="use an existing CLI instead of building the current one")
    parser.add_argument("--output", type=Path, help="raw report directory")
    parser.add_argument("--pdf", type=Path, help="PDF scorecard path")
    parser.add_argument("--no-pdf", action="store_true", help="measure without the optional PDF dependency")
    parser.add_argument("--allow-missing", action="store_true", help="benchmark available toolchains without aborting on missing ones")
    parser.add_argument("--svg", type=Path, help="native-kernel SVG path")
    parser.add_argument("--render-only", type=Path, help="render an existing combined JSON report")
    args = parser.parse_args()
    if args.samples < 3 or args.warmup < 0 or args.timeout <= 0 or (
        args.workers is not None and args.workers < 1
    ):
        parser.error("samples >= 3; warmup >= 0; timeout > 0; workers >= 1")
    if args.render_only:
        report = json.loads(args.render_only.read_text())
        if "suites" not in report and report.get("languages", LANGUAGES) == LANGUAGES:
            svg = args.svg or (args.output or args.render_only.parent) / "results.svg"
            if "smoke" in report["profile"] and svg.resolve() == ROOT / "benchmarks/results.svg":
                parser.error("smoke reports cannot overwrite the published SVG")
            render_table(report, svg)
            print(f"SVG: {svg}", flush=True)
            return
        validate_report(report)
        smoke = "smoke" in report["profile"]
        pdf = args.pdf or (args.render_only.parent / "scorecard.pdf" if smoke else PUBLIC_PDF)
        if smoke and pdf.resolve() == PUBLIC_PDF:
            parser.error("smoke reports cannot overwrite the published PDF")
        render_pdf(report, pdf)
        print(f"PDF: {pdf}", flush=True)
        return
    suites = selected_suites(args.suite)
    output = (args.output or BUILD / ("smoke" if args.quick else "latest")).resolve()
    pdf = (args.pdf or (output / "scorecard.pdf" if args.quick else PUBLIC_PDF)).resolve()
    if args.quick and (pdf == PUBLIC_PDF or (
        args.svg and args.svg.resolve() == ROOT / "benchmarks/results.svg"
    )):
        parser.error("smoke runs cannot overwrite published reports")
    if not args.no_pdf:
        if importlib.util.find_spec("reportlab") is None:
            parser.error("install PDF dependencies: python3 -m pip install -r benchmarks/requirements.txt")
    exe = ".exe" if sys.platform == "win32" else ""
    compiler = args.compiler.resolve() if args.compiler else ROOT / f"target/debug/ntsc{exe}"
    if not args.compiler:
        subprocess.run(["cargo", "build", "-p", "ntsc-cli"], cwd=ROOT, check=True)
    if not compiler.is_file():
        parser.error(f"compiler not found: {compiler}")
    tools = {"rustc"}
    if "native" in suites:
        tools.update(("gcc", "clang", "go"))
    if "concurrency" in suites:
        tools.add("go")
    if not args.allow_missing:
        for tool in sorted(tools):
            if not shutil.which(tool):
                lang_name = {"gcc": "C · GCC", "clang": "C · Clang", "go": "Go", "rustc": "Rust"}.get(tool)
                if lang_name and find_prebuilt_binary(compiler, lang_name):
                    continue
                parser.error(f"required compiler not found: {tool}")
    allowed = sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else list(range(os.cpu_count() or 1))
    first = allowed[0] if args.cpu is None else args.cpu
    if first not in allowed:
        parser.error(f"CPU {first} is not in the allowed affinity")
    cpus = [first] + [cpu for cpu in allowed if cpu != first]
    args.workers = args.workers or min(4, len(cpus))
    if args.workers > len(cpus):
        parser.error("workers exceeds the available CPU affinity")
    cpus = cpus[:args.workers]
    environment = {key: value for key, value in os.environ.items() if not key.startswith("NTS_BENCH_")}
    environment.update(GOCACHE=str(BUILD / "go-cache"), GO111MODULE="off")
    cpu_info = Path("/proc/cpuinfo").read_text() if Path("/proc/cpuinfo").is_file() else ""
    model = next((line.split(":", 1)[1].strip() for line in cpu_info.splitlines()
                  if line.startswith("model name")), platform.processor() or "Unknown CPU")
    tool_map = {"C · GCC": "gcc", "C · Clang": "clang", "Go": "go", "Rust": "rustc"}
    def has_tool_or_prebuilt(lang, tool):
        if not args.allow_missing:
            return True
        if shutil.which(tool):
            return True
        return find_prebuilt_binary(compiler, lang) is not None

    native_languages = ["Neutron"] + [lang for lang, tool in tool_map.items() if has_tool_or_prebuilt(lang, tool)]
    rust_version = command_output(["rustc", "--version"]) if shutil.which("rustc") else "not installed"
    report = {
        "schema_version": 1, "status": "running", "selected_suites": suites,
        "measured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "git_revision": command_output(["git", "rev-parse", "HEAD"], cwd=ROOT),
        "git_worktree_dirty": bool(command_output(["git", "status", "--porcelain"], cwd=ROOT)),
        "profile": "smoke / not for publication" if args.quick else "full",
        "samples": args.samples, "warmup": args.warmup, "workers": args.workers,
        "random_seed": 20261118,
        "system": {"cpu": model, "cpus": cpus, "os": platform.platform()},
        "versions": {"Neutron": command_output([str(compiler), "--version"]),
                     "Rust": rust_version},
        "methodology": [
            "Release builds; sequential randomized interleaved samples; median end-to-end wall time.",
            "Zero exit status and independent full checksum validation on every run, including warmups.",
            "Native and legacy: one CPU. Concurrency: identical CPU affinity and Go GOMAXPROCS.",
            "Concurrency RSS/thread diagnostics are separate, validated, untimed runs.",
            "Legacy fixed-input fixtures can be constant-folded and are not kernel rankings.",
        ],
        "suites": [],
    }
    if "go" in tools and shutil.which("go"):
        report["versions"]["Go"] = command_output(["go", "version"])
    write_json(report, output)
    for identifier in suites:
        suite_output = output / identifier
        if identifier == "native":
            suite = measure_native(compiler, args, suite_output, first, languages=native_languages)
        else:
            suite = measure_fixtures(identifier, compiler, args, environment,
                                     cpus if identifier == "concurrency" else [first], suite_output)
        report["suites"].append(suite)
        write_json(report, output)
    report["status"] = "complete"
    validate_report(report)
    write_json(report, output)
    if not args.no_pdf:
        render_pdf(report, pdf)
        print(f"PDF: {pdf}", flush=True)
    print(f"Raw samples: {output / 'results.json'}", flush=True)


def entrypoint():
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Benchmark aborted; published PDF unchanged: {error}", file=sys.stderr)
        raise SystemExit(1) from error


if __name__ == "__main__":
    entrypoint()
