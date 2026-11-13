#!/usr/bin/env python3
"""
NTSC vs Rust Benchmark Suite

Compares the performance of NTSC (Neutron Rust Rewrite) against native Rust
on the same algorithms. Both compile to native binaries; execution time is
measured via hyperfine for stable, repeatable results.

Usage:
    python run_benchmark.py [--iterations 10]
"""

import argparse
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time

# ── Colours ──────────────────────────────────────────────────────────────

class Colours:
    RED = '\033[0;31m'
    GREEN = '\033[0;32m'
    YELLOW = '\033[1;33m'
    BLUE = '\033[0;34m'
    CYAN = '\033[0;36m'
    MAGENTA = '\033[0;35m'
    BOLD = '\033[1m'
    NC = '\033[0m'

    @staticmethod
    def print(text, colour=NC, end='\n'):
        try:
            if sys.stdout.isatty() and platform.system() != "Windows":
                print(f"{colour}{text}{Colours.NC}", end=end)
            else:
                print(text, end=end)
        except UnicodeEncodeError:
            safe = text.encode('ascii', 'replace').decode('ascii')
            print(safe, end=end)

# ── Helpers ──────────────────────────────────────────────────────────────

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
REWRITE_DIR = os.path.dirname(SCRIPT_DIR)  # rewrite/
PROJECT_DIR = os.path.dirname(REWRITE_DIR)  # neutron-rs/
NEUTRON_BENCH_DIR = os.path.join(SCRIPT_DIR, "ntsc")  # new NTSC-compatible benchmarks
RUST_BENCH_DIR = os.path.join(SCRIPT_DIR, "rust")
BUILD_DIR = os.path.join(REWRITE_DIR, "target", "bench-legacy")

