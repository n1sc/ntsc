#!/usr/bin/env python3
"""Reproducible native-kernel comparisons; no third-party Python packages."""

import argparse
import datetime
import hashlib
import heapq
import json
import math
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / "benchmarks/hard"
BUILD = ROOT / "target/bench-hard"
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


def build_binaries(compiler):
    BUILD.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(GOCACHE=str(BUILD / "go-cache"), GO111MODULE="off")
    builds = {
        "C · GCC": ["gcc", "-O3", "-std=c11", "-ffp-contract=off", str(SOURCES / "kernels.c"), "-lm", "-o", str(BUILD / "gcc")],
        "C · Clang": ["clang", "-O3", "-std=c11", "-ffp-contract=off", str(SOURCES / "kernels.c"), "-lm", "-o", str(BUILD / "clang")],
        "Go": ["go", "build", "-trimpath", "-o", str(BUILD / "go"), str(SOURCES / "kernels.go")],
        "Rust": ["rustc", "--edition=2024", "-C", "opt-level=3", "-C", "codegen-units=1", "-C", "panic=abort", str(SOURCES / "kernels.rs"), "-o", str(BUILD / "rust")],
    }
    project = BUILD / "neutron"
    (project / "src").mkdir(parents=True, exist_ok=True)
    shutil.copyfile(SOURCES / "kernels.nt", project / "src/main.nt")
    triple = command_output(["rustc", "-vV"]).split("host: ", 1)[1].splitlines()[0]
    (project / "neutron.toml").write_text(
        f'[package]\ntarget = "{triple}"\nentry = "src/main.nt"\noutput = "kernels"\n'
    )
    builds["Neutron"] = [str(compiler), "build", "--release"]
    paths = {name: BUILD / filename for name, filename in [
        ("C · GCC", "gcc"), ("C · Clang", "clang"), ("Go", "go"), ("Rust", "rust"),
    ]}
    paths["Neutron"] = project / "build/release/kernels"
    records = {}
    for name in LANGUAGES:
        print(f"Building {name}...", flush=True)
        started = time.perf_counter()
        result = subprocess.run(
            builds[name], cwd=project if name == "Neutron" else ROOT,
            env=environment, capture_output=True, text=True, timeout=600,
        )
        (BUILD / f"{paths[name].name}-build.log").write_text(result.stdout + result.stderr)
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


