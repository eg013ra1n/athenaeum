# ── Stage 1: Build Rust backend ────────────────────────────────────────────────
# 1.87+ required: cdshealpix 0.9 uses str::from_utf8_unchecked (stab. 1.87);
# pinned to match the workspace's local toolchain.
FROM rust:1.96-bookworm AS rust-builder

WORKDIR /build

# No `apt-get install nasm cmake` here on purpose. It used to be required
# because JPEG encoding bound the C libjpeg-turbo via turbojpeg-sys, which
# builds the library from source. The encoder is now the pure-Rust
# libjpeg-turbo-rs (inside the rustafits submodule), and turbojpeg-sys — the
# workspace's only user of the `cmake` crate — is out of Cargo.lock entirely,
# so this stage needs nothing beyond the Rust toolchain the base image ships.
# If a future dep reintroduces a C build step, add it back here rather than
# assuming it exists.

# Copy workspace manifests first for layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/athenaeum-core/Cargo.toml crates/athenaeum-core/Cargo.toml
COPY crates/athenaeum-web/Cargo.toml crates/athenaeum-web/Cargo.toml

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
    && mkdir -p crates/athenaeum-web/src && echo "fn main() {}" > crates/athenaeum-web/src/main.rs

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

# A `sed` patch of zune-jpeg's NEON upsampler used to live here (0.5.12 had
# safe fns calling unsafe intrinsics — an aarch64 build failure on any stable
# Rust). It is gone for two independent reasons: `608ae350` moved the JPEG
# encoder off the `image` crate (whose `jpeg` feature was what pulled
# zune-jpeg), so zune-jpeg is no longer in Cargo.lock at all — that stayed true
# through the later move to libjpeg-turbo-rs — and upstream fixed the
# signatures in 0.5.15. Leaving the patch in place was
# actively fatal, not merely dead: with the crate unfetched the glob matched
# nothing, sed got the literal pattern, and this RUN failed the whole build.
RUN cargo fetch

RUN cargo build --release --package athenaeum-web 2>/dev/null || true

# Copy real source and build
COPY crates/athenaeum-core/ crates/athenaeum-core/
COPY crates/athenaeum-web/ crates/athenaeum-web/

# Touch source files to invalidate the dummy build cache
RUN touch crates/athenaeum-core/src/lib.rs crates/athenaeum-web/src/main.rs

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