def find_ntsc():
    """Locate the `ntsc` CLI binary. Prefers release build."""
    candidates = [
        os.path.join(REWRITE_DIR, "target", "release", "ntsc"),
        os.path.join(REWRITE_DIR, "target", "debug", "ntsc"),
    ]
    for c in candidates:
        if os.path.exists(c):
            return c
    # Build it if not found
    print("Building NTSC compiler...")
    subprocess.run(
        ["cargo", "build", "-p", "ntsc-cli"],
        cwd=REWRITE_DIR, check=True,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    # Build runtime too
    subprocess.run(
        ["cargo", "build", "-p", "ntsc-runtime"],
        cwd=REWRITE_DIR, check=True,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    return candidates[0] if os.path.exists(candidates[0]) else None

def ensure_rustc():
    """Ensure `rustc` is available."""
    if not shutil.which("rustc"):
        print("rustc not found. Please install Rust: https://rustup.rs")
        sys.exit(1)

def ensure_hyperfine():
    """Check if hyperfine is available (optional but recommended)."""
    return shutil.which("hyperfine") is not None

def host_triple():
    """LLVM target triple for the host platform."""
    machine = platform.machine().lower()
    arch = {
        "x86_64": "x86_64",
        "amd64": "x86_64",
        "aarch64": "aarch64",
        "arm64": "aarch64",
    }.get(machine, "x86_64")
    system = platform.system().lower()
    if system == "darwin":
        return f"{arch}-apple-darwin"
    if system == "windows":
        return f"{arch}-pc-windows-msvc"
    return f"{arch}-unknown-linux-gnu"

def compile_ntsc(ntsc_bin, src_file, out_dir):
    """Compile an NTSC source file into a native binary."""
    name = os.path.splitext(os.path.basename(src_file))[0]
    out_bin = os.path.join(out_dir, f"ntsc_{name}")

    # Create project inside REWRITE_DIR so the CLI can find the workspace root.
    bench_tmp = os.path.join(REWRITE_DIR, "target", f"bench_{name}")
    os.makedirs(bench_tmp, exist_ok=True)
    src_tmp = os.path.join(bench_tmp, "src")
    os.makedirs(src_tmp, exist_ok=True)

    # Write neutron.toml
    with open(os.path.join(bench_tmp, "neutron.toml"), "w") as f:
        f.write("[package]\n")
        f.write(f'target = "{host_triple()}"\n')
        f.write(f'entry = "src/main.nt"\n')
        f.write(f'output = "{out_bin}"\n')

    # Copy source as main.nt
    with open(src_file) as f:
        src_content = f.read()
    with open(os.path.join(src_tmp, "main.nt"), "w") as f:
        f.write(src_content)

    # Build (release: the IR optimization pipeline only runs in release builds)
    try:
        result = subprocess.run(
            [ntsc_bin, "build", "--release"],
            cwd=bench_tmp,
            capture_output=True, text=True, timeout=120,
        )
        if result.returncode != 0:
            print(f"  NTSC build failed for {name}:")
            for line in result.stderr.splitlines():
                print(f"    {line}")
            return None
    finally:
        # Clean up
        shutil.rmtree(bench_tmp, ignore_errors=True)

    return out_bin if os.path.exists(out_bin) else None

def compile_rust(src_file, out_dir):
    """Compile a Rust source file into a native binary."""
    name = os.path.splitext(os.path.basename(src_file))[0]
    out_bin = os.path.join(out_dir, f"rust_{name}")
    result = subprocess.run(
        ["rustc", "-O", src_file, "-o", out_bin],
        capture_output=True, text=True, timeout=120,
    )
    if result.returncode != 0:
        print(f"  Rust compile failed for {name}:")
        for line in result.stderr.splitlines():
            print(f"    {line}")
        return None
    return out_bin if os.path.exists(out_bin) else None

def run_benchmark_hyperfine(ntsc_bin, rust_bin, name, iterations):
    """Run a single benchmark using hyperfine."""
    results = {}

    for lang, binary in [("Rust", rust_bin), ("NTSC", ntsc_bin)]:
        if binary is None:
            results[lang] = {"time": None, "display": "FAILED"}
            continue

        hyperfine_result = subprocess.run(
            ["hyperfine",
             "--warmup", "3",
             "--runs", str(iterations),
             "--ignore-failure",
             "--show-output",
             binary],
            capture_output=True, text=True, timeout=300,
        )
        if hyperfine_result.returncode != 0:
            results[lang] = {"time": None, "display": "FAILED"}
            continue

        # Parse hyperfine output for mean time
        for line in hyperfine_result.stdout.splitlines():
            if "Time (mean" in line:
                # e.g. "  Time (mean ± σ):      45.3 ms ± 2.1 ms"
                parts = line.split(":")
                if len(parts) >= 2:
                    tokens = parts[1].strip().split()
                    try:
                        val = float(tokens[0])
                        # hyperfine switches unit by magnitude: ms, µs, ns, s
                        unit = tokens[1] if len(tokens) > 1 else "s"
                        multiplier = {
                            "ms": 1e-3,
                            "µs": 1e-6,
                            "us": 1e-6,
                            "ns": 1e-9,
                            "s": 1.0,
                        }.get(unit, 1.0)
                        seconds = val * multiplier
                        if seconds >= 1.0:
                            display = f"{seconds:.3f}s"
                        elif seconds >= 1e-3:
                            display = f"{seconds*1000:.2f}ms"
                        else:
                            display = f"{seconds*1e6:.1f}µs"
                        results[lang] = {"time": seconds, "display": display}
                    except (ValueError, IndexError):
                        results[lang] = {"time": None, "display": "PARSE ERROR"}
                break

        if lang not in results:
            results[lang] = {"time": None, "display": "NO DATA"}

    return results

def run_benchmark_basic(ntsc_bin, rust_bin, name):
    """Run a single benchmark using basic subprocess timing (fallback)."""
    results = {}

    for lang, binary in [("Rust", rust_bin), ("NTSC", ntsc_bin)]:
        if binary is None:
            results[lang] = {"time": None, "display": "FAILED"}
            continue

        # Warmup
        for _ in range(3):
            subprocess.run([binary], capture_output=True, timeout=60)

        # Timed runs (exit code is the benchmark result for NTSC, so any
        # completed run counts)
        times = []
        for _ in range(10):
            start = time.time()
            result = subprocess.run([binary], capture_output=True, timeout=60)
            elapsed = time.time() - start
            if result.returncode is not None:
                times.append(elapsed)

        if times:
            avg = sum(times) / len(times)
            results[lang] = {"time": avg, "display": f"{avg*1000:.1f}ms"}
        else:
            results[lang] = {"time": None, "display": "FAILED"}

    return results

# ── Main ─────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="NTSC vs Rust Benchmark Suite")
    parser.add_argument("--iterations", type=int, default=10,
                        help="Number of iterations per benchmark (default: 10)")
    args = parser.parse_args()

    os.makedirs(BUILD_DIR, exist_ok=True)

    # Locate tools
    ntsc_bin = find_ntsc()
    if not ntsc_bin:
        Colours.print("Cannot locate or build NTSC compiler.", Colours.RED)
        sys.exit(1)

    ensure_rustc()
    use_hyperfine = ensure_hyperfine()

    # Define benchmarks
    # Sorting skipped: array IndexGet/IndexSet codegen needs LLVM type fix
    benchmarks = [
        ("Fibonacci", "fibonacci"),
        ("Primes", "primes"),
        ("Matrix", "matrix"),
        ("Loops", "loops"),
    ]

    print()
    Colours.print(f"╔{'═' * 68}╗", Colours.CYAN)
    Colours.print(f"║{' ':<25}NTSC vs Rust Benchmark Suite{' ':<25}║", Colours.CYAN)
    Colours.print(f"╚{'═' * 68}╝", Colours.CYAN)
    print()
    Colours.print(f"NTSC compiler: {ntsc_bin}", Colours.BLUE)
    col = Colours.GREEN if use_hyperfine else Colours.YELLOW
    msg = "hyperfine" if use_hyperfine else "basic timing (install hyperfine for better precision)"
    Colours.print(f"Runner:        {msg}", col)
    print()

    if not use_hyperfine:
        # Build NTSC runtime if using basic timing
        subprocess.run(
            ["cargo", "build", "-p", "ntsc-runtime"],
            cwd=REWRITE_DIR,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )

    # Header
    print()
    Colours.print(f"┌{'─' * 20}┬{'─' * 14}┬{'─' * 14}┬{'─' * 18}┐", Colours.CYAN)
    Colours.print(f"│ {'Benchmark':<18} │ {'NTSC':<12} │ {'Rust':<12} │ {'Ratio':<16} │", Colours.CYAN)
    Colours.print(f"├{'─' * 20}┼{'─' * 14}┼{'─' * 14}┼{'─' * 18}┤", Colours.CYAN)

    ntsc_wins = 0
    rust_wins = 0
    failed = 0

    for label, name in benchmarks:
        ntsc_src = os.path.join(NEUTRON_BENCH_DIR, f"{name}.nt")
        rust_src = os.path.join(RUST_BENCH_DIR, f"{name}.rs")

        if not os.path.exists(ntsc_src):
            Colours.print(f"│ {label:<18} │ {'NO NTSC':<12} │ {'':<12} │ {'':<16} │", Colours.YELLOW)
            continue

        if not os.path.exists(rust_src):
            Colours.print(f"│ {label:<18} │ {'':<12} │ {'NO RUST':<12} │ {'':<16} │", Colours.YELLOW)
            continue

        # Compile both
        Colours.print(f"\r  Compiling {label}...", Colours.BLUE, end='')
        sys.stdout.flush()

        ntsc_exe = compile_ntsc(ntsc_bin, ntsc_src, BUILD_DIR)
        rust_exe = compile_rust(rust_src, BUILD_DIR)

        if ntsc_exe is None and rust_exe is None:
            Colours.print(f"│ {label:<18} │ {'BOTH FAIL':<12} │ {'BOTH FAIL':<12} │ {'':<16} │", Colours.RED)
            failed += 1
            continue

        # Run benchmark
        Colours.print(f"\r  Running {label}...     ", Colours.BLUE, end='')
        sys.stdout.flush()

        if use_hyperfine:
            results = run_benchmark_hyperfine(ntsc_exe, rust_exe, label, args.iterations)
        else:
            results = run_benchmark_basic(ntsc_exe, rust_exe, label)

        # Calculate ratio
        rust_time = results.get("Rust", {}).get("time")
        ntsc_time = results.get("NTSC", {}).get("time")

        if rust_time is not None and ntsc_time is not None and rust_time > 0:
            ratio = ntsc_time / rust_time
            ratio_str = f"{ratio:.2f}x"
            if ratio < 1.0:
                ratio_str += " 🏆"
                ntsc_wins += 1
            else:
                ratio_str += ""
                rust_wins += 1
        else:
            ratio_str = "N/A"
            if ntsc_time is None and rust_time is not None:
                failed += 1
            elif rust_time is None and ntsc_time is not None:
                failed += 1
            else:
                failed += 1

        ntsc_disp = results.get("NTSC", {}).get("display", "FAILED")
        rust_disp = results.get("Rust", {}).get("display", "FAILED")

        Colours.print(f"│ {label:<18} │ {ntsc_disp:<12} │ {rust_disp:<12} │ {ratio_str:<16} │", Colours.NC)

    # Footer
    Colours.print(f"└{'─' * 20}┴{'─' * 14}┴{'─' * 14}┴{'─' * 18}┘", Colours.CYAN)
    print()

    # Summary
    total = len(benchmarks) - failed
    if total > 0:
        print(f"  NTSC wins: {ntsc_wins} / {total}")
        print(f"  Rust wins: {rust_wins} / {total}")
        if failed > 0:
            print(f"  Failed:    {failed}")
    else:
        print("  No benchmarks completed successfully.")
        print(f"  Failed:    {failed}")

    if failed > 0:
        print()
        print("Some benchmarks failed. Check output above for details.")
        sys.exit(1)


if __name__ == "__main__":
    main()
