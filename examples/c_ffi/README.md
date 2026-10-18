# C FFI

This example calls a project-owned C static library through an `extern "C"`
declaration. The C library is in `native/geometry.c`; it uses `libm` for
distance and signal calculations. The Neutron side builds a small `Vector`
type, computes vector lengths, and samples a composite sine/cosine signal.

Build the local library first:

```console
make -C native
```

From this directory:

```console
cargo run --manifest-path ../../Cargo.toml -p ntsc-cli -- run
```

Or run the installed compiler from this directory:

```console
ntsc run
```

The output starts with:

```text
vector (3, 4) length=5
first vector dot (1, 1): 7
```

The `[ffi].search_paths` entry adds `native/` as a linker search directory,
and `[ffi].libraries` links both `libgeometry.a` and `libm`. Search paths are
project-relative. The calls are inside `unsafe` because foreign code's memory
and error preconditions are outside Neutron's static guarantees.
