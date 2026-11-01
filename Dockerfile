# Stage 1: Build
# Keep in sync with rust-toolchain.toml (channel pin).
FROM rust:1.99-bookworm AS builder

RUN apt-get update && apt-get install -y protobuf-compiler && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the full workspace (root package + member crates) and build. The root
# macp-runtime package and all crates/* members must be present before any
# cargo invocation so the workspace resolves and the macp-runtime binary target
# (referenced by default-run) exists.
COPY Cargo.toml Cargo.lock build.rs ./
COPY crates/ crates/
COPY src/ src/
# benches/ is declared as a [[bench]] target in Cargo.toml; cargo refuses to
# parse the manifest if the file is missing, even for a release build.
COPY benches/ benches/
# Optional Cargo features, e.g. `--build-arg CARGO_FEATURES=otel` for the
# OpenTelemetry export path. Empty (the default) builds the plain binary.
ARG CARGO_FEATURES=""
RUN cargo build --release ${CARGO_FEATURES:+--features "$CARGO_FEATURES"}

# Stage 2: Minimal runtime image
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --shell /bin/bash macp
WORKDIR /home/macp
# Created as `macp` so a named volume mounted here inherits that ownership.
RUN install -d -o macp -g macp /home/macp/.macp-data

COPY --from=builder /app/target/release/macp-runtime /usr/local/bin/macp-runtime
# The container starts as root only so the entrypoint can hand a root-owned
# MACP_DATA_DIR (a platform volume or a bind mount) to `macp`; it then drops
# to `macp` with setpriv, so the runtime never runs as root (#258).
COPY docker/entrypoint.sh /usr/local/bin/macp-entrypoint

ENV MACP_BIND_ADDR=0.0.0.0:50051
# NOTE: no MACP_ALLOW_INSECURE here. The runtime refuses to start without
# configured auth + TLS unless the operator explicitly opts into dev mode:
#   docker run -e MACP_ALLOW_INSECURE=1 ...   (local development only)
ENV MACP_DATA_DIR=/home/macp/.macp-data

EXPOSE 50051

ENTRYPOINT ["macp-entrypoint"]