def render_report(report, output):
    output.mkdir(parents=True, exist_ok=True)
    data = json.dumps(report, indent=2, allow_nan=False)
    (output / "results.json").write_text(data + "\n")
    template = (ROOT / "benchmarks/report-template.html").read_text()
    (output / "index.html").write_text(
        template.replace("__BENCHMARK_DATA__", data.replace("</", "<\\/"))
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--samples", type=int, default=7)
    parser.add_argument("--warmup", type=int, default=2)
    parser.add_argument("--quick", action="store_true", help="smoke sizes, NOT publication results")
    parser.add_argument("--timeout", type=float, default=120)
    parser.add_argument("--cpu", type=int, help="one allowed logical CPU; default: first allowed CPU")
    parser.add_argument("--compiler", type=Path, default=ROOT / "target/debug/ntsc")
    parser.add_argument("--output", type=Path, default=ROOT / "benchmarks/report")
    parser.add_argument("--render-only", type=Path, help="render existing measured JSON without rerunning")
    args = parser.parse_args()
    if args.render_only:
        render_report(json.loads(args.render_only.read_text()), args.output)
        return
    if args.samples < 3 or args.warmup < 0 or args.timeout <= 0:
        parser.error("samples must be >= 3; warmup >= 0; timeout > 0")
    for tool in ["gcc", "clang", "go", "rustc"]:
        if not shutil.which(tool):
            parser.error(f"required compiler not found: {tool}")
    if not args.compiler.is_file():
        parser.error("build the current CLI first: arq build_debug")
    binaries, builds, environment = build_binaries(args.compiler.resolve())
    cpu = None
    if hasattr(os, "sched_getaffinity"):
        allowed = sorted(os.sched_getaffinity(0))
        cpu = allowed[0] if args.cpu is None else args.cpu
        if cpu not in allowed:
            parser.error(f"CPU {cpu} is not in allowed affinity {allowed}")
        os.sched_setaffinity(0, {cpu})
    environment["GOMAXPROCS"] = "1"
    cpu_info = Path("/proc/cpuinfo").read_text() if Path("/proc/cpuinfo").exists() else ""
    model = next((line.split(":", 1)[1].strip() for line in cpu_info.splitlines() if line.startswith("model name")), platform.processor())
    report = {
        "schema_version": 1, "status": "running", "total_workloads": len(WORKLOADS),
        "measured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "git_revision": command_output(["git", "rev-parse", "HEAD"], cwd=ROOT),
        "git_worktree_dirty": bool(command_output(["git", "status", "--porcelain"], cwd=ROOT)),
        "profile": "smoke / not for publication" if args.quick else "hard",
        "samples": args.samples, "warmup": args.warmup, "random_seed": 20261105,
        "system": {"cpu": model, "logical_cpus": os.cpu_count(), "pinned_cpu": cpu, "os": platform.platform()},
        "versions": {
            "Neutron": command_output([str(args.compiler.resolve()), "--version"]),
            "C · GCC": command_output(["gcc", "--version"]).splitlines()[0],
            "C · Clang": command_output(["clang", "--version"]).splitlines()[0],
            "Go": command_output(["go", "version"]),
            "Rust": command_output(["rustc", "--version"]),
        },
        "source_sha256": {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in sorted(SOURCES.glob("kernels.*"))},
        "builds": builds, "languages": LANGUAGES, "workloads": [],
        "methodology": [
            "Equivalent algorithms and identical seeded inputs; 64-bit integer and f64 arithmetic.",
            "Optimized native binaries on a generic host ISA: C -O3, Rust opt-level=3, Neutron --release, normal optimized Go build. No -march=native, fast-math, external libraries, unsafe Rust, or parallel kernels.",
            "C floating-point contraction is disabled. Rust/Go/Neutron use ordinary floating-point semantics; FFT and matrix checks use declared numerical tolerances.",
            "Each size is passed at runtime. Every run emits and validates a checksum; nonzero exits, invalid output, and timeouts fail the run.",
            "Independent Python oracles validate all six workloads at smoke sizes and the hard-size checksums. Sortedness is checked inside the sort.",
            "End-to-end wall time includes process startup, input generation, allocation, kernel computation, checksum and cleanup. It excludes compilation and Python oracle generation.",
            "Languages execute sequentially on one inherited CPU affinity; randomized interleaved rounds after warmups. Go GOMAXPROCS=1. No concurrent benchmark runs.",
            "Median and min–max spread are shown. Raw samples are downloadable. Binary size is not RSS; peak RSS is not measured.",
            "C arrays have no bounds checks; Rust/Go/Neutron use their ordinary safe array APIs. Memory-safety costs are included, not disabled to manufacture parity.",
            "One machine and one run, not a universal language ranking. No application/server/I/O claims, BLAS comparisons, or cherry-picked omitted results.",
        ],
    }
    generator = random.Random(report["random_seed"])
    for workload, title, category, description, hard_size, smoke_size in WORKLOADS:
        print(f"Validating {title} against independent oracle...", flush=True)
        smoke_expected = reference_result(workload, smoke_size)
        for name in LANGUAGES:
            _, actual = run_binary(binaries[name], workload, smoke_size, environment, args.timeout)
            check_result(actual, smoke_expected, workload)
        size = smoke_size if args.quick else hard_size
        expected = smoke_expected if args.quick else reference_result(workload, size)
        samples = {name: [] for name in LANGUAGES}
        for round_number in range(args.warmup + args.samples):
            order = LANGUAGES.copy()
            generator.shuffle(order)
            for name in order:
                elapsed, actual = run_binary(binaries[name], workload, size, environment, args.timeout)
                check_result(actual, expected, workload)
                if round_number >= args.warmup:
                    samples[name].append(elapsed)
            print(f"  {title}: round {round_number + 1}/{args.warmup + args.samples}", flush=True)
        entry = {
            "id": workload, "title": title, "category": category, "description": description,
            "size": size, "checksum": expected, "oracle_verified": True,
            "results": {
                name: {
                    "seconds": times, "median_seconds": statistics.median(times),
                    "min_seconds": min(times), "max_seconds": max(times),
                    "stdev_seconds": statistics.stdev(times),
                } for name, times in samples.items()
            },
        }
        report["workloads"].append(entry)
        render_report(report, args.output)
        print("  " + " | ".join(f"{name}: {statistics.median(samples[name])*1000:.2f} ms" for name in LANGUAGES), flush=True)
    report["status"] = "complete"
    render_report(report, args.output)
    print(f"Report: {args.output / 'index.html'}", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Benchmark aborted (no invented results): {error}", file=sys.stderr)
        sys.exit(1)
