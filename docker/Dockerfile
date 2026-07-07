# ── Stage 1: Build Rust backend ────────────────────────────────────────────────
# 1.87+ required: cdshealpix 0.9 uses str::from_utf8_unchecked (stab. 1.87);
# pinned to match the workspace's local toolchain.
FROM rust:1.96-bookworm AS rust-builder

WORKDIR /build

# rustafits → turbojpeg → turbojpeg-sys requires cmake + nasm to build
# libjpeg-turbo from source. Matches rustafits's own release.yml setup.
RUN apt-get update \
    && apt-get install -y --no-install-recommends nasm cmake \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace manifests first for layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/athenaeum-core/Cargo.toml crates/athenaeum-core/Cargo.toml
COPY crates/athenaeum-web/Cargo.toml crates/athenaeum-web/Cargo.toml
COPY crates/jpeg-encode/Cargo.toml crates/jpeg-encode/Cargo.toml

# Copy rustafits submodule in full — it's a path dep workspace member.
# Cannot use the dummy-src trick because rustafits has a renamed [lib]
# (name = "astroimage") and two [[bin]] targets that would all need stubs.
# rustafits changes infrequently so the cache hit rate is still acceptable.
COPY rustafits/ rustafits/

# Copy solvemyastro submodule in full for the same reason — it's a workspace
# member AND a path dep of athenaeum-core (plate solving), with its own [[bin]]
# target, and it path-deps ../rustafits (layout above matches).
COPY solvemyastro/ solvemyastro/

# Create dummy lib/main to cache dependency compilation
RUN mkdir -p crates/athenaeum-core/src && echo "pub fn _dummy() {}" > crates/athenaeum-core/src/lib.rs \
    && mkdir -p crates/athenaeum-web/src && echo "fn main() {}" > crates/athenaeum-web/src/main.rs \
    && mkdir -p crates/jpeg-encode/src && echo "pub fn _dummy() {}" > crates/jpeg-encode/src/lib.rs

# Exclude athenaeum-tauri (desktop shell), catalog-builder (offline dev
# tooling), log-mcp (dev-time log query server), and perseus (headless capture
# agent — its own binary crate, not part of the web build) from the workspace
# during the Docker build. Their sources are not copied into the image, so
# leaving them as members would fail workspace manifest resolution for
# `cargo build --package athenaeum-web`.
RUN sed -i '/"crates\/athenaeum-tauri",/d' Cargo.toml \
    && sed -i '/"crates\/catalog-builder",/d' Cargo.toml \
    && sed -i '/"crates\/log-mcp",/d' Cargo.toml \
    && sed -i '/"crates\/perseus",/d' Cargo.toml

# Fetch deps, then patch zune-jpeg NEON build for aarch64.
# zune-jpeg 0.5.12 has safe fns calling unsafe NEON intrinsics without unsafe
# blocks — fails on any stable Rust for aarch64. Changing to unsafe fn is
# correct since all call sites already use unsafe blocks.
# TODO: Remove once zune-jpeg publishes a fix.
RUN cargo fetch \
    && sed -i \
       -e 's/^pub fn upsample_horizontal_neon/pub unsafe fn upsample_horizontal_neon/' \
       -e 's/^pub fn upsample_vertical_neon/pub unsafe fn upsample_vertical_neon/' \
       -e 's/^pub fn upsample_hv_neon/pub unsafe fn upsample_hv_neon/' \
       /usr/local/cargo/registry/src/*/zune-jpeg-*/src/upsampler/neon.rs

RUN cargo build --release --package athenaeum-web 2>/dev/null || true

# Copy real source and build
COPY crates/athenaeum-core/ crates/athenaeum-core/
COPY crates/athenaeum-web/ crates/athenaeum-web/
COPY crates/jpeg-encode/ crates/jpeg-encode/

# Touch source files to invalidate the dummy build cache
RUN touch crates/athenaeum-core/src/lib.rs crates/athenaeum-web/src/main.rs crates/jpeg-encode/src/lib.rs

RUN cargo build --release --package athenaeum-web

# ── Stage 2: Build frontend ───────────────────────────────────────────────────
FROM node:22-bookworm-slim AS node-builder

WORKDIR /build

# Copy package files for layer caching
COPY package.json package-lock.json* ./
RUN npm ci

# Copy frontend source and build for web target
COPY tsconfig.json tsconfig.node.json vite.config.ts postcss.config.js tailwind.config.js index.html ./
COPY src/ src/
COPY public/ public/

ENV VITE_TARGET=web
RUN npm run build:web

# ── Stage 3: Runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy built binary
COPY --from=rust-builder /build/target/release/athenaeum-web /app/athenaeum-web

# Copy built frontend
COPY --from=node-builder /build/dist /app/static

# Default environment
ENV ATHENAEUM_DB_PATH=/data/athenaeum.db
ENV ATHENAEUM_PORT=3000
ENV ATHENAEUM_STATIC_DIR=/app/static
ENV ATHENAEUM_ALLOWED_PATHS=/astro-files,/exports

EXPOSE 3000

# Data volume
VOLUME ["/data"]

CMD ["/app/athenaeum-web"]
