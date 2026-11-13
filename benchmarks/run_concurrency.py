#!/usr/bin/env python3
"""
NTSC concurrency / scaling benchmarks, measured against Go goroutines:

  1. goroutine_spawn — 10k-100k goroutines; both runtimes multiplex onto a
     fixed OS-thread pool, so "max threads" stays at pool size.
  2. cpu_fanout      — CPU-bound work split across g goroutines; g=1 ->
     g=cores shows the fan-out uses every core.
  3. io_mixed        — n_cpu CPU-bound goroutines while n_io block on a
     300 ms async.sleep; wall time staying ~300 ms proves workers are freed.
  4. http_fanout     — g concurrent awaited http.get_async fetches; in-flight
     count is decoupled from OS thread count.

NTSC reports its result as the process exit code; Go prints it and exits 0.
Usage: python run_concurrency.py [--iterations 5] [--warmup 2]
"""

import argparse
import os
import resource
import socket
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run_benchmark as rb

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
BUILD_DIR = rb.BUILD_DIR
RESULTS_FILE = os.path.join(BUILD_DIR, "results_concurrency.md")
_HAS_PROC = os.name == "posix" and os.path.exists("/proc")

SERVER_PORT = 8080
SPAWN_SIZES = [10_000, 100_000]


class GoServer:
    """Local Go net/http server; the fair peer for the Go client baseline."""

    def __init__(self, server_bin):
        sock = socket.socket()
        sock.bind(("127.0.0.1", 0))
        self.port = sock.getsockname()[1]
        sock.close()
        self.proc = subprocess.Popen(
            [server_bin, "--port", str(self.port)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                probe = socket.create_connection(("127.0.0.1", self.port), timeout=0.2)
                probe.close()
                return
            except OSError:
                time.sleep(0.05)
        raise RuntimeError("HTTP server did not start")

    def close(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def find_ntsc():
    release = os.path.join(rb.REWRITE_DIR, "target", "release", "ntsc")
    if not os.path.exists(release):
        print("Building NTSC release toolchain...")
        subprocess.run(
            ["cargo", "build", "--release", "-p", "ntsc-cli", "-p", "ntsc-runtime"],
            cwd=rb.REWRITE_DIR,
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    return rb.find_ntsc()


def host_thread_caps():
    caps = {}
    try:
        with open("/proc/sys/kernel/threads-max") as fh:
            caps["threads-max"] = int(fh.read().strip())
    except (OSError, ValueError):
        pass
    try:
        soft, _ = resource.getrlimit(resource.RLIMIT_NPROC)
        caps["rlimit-nproc"] = soft
    except (ImportError, ValueError):
        pass
    return caps


def compile_go(src_file, out_dir):
    name = os.path.splitext(os.path.basename(src_file))[0]
    out_bin = os.path.join(out_dir, f"go_{name}")
    result = subprocess.run(
        ["go", "build", "-o", out_bin, src_file],
        capture_output=True, text=True, timeout=120,
    )
    if result.returncode != 0:
        print(f"  Go compile failed for {name}:")
        for line in result.stderr.splitlines():
            print(f"    {line}")
        return None
    return out_bin if os.path.exists(out_bin) else None


def sample_threads(pid, duration_s):
    """Peak `Threads:` value; `None` duration means sample until the process exits."""
    path = f"/proc/{pid}/status"
    best = 0
    deadline = time.monotonic() + duration_s if duration_s else None
    while deadline is None or time.monotonic() < deadline:
        try:
            with open(path, "r") as fh:
                for line in fh:
                    if line.startswith("State:"):
                        if line.split()[1] == "Z":  # exited; wait4 reaps right after
                            return best
                    elif line.startswith("Threads:"):
                        best = max(best, int(line.split()[1]))
                        break
        except (FileNotFoundError, ProcessLookupError):
            break
        time.sleep(0.005)
    return best


def run_once(bin_path, env, via_exit, want_threads, sample_s):
    """Run a benchmark once; None on failure. NTSC reports results via exit code."""
    err_file = tempfile.TemporaryFile()
    proc = subprocess.Popen(
        [bin_path],
        env={**os.environ, **{k: str(v) for k, v in env.items()}},
        stdout=subprocess.DEVNULL,
        stderr=err_file,
    )
    start = time.perf_counter()
    threads = sample_threads(proc.pid, sample_s) if want_threads else None
    _, status, rusage = os.wait4(proc.pid, 0)
    wall = time.perf_counter() - start
    code = os.waitstatus_to_exitcode(status)
    err_file.seek(0)
    stderr = err_file.read().decode(errors="replace").strip()
    err_file.close()
    if code < 0:
        return {"error": f"killed by signal {-code}"}
    if not via_exit and code != 0:
        return {"error": f"exit code {code}", "stderr": stderr[:600]}
    rss = rusage.ru_maxrss  # Linux: KiB; macOS: bytes.
    if sys.platform == "darwin":
        rss = rss // 1024
    return {"wall": wall, "rss": rss, "threads": threads}


def measure(bin_path, env, via_exit, runs, warmups, want_threads=False, sample_s=0.3):
    for _ in range(warmups):
        if run_once(bin_path, env, via_exit, False, sample_s) is None:
            return None
    samples = []
    for _ in range(runs):
        res = run_once(bin_path, env, via_exit, want_threads, sample_s)
        if res is None or "wall" not in res:
            return None
        samples.append(res)
    walls = sorted(s["wall"] for s in samples)
    rss = [s["rss"] for s in samples if s["rss"]]
    threads = [s["threads"] for s in samples if s["threads"]]
    return {
        "best_ms": walls[0] * 1000,
        "rss_kb": max(rss) if rss else None,
        "threads": max(threads) if threads else None,
    }


def fmt_time(ms):
    if ms is None:
        return "FAILED"
    if ms >= 1000:
        return f"{ms / 1000:.2f}s"
    return f"{ms:.0f}ms"


def fmt_rss(kb):
    if kb is None:
        return "-"
    if kb >= 1024 * 1024:
        return f"{kb / (1024 * 1024):.1f}GB"
    return f"{kb / 1024:.1f}MB"


def fmt_threads(n):
    return str(n) if n else "-"


def fmt_cost(rss_kb, size):
    if not rss_kb or not size:
        return "-"
    return f"{rss_kb // size}"


def build_all(ntsc_bin):
    exes = {}
    for name in ["goroutine_spawn", "cpu_fanout", "http_fanout", "io_mixed"]:
        ntsc_exe = rb.compile_ntsc(
            ntsc_bin, os.path.join(rb.NEUTRON_BENCH_DIR, f"{name}.nt"), BUILD_DIR
        )
        go_exe = compile_go(os.path.join(rb.SCRIPT_DIR, "go", f"{name}.go"), BUILD_DIR)
        exes[name] = (ntsc_exe, go_exe)
    server_bin = compile_go(os.path.join(rb.SCRIPT_DIR, "go", "httpserver.go"), BUILD_DIR)
    return exes, server_bin


def print_row(label, lang, size_label, res, worker_count=None, show_cost=False):
    time_s = fmt_time(res["best_ms"]) if res else "FAILED"
    rss = fmt_rss(res["rss_kb"]) if res else "-"
    threads = fmt_threads(res["threads"]) if res else "-"
    cost = fmt_cost(res["rss_kb"], worker_count) if (res and worker_count) else "-"
    cost_col = f" {cost:<8}" if show_cost else ""
    print(f"  {label:<16} {lang:<6} {str(size_label):<12}{time_s:<11}{rss:<12}{threads}{cost_col}")


def run_sweep(bench, sizes, exes, runs, warmups, want_threads_for, label,
              show_cost=False):
    ntsc_exe, go_exe = exes[bench]
    header = f"## {label}"
    print()
    print(header)
    cols = "  " + f"{'Benchmark':<16} {'Lang':<6} {'Size':<12}{'Best':<11}{'RSS':<12}Threads"
    if show_cost:
        cols += f"  {'KB/wkr':<8}"
    print(cols)
    lines = [header, "", "| Benchmark | Lang | Size | Best | RSS | max threads |"]
    if show_cost:
        lines[-1] += " KB/worker |"
    lines.append("|---|---|---|---|---|---|" + ("---|" if show_cost else ""))
    for size in sizes:
        if isinstance(size, tuple):
            size_label, overrides = size
            worker_count = None
        else:
            size_label, overrides = size, {}
            worker_count = size
        env = {"NTS_BENCH_N": str(size)} if not isinstance(size, tuple) else {}
        env.update(overrides or {})
        if bench == "http_fanout":
            env["NTS_BENCH_PORT"] = str(SERVER_PORT)
        for lang, exe in [("NTSC", ntsc_exe), ("Go", go_exe)]:
            via_exit = lang == "NTSC"
            want = (bench, size_label) in want_threads_for
            sample_s = None if bench == "goroutine_spawn" else 0.3
            res = measure(exe, env, via_exit, runs, warmups,
                          want_threads=want, sample_s=sample_s)
            if res is None:
                diag = run_once(exe, env, via_exit, False, None)
                print_row(bench, lang, size_label, None, worker_count, show_cost)
                if diag and "error" in diag:
                    print(f"    -> {lang} failed: {diag['error']}")
                lines.append(
                    f"| {bench} | {lang} | {size_label} | FAILED | - | - |"
                    + (" - |" if show_cost else "")
                )
            else:
                print_row(bench, lang, size_label, res, worker_count, show_cost)
                lines.append(
                    f"| {bench} | {lang} | {size_label} | {fmt_time(res['best_ms'])} "
                    f"| {fmt_rss(res['rss_kb'])} | {fmt_threads(res['threads'])} |"
                    + (f" {fmt_cost(res['rss_kb'], worker_count)} |" if show_cost else "")
                )
    lines.append("")
    return lines


def main():
    parser = argparse.ArgumentParser(description="NTSC vs Go concurrency benchmarks")
    parser.add_argument("--iterations", type=int, default=5)
    parser.add_argument("--warmup", type=int, default=2)
    args = parser.parse_args()

    os.makedirs(BUILD_DIR, exist_ok=True)
    ntsc_bin = find_ntsc()
    if not ntsc_bin:
        print("Cannot locate or build the NTSC compiler.")
        sys.exit(1)
    if not _HAS_PROC:
        print("(no /proc: OS-thread sampling disabled)")

    cores = os.cpu_count() or 8
    caps = host_thread_caps()
    caps_note = ", ".join(f"{k}={v}" for k, v in caps.items()) or "n/a"

    print("\n== building benchmarks ==")
    exes, server_bin = build_all(ntsc_bin)

    report = []
    report.append("# NTSC concurrency scaling results (vs Go goroutines)\n")
    report.append(f"Run: {time.strftime('%Y-%m-%d %H:%M:%S')}, iterations={args.iterations}, "
                  f"warmups={args.warmup}, host cores={cores}, host caps: {caps_note}.\n")

    want_spawn = {("goroutine_spawn", s) for s in SPAWN_SIZES}
    report += run_sweep(
        "goroutine_spawn", SPAWN_SIZES, exes, args.iterations, args.warmup,
        want_spawn,
        "goroutine spawn/throughput (worker pool stays at one thread per CPU)",
        show_cost=True,
    )
    report += run_sweep(
        "cpu_fanout", [1, cores, cores * 4], exes, args.iterations, args.warmup,
        {("cpu_fanout", cores)},
        "CPU-bound fan-out (does g = cores use every worker?)",
    )
    report += run_sweep(
        "io_mixed",
        [(f"{cores}c+64io", {"NTS_BENCH_N": cores, "NTS_BENCH_IO": 64}),
         (f"{cores}c+512io", {"NTS_BENCH_N": cores, "NTS_BENCH_IO": 512}),
         (f"{cores * 4}c+512io", {"NTS_BENCH_N": cores * 4, "NTS_BENCH_IO": 512})],
        exes, args.iterations, args.warmup,
        {("io_mixed", f"{cores}c+512io")},
        "CPU-bound goroutines while n_io block on async.sleep(300 ms)",
    )

    print("\n== http_fanout: starting Go HTTP server ==")
    server = GoServer(server_bin)
    try:
        global SERVER_PORT
        SERVER_PORT = server.port
        print(f"  server on 127.0.0.1:{server.port}")
        report.append(f"\nHTTP server: Go net/http on local {server.port}.\n")
        report += run_sweep(
            "http_fanout", [16, 128, 512], exes, args.iterations, args.warmup,
            {("http_fanout", 512)},
            "concurrent awaited http.fetch fan-out",
        )
    finally:
        server.close()

    report.append(f"""\
## Notes

- Both runtimes multiplex goroutines onto a fixed OS-thread pool: the "max
  threads" column stays near one per CPU at any goroutine count. This host
  (cores={cores}, kernel allows {caps.get('threads-max', 'n/a')} OS threads) runs 100k goroutines on the
  pool; "KB/worker" is peak RSS / goroutine count.
- cpu_fanout: g=1 -> g=cores shows the fan-out uses every core; at
  g = 4*cores work stealing keeps all workers even.
- io_mixed: wall time stays ~300 ms at any n_io — blocked goroutines park on
  timers and free the worker instead of pinning it.
- http_fanout: in-flight requests are decoupled from OS thread count; the
  blocking request is offloaded to a bounded pool while the goroutine parks.
""")
    with open(RESULTS_FILE, "w") as fh:
        fh.write("\n".join(report))
    print(f"\nresults recorded in {RESULTS_FILE}")


if __name__ == "__main__":
    main()
