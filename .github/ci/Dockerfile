# CI image: every check runs inside this build, so CI and a local `docker buildx build` match.
# The crate has no dependencies, so there is no `cargo fetch` layer. The zstd CLI is for the tests
# that compare the decoder with frames it writes; only the `ci-tools` stage installs it, so the
# `arm64` stage skips those cases.
# Every tool is installed in a stage before `COPY . .`, so a new commit does not install it again.
FROM rust:1.99.1-bookworm AS toolchain
RUN rustup component add rustfmt clippy && rustup target add aarch64-unknown-linux-gnu
WORKDIR /src

FROM toolchain AS src
COPY . .

# The checks run in the build, so a red check fails the build; a rerun of the same source is a
# BuildKit cache hit. The registry and target dirs are cache mounts, so they persist across builds.
FROM toolchain AS ci-tools
RUN apt-get update && apt-get install -y --no-install-recommends zstd \
    && rm -rf /var/lib/apt/lists/*
ENV ZSTD_REQUIRE_CLI=1

FROM ci-tools AS ci
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-target,target=/src/target \
    cargo fmt --check
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-target,target=/src/target \
    cargo clippy --locked --all-targets -- -D warnings
# The NEON module is not compiled for x86_64, so lint the aarch64 target too.
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-target,target=/src/target \
    cargo clippy --locked --all-targets --target aarch64-unknown-linux-gnu -- -D warnings
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-target,target=/src/target \
    cargo test --locked
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-target,target=/src/target \
    ZSTD_SIMD=scalar cargo test --locked
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-target,target=/src/target \
    cargo build --locked --release --target aarch64-unknown-linux-gnu

# The arm64 job runs this stage on a native aarch64 runner: both SIMD tiers (NEON, then
# ZSTD_SIMD=scalar), only the tests that the target changes. The unit tests hold the NEON interleave
# kernel and the plain sequence loop against their references and the dispatch tests;
# tests/conformance.rs decodes plain frames (sequence loop) and tests/czi.rs decodes CZI hi/lo
# subblocks (NEON interleave). The full suite runs on x86_64 in `ci`; Go parity on aarch64 is in
# the parity-arm64 stage.
FROM src AS arm64
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-target,target=/src/target \
    cargo test --locked --lib --test conformance --test czi
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-target,target=/src/target \
    ZSTD_SIMD=scalar cargo test --locked --lib --test conformance --test czi

# Miri (ci.yml, off pull requests): the unit tests of the non-SIMD unsafe kernels, which are the raw-pointer
# copies of the sequence loop (`seqloop`, by its own tests and through `frame::tests`) and the
# plain Huffman 4-stream kernels (`hufloop`, by its own tests). Miri
# cannot run the SIMD intrinsics, so the interleave dispatch picks scalar under it (cfg(miri)); the
# unit tests use cfg!(miri) to shrink their loops. Nightly and Miri are installed before `COPY . .`.
# No `cargo miri setup` here: a sysroot built in this layer, then reused by `miri test` in /src,
# failed with "detected a concurrent sysroot build with different settings"; `miri test` builds
# its own sysroot.
FROM toolchain AS miri-tools
RUN rustup toolchain install nightly --profile minimal --component miri,rust-src

FROM miri-tools AS miri
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-rs-miri-target,target=/src/target \
    cargo +nightly miri test --locked --lib -- interleave:: seqloop:: frame::tests:: hufloop::

# Go parity: the Go oracle writes the inputs and decodes them; the Rust example decodes the same
# inputs; the two output trees must be equal.
FROM golang:1.27-bookworm AS go-oracle
ENV GOTOOLCHAIN=local CGO_ENABLED=0
WORKDIR /src
COPY tests/parity/go tests/parity/go
# oracle-v4 is the same program built with GOAMD64=v4, for the v4 column of the bench.
RUN --mount=type=cache,target=/root/.cache/go-build cd tests/parity/go/oracle && go build -o /out/oracle . \
    && GOAMD64=v4 go build -o /out/oracle-v4 .

FROM go-oracle AS go-run
COPY tests/data tests/data
COPY tests/fuzz/seeds tests/fuzz/seeds
COPY tests/fuzz/seeds-czi tests/fuzz/seeds-czi
# Inputs: the generated ones, the zstd-go FuzzDecodeAll seeds as frames, and its czi FuzzDecode
# seeds ([mode][pt][n u16 LE][src]) as .czi inputs ([mode][pt][n u32 LE][src]).
RUN /out/oracle -gen tests/data /inputs \
    && for f in tests/fuzz/seeds/*; do cp $f /inputs/seed-$(basename $f).zst; done \
    && for f in tests/fuzz/seeds-czi/*; do { head -c 4 $f; printf '\000\000'; tail -c +5 $f; } > /inputs/seed-$(basename $f).czi; done \
    && ls /inputs | wc -l && du -sh /inputs
RUN /out/oracle /inputs /go-out && ls /go-out | wc -l && du -sh /go-out
# The same at a small memory limit (1 MiB, so the charging of every growth is compared, not only
# the edge of 256 MiB), on the inputs under 16 KiB (ORACLE_MAX_INPUT=15360, what `find -size -16k` matched): the mutations of the small
# fixtures and the lying-header frames, not the tiles.
RUN ORACLE_MEM_LIMIT=1048576 ORACLE_MAX_INPUT=15360 /out/oracle /inputs /go-out-1m && ls /go-out-1m | wc -l && du -sh /go-out-1m

# The Rust oracle runs three times, each diffed against Go: the default tier (AVX2), the same at a
# 1 MiB memory limit on the inputs under 16 KiB, and the scalar tier (ZSTD_SIMD=scalar). The Go
# trees are bind mounts from go-run, not COPY: a copy of the trees after `COPY . .` would run on
# every commit. The Rust output is deleted in the same RUN (each tree right after its diff, to keep
# disk use low), so the layer (and the cache export) stays empty.
FROM src AS parity
# A tree that came out empty (a wrong tests/data path, say) must not pass as equal to another one.
# The target cache has its own id, so another stage's build cannot replace
# target/release/examples/oracle.
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-rs-parity-target,target=/src/target \
    --mount=type=bind,from=go-run,source=/inputs,target=/inputs \
    --mount=type=bind,from=go-run,source=/go-out,target=/go-out \
    --mount=type=bind,from=go-run,source=/go-out-1m,target=/go-out-1m \
    [ "$(ls /inputs | wc -l)" -ge 30000 ] && [ "$(ls /go-out | wc -l)" -ge 30000 ] \
    && [ "$(ls /go-out-1m | wc -l)" -ge 1000 ] \
    && cargo run --locked --release --example oracle -- /inputs /rust-out \
    && echo "$(ls /inputs | wc -l) inputs, $(ls /go-out | wc -l) .res files" \
    && sh tests/parity/diff.sh /go-out /rust-out \
    && rm -rf /rust-out \
    && ORACLE_MEM_LIMIT=1048576 ORACLE_MAX_INPUT=15360 cargo run --locked --release --example oracle -- /inputs /rust-out-1m \
    && echo "$(ls /go-out-1m | wc -l) .res files at 1 MiB" \
    && sh tests/parity/diff.sh /go-out-1m /rust-out-1m \
    && rm -rf /rust-out-1m \
    && ZSTD_SIMD=scalar cargo run --locked --release --example oracle -- /inputs /rust-out-scalar \
    && sh tests/parity/diff.sh /go-out /rust-out-scalar \
    && rm -rf /rust-out-scalar

# The same on a native aarch64 runner (parity-arm64 job): the Go oracle runs there too, and the
# Rust oracle (NEON) is diffed against it.
FROM src AS parity-arm64
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-rs-parity-arm64-target,target=/src/target \
    --mount=type=bind,from=go-run,source=/inputs,target=/inputs \
    --mount=type=bind,from=go-run,source=/go-out,target=/go-out \
    [ "$(ls /inputs | wc -l)" -ge 30000 ] && [ "$(ls /go-out | wc -l)" -ge 30000 ] \
    && cargo run --locked --release --example oracle -- /inputs /rust-out-arm \
    && sh tests/parity/diff.sh /go-out /rust-out-arm \
    && rm -rf /rust-out-arm

# Fuzzing (fuzz.yml): nightly and cargo-fuzz, with the targets built into the image. The
# workflow runs it with `docker run`, so a BuildKit cache hit cannot skip the fuzzing.
FROM toolchain AS fuzz-tools
RUN rustup toolchain install nightly --profile minimal \
    && cargo install cargo-fuzz --locked --version 0.13.1

FROM fuzz-tools AS fuzz
COPY . .
# -a: debug assertions and overflow checks, so a wrap in the size arithmetic is a crash.
RUN cargo +nightly fuzz build --fuzz-dir tests/fuzz -a

# Benchmark (bench.yml): the Go oracle and the Rust bench example in one image (each also as a
# v4 build: GOAMD64=v4; RUSTFLAGS=-C target-cpu=x86-64-v4, in its own target dir), with frames in
# /bench/huf whose decode time is mostly the Huffman 4-stream loops (on the tiles they are 1 to 2
# percent of the instructions). Their bytes come from a Park-Miller LCG through a geometric
# distribution (p, then mod 255, plus 1; awk doubles hold the products exactly), so they have few
# matches and are the same on every build; the zstd CLI compresses 128 KiB and 1 MiB of them at
# levels 1 and 19. At p = 0.985 the literals take about 7.5 bits a byte, where HUF_selectDecoder
# picks X1 (`huf_x1_*`); at p = 0.9 about 4.7, where it picks X2 (`huf_x2_*`). `ci-tools` has the
# zstd CLI.
FROM ci-tools AS bench-tools
RUN set -e; mkdir -p /bench/huf && cd /bench/huf \
    && for c in x1:0.985 x2:0.9; do for k in 128 1024; do \
         LC_ALL=C awk -v p=${c#*:} -v n=$((k * 1024)) 'BEGIN { x = 1; l = log(p); for (i = 0; i < n; i++) { x = x * 16807 % 2147483647; printf "%c", int(log(x / 2147483647) / l) % 255 + 1 } }' >src \
         && for z in 1 19; do zstd -q -$z src -o huf_${c%%:*}_${k}k_z$z.zst; done; \
       done; done \
    && rm src && ls -l && [ "$(ls *.zst | wc -l)" -eq 8 ]

FROM bench-tools AS bench
COPY . .
COPY --from=go-oracle /out/oracle /out/oracle-v4 /usr/local/bin/
RUN sh tests/parity/mf.sh tests/data/tiles
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-rs-bench-target,target=/src/target \
    cargo build --locked --release --example bench && cp target/release/examples/bench /usr/local/bin/bench \
    && RUSTFLAGS="-C target-cpu=x86-64-v4" cargo build --locked --release --example bench --target-dir target/v4 \
    && cp target/v4/release/examples/bench /usr/local/bin/bench-v4

# PGO and BOLT (bench.yml, dispatch input `pgo`): the bench example built with cargo-pgo, first
# with PGO (profile: the tiles), then PGO + BOLT (llvm-bolt from Debian's bolt-16; it works on the
# linked binary, so its LLVM need not match rustc's). The profiles are written in this RUN only,
# from a clean target, and the timing runs with `docker run`.
FROM bench AS pgo
RUN apt-get update && apt-get install -y --no-install-recommends bolt-16 \
    && rm -rf /var/lib/apt/lists/* \
    && ln -s /usr/lib/llvm-16/bin/llvm-bolt /usr/lib/llvm-16/bin/merge-fdata /usr/local/bin/ \
    && ln -s /usr/lib/llvm-16/lib/libbolt_rt_*.a /usr/local/lib/ \
    && rustup component add llvm-tools-preview \
    && CARGO_NET_OFFLINE=false cargo install cargo-pgo --locked --version 0.2.9
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-rs-pgo-target,target=/src/target \
    rm -rf target/pgo-profiles target/bolt-profiles \
    && cargo pgo build -- --example bench \
    && target/x86_64-unknown-linux-gnu/release/examples/bench tests/data/tiles 20 >/dev/null \
    && cargo pgo optimize build -- --example bench \
    && cp target/x86_64-unknown-linux-gnu/release/examples/bench /usr/local/bin/bench-pgo \
    && cargo pgo bolt build --with-pgo -- --example bench \
    && target/x86_64-unknown-linux-gnu/release/examples/bench-bolt-instrumented tests/data/tiles 20 >/dev/null \
    && cargo pgo bolt optimize --with-pgo -- --example bench \
    && cp target/x86_64-unknown-linux-gnu/release/examples/bench-bolt-optimized /usr/local/bin/bench-pgo-bolt

# Profile (bench.yml, dispatch input `profile`): callgrind over the Rust bench (release with line
# tables) and the Go oracle, a few tiles per class.
FROM bench AS profile
RUN apt-get update && apt-get install -y --no-install-recommends valgrind \
    && rm -rf /var/lib/apt/lists/*
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,id=zstd-rs-profile-target,target=/src/target \
    CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build --locked --release --example bench \
    && cp target/release/examples/bench /usr/local/bin/bench
