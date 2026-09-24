# Builds a container that can compile and run the integration_tests crate
# (Tier 1 protocol, Tier 2 Rig tools, Tier 3 real-LLM E2E) against a runtime
# binary built from the same source tree. Mirrors what ci.yml's `integration`
# and integration-tests.yml jobs do (MACP_TEST_BINARY subprocess mode) so
# behavior matches CI exactly -- this is a portable way to run that same
# matrix locally via `docker compose`, not a different test topology.
#
# Several tier1 tests (TLS, limits, persistence, promotion, JWT) spawn their
# *own* dedicated server subprocess via ServerManager::start_with_env rather
# than using the shared endpoint, so a real local binary must be present in
# the image -- a network-hosted MACP_TEST_ENDPOINT would not satisfy those.
#
# Build context must be the repository root (see docker-compose.yml), since
# integration_tests/Cargo.toml depends on the parent crate via `path = ".."`.
FROM rust:1.98-bookworm

RUN apt-get update && apt-get install -y protobuf-compiler && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock build.rs rust-toolchain.toml ./
COPY crates/ crates/
COPY src/ src/
COPY benches/ benches/
COPY integration_tests/ integration_tests/

# Build the runtime binary the tests spawn, then pre-build the test binaries
# themselves so `docker compose run` pays compile time once at image build,
# not on every test invocation.
RUN cargo build
RUN cd integration_tests && cargo metadata --locked --manifest-path Cargo.toml --format-version 1 > /dev/null && cargo build --tests

WORKDIR /app/integration_tests
ENV MACP_TEST_BINARY=/app/target/debug/macp-runtime
ENV RUST_LOG=warn

ENTRYPOINT ["cargo", "test"]
CMD ["--test", "tier1", "--test", "tier1_jwt", "--test", "tier2", "--", "--test-threads=1"]
