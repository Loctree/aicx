# Linux Release Matrix

As part of the goal to make Linux targets first-class citizens alongside macOS, the `aicx` project tracks cross-platform builds and specific artifacts required for compatibility.

## Supported Targets

| Target                         | Support Level | Description                                |
|--------------------------------|---------------|--------------------------------------------|
| `x86_64-unknown-linux-gnu`     | First-class   | Standard Linux x86_64 environments         |
| `aarch64-unknown-linux-musl`   | First-class   | Linux aarch64 environments (e.g. Alpine)   |

## Local Build Instructions

The recommended way to build release binaries for Linux targets is via the provided `Makefile` target which wraps the `cross` toolchain for deterministic builds.

### Prerequisites

You need `cross` installed on your development machine, which relies on a running Docker daemon.

```bash
cargo install cross --git https://github.com/cross-rs/cross
```

If the docker-based `cross` environment fails (for example, with a "toolchain not fully qualified" error), you may try a local native approach:
```bash
rustup target add x86_64-unknown-linux-gnu
rustup target add aarch64-unknown-linux-musl
```
Note that building C-dependencies (like `sqlite-vec`) for a different architecture via pure `cargo` without cross compilers (like `aarch64-linux-musl-gcc`) will fail locally. `cross` provides the necessary containerized environments.

### Building

To build slim unsigned release binaries:

```bash
make release-binaries-linux
```

This will produce `.tar.gz` artifacts inside the `dist/` directory containing both `aicx` and `aicx-mcp` binaries for the defined Linux targets.

## GitHub Actions Workflow

A dedicated workflow (`.github/workflows/release-linux.yml`) is triggered on newly pushed version tags (`v*`).

- Runs on `ubuntu-latest`.
- Uses a matrix strategy across `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-musl`.
- Compiles via `cross` using the `taiki-e/install-action@v2` setup action.
- Uploads packaged `.tar.gz` slim artifacts to the newly created GitHub Release.
- Runs a smoke test via `cross test --workspace --lib` to ensure base functional readiness.

## sqlite-vec Musl Pre-Flight Spike Results

**Context**: In order to verify if the C amalgamation from `sqlite-vec` (0.1.x) builds cleanly on `aarch64-unknown-linux-musl` post-PR #199, a spike was run.

**Environment**: `darwin` with `cross` missing local docker toolchains and natively missing C-linkers (`aarch64-linux-musl-gcc`).

**Verbatim Output** (Native build fallback due to cross limitations):
```
Compiling find-msvc-tools v0.1.9
Compiling shlex v1.3.0
Compiling cc v1.2.62
Compiling sqlite-vec v0.1.9
warning: sqlite-vec@0.1.9: Compiler family detection failed due to error: ToolNotFound: failed to find tool "aarch64-linux-musl-gcc": No such file or directory (os error 2)
error: failed to run custom build command for `sqlite-vec v0.1.9`
...
error occurred in cc-rs: failed to find tool "aarch64-linux-musl-gcc": No such file or directory (os error 2)
```

**Verdict**: FRICTION / BLOCKER on Local Build. 
- While `sqlite-vec` might compile cleanly on musl inside the official `cross` Docker images, running it without an installed `aarch64-linux-musl-gcc` host compiler will instantly break `cargo check` and `cargo build`.
- For the C2 dispatch, using `sqlite-vec` means we absolutely depend on `cross` working accurately or GHA validating it on its provided Ubuntu container matrix. We recommend waiting for upstream CI tests or adopting a fallback pure-Rust approach on musl if the C2 implementation faces local development difficulties.

## Known Limitations

- **Embedder Features**: Using features like `native-embedder` might have varied support depending on the underlying dynamically linked libraries or cross-compilation matrix availability.
- **Local Dev Environments**: Developing against `sqlite-vec` for musl from macOS without a fully functional `cross` + docker toolchain is currently blocked by missing C cross-compilers.

---
*Vibecrafted. with AI Agents by VetCoders (c)2024-2026 LibraxisAI*
