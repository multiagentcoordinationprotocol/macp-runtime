.PHONY: setup build test test-integration test-conformance test-all fmt clippy check audit coverage test-integration-grpc test-integration-agents test-integration-e2e test-integration-hosted

## First-time setup: configure git hooks
setup:
	git config core.hooksPath .githooks
	@echo "Git hooks configured."

build:
	cargo build

test:
	cargo test

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test-integration:
	cargo test --test '*'

test-conformance:
	cargo test conformance

test-policy:
	cargo test policy

test-all: fmt clippy test test-integration test-conformance test-policy

coverage:
	cargo tarpaulin --all-targets --out html

audit:
	cargo audit

check: fmt clippy test

## Integration tests (gRPC, Rig agents)
test-integration-grpc:
	cd integration_tests && cargo test --test tier1 -- --test-threads=1

test-integration-agents:
	cd integration_tests && cargo test --test tier2 -- --test-threads=1

test-integration-e2e:
	cd integration_tests && cargo test -- --ignored --test-threads=1

test-integration-hosted:
	cd integration_tests && cargo test -- --test-threads=1
