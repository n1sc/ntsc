# Native benchmark lab

The hard suite compares **Neutron, C/GCC, C/Clang, Go, and Rust** on equivalent
native workloads. The legacy tiny-loop and concurrency scripts are preserved,
but are not inputs to this report.

## Workloads

| Kernel | Hard input | What is actually measured |
| --- | --- | --- |
| Exact N-Queens | 15 queens; 2,279,184 solutions | Exhaustive bitboard backtracking without symmetry shortcuts |
| Mandelbrot | 1,024 × 1,024 pixels, up to 256 iterations | Branch-heavy double-precision escape computation |
| Blocked matrix multiplication | 192 × 192, f64, 32-wide tiles | Real row-major A×B buffers, not a closed-form arithmetic loop |
| Radix sort | 1,000,000 seeded 31-bit keys | Four stable byte passes, buffers, copying, sortedness check |
| Dijkstra | 60,000 vertices, 480,000 directed weighted edges | Materialized graph, binary min-heap, stale-entry rejection |
| FFT | 262,144 complex samples | Bit reversal and an in-place radix-2 Cooley–Tukey transform |

Sources live in [`hard/`](hard/). The C file is compiled separately with GCC and
Clang. The implementations use the same input generator, storage widths,
iteration order, heap policy and numerical formulas. No language delegates its
kernel to an external library.

These are substantial **single-threaded computational kernels**, not full
applications, web services, parallel throughput tests, or a claim to be the
hardest possible instances. They intentionally include both scalar workloads
and workloads that expose the cost of Neutron's checked heap arrays.

## Run

From the repository root:

```bash
arq benchmark_smoke  # small correctness inputs; not publishable timings
arq benchmark_hard   # seven measured samples, two warmups per comparison
arq test_benchmarks  # benchmark-harness unit tests
```

The tasks build the **current** CLI, rather than accidentally using an older
installed or release compiler. Neutron benchmark programs are built with
`--release` even when the compiler executable is a Cargo debug build.

For custom sample counts, CPU affinity, or output directories:

```bash
python3 benchmarks/run_hard.py --samples 11 --warmup 3 --cpu 2
python3 benchmarks/run_hard.py --render-only benchmarks/report/results.json
```

All four compilers must be installed. No Python packages, npm packages, or
network dependency downloads are needed. Go's build cache stays under
`target/bench-hard/`. Binaries, scratch Neutron projects, and compiler logs also
stay under that ignored directory.

## Read the report

The measured page is [`report/index.html`](report/index.html), with the raw
sample data in [`report/results.json`](report/results.json). The HTML is
self-contained: CSS, JavaScript, inline SVG graphs, and data are embedded;
there are no CDNs, remote fonts, or runtime fetches. Open it directly as a file
or serve it:

```bash
arq benchmark_serve
# http://127.0.0.1:8765/
```

Both absolute milliseconds and normalized slowdown graphs are available.
Linear bars start at zero. Log mode uses points and explicitly labeled
logarithmic axes, not misleading log-scaled bar lengths. Whiskers show observed
minimum and maximum timings, **not confidence intervals**. The full suite is
included in the equally weighted geometric mean; no losing workload is dropped.

## Measurement contract

- Every workload is cross-checked against an independent Python oracle at
  small sizes, and hard-size outputs are also verified.
- Each run must exit successfully and print one finite, nonnegative checksum.
  Integer checksums match exactly. Matrix output uses a relative/absolute
  tolerance of `1e-10`; FFT uses `1e-8`.
- Matrix checksum validation uses the independently factored sum of A×B;
  FFT uses Parseval's energy identity. Dijkstra uses Python's `heapq`, radix
  uses Python's sort, and N-Queens uses a known exhaustive count.
- Arguments are provided at runtime and every timed output is checked, so the
  work cannot be discarded as an unobservable result.
- C uses `-O3 -ffp-contract=off`, Rust uses `opt-level=3`, one codegen unit and
  `panic=abort`, Neutron uses `--release`, and Go uses its ordinary optimized
  build. No `-march=native`, fast-math, external BLAS, or parallel kernels.
- C arrays are unchecked. Rust, Go, and Neutron retain their ordinary array
  safety mechanisms. Rust and Neutron release integer arithmetic wraps on
  overflow. All inputs and calculations stay within signed 64-bit range.
- One allowed logical CPU is inherited by the harness's children on Linux;
  Go gets `GOMAXPROCS=1`. Toolchains are interleaved in a seeded randomized
  order after warmups, never timed concurrently.
- Wall time includes process startup, allocation, input generation, compute,
  checksum generation, and cleanup. Compilation and Python oracle generation
  are excluded. In particular, Go's GC and process-exit reclamation are ordinary
  language runtime behavior, not replaced with manual frees.
- Report provenance includes CPU/OS, compiler versions, source hashes, Git
  revision plus dirty status, flags, executable sizes and every timing sample.
  Executable size is **not** peak memory usage; RSS is not measured.
- Partial runs and smoke runs are visibly labeled. Failures and timeouts abort
  rather than being changed into fabricated measurements.

Run on a quiet machine, preferably more than once and on more than one CPU
architecture, before using results as public performance claims. A single
desktop run is evidence about these kernels on this machine—not a universal
language ranking. Compilation latency, ecosystem quality, application behavior,
memory safety, and parallel/I/O workloads deserve separate experiments.
