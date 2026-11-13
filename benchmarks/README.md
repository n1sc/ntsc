# Benchmarks

Six native kernels across Neutron, GCC, Clang, Go, and Rust.
[Results](results.svg) are embedded in the main README.

```bash
arq benchmark_hard   # measure and refresh results.svg
arq benchmark_smoke  # small correctness inputs; does not replace published results
arq test_benchmarks  # harness tests
```

Requires GCC, Clang, Go, and stable Rust. Uses optimized, single-core binaries,
identical inputs, independent checksum checks, and seven interleaved timing samples.
Times include startup and allocation; compilation is excluded.
Raw samples, compiler logs, and build files stay under ignored `target/bench-hard/`.
