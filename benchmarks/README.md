# Benchmarks

One runner for all three suites: native kernels (Neutron, GCC, Clang, Go, Rust),
concurrency (Neutron, Go), and legacy process fixtures (Neutron, Rust).
The compact PDF keeps native timings separate from concurrency and startup.

```bash
python3 -m venv .venv
.venv/bin/python -m pip install -r benchmarks/requirements.txt
.venv/bin/python -B benchmarks/run.py        # all suites + PDF
# arq automatically uses .venv when present:
arq benchmark                              # same combined run
BENCH_FLAGS="--quick --samples 3" arq benchmark
arq test_benchmarks
```

Output: `benchmarks/neutron-benchmarks.pdf`. Raw samples, checksums, source hashes,
compiler details, and separate concurrency RSS/thread diagnostics:
`target/benchmarks/latest/results.json`. Smoke output stays in
`target/benchmarks/smoke/` and never replaces the published PDF.

```bash
python3 -B benchmarks/run.py --suite concurrency --workers 4
python3 -B benchmarks/run.py --suite native --suite legacy
python3 -B benchmarks/run.py --render-only target/benchmarks/latest/results.json
python3 -B benchmarks/run.py --no-pdf         # standard-library-only measurements
```

Requires GCC, Clang, Go, and stable Rust. The runner builds the current CLI once,
uses release binaries, validates full checksums and zero exit status on every run,
and takes seven randomized interleaved samples after two warmups. Native/legacy
use one CPU; concurrency uses up to four allowed CPUs and matching `GOMAXPROCS`.
Times include startup, allocation, and cleanup, not compilation. Fixed-input
legacy fixtures may be constant-folded; they are not kernel rankings.
Spawn peers now send the same arithmetic sum; HTTP peers both count the response
body rather than comparing Neutron's JSON response envelope to Go's raw body.

`run.py` is the only benchmark script, including the PDF renderer.
`--iterations` aliases `--samples`. `arq benchmark_hard`, `arq benchmark_smoke`,
and the [README SVG](results.svg) remain available. Tests live in `tests/`.
