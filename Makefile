.PHONY: setup build test test-integration test-conformance test-all fmt clippy check audit coverage test-integration-grpc test-integration-agents test-integration-e2e test-integration-hosted doppler-sync

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

# Real-LLM keys come from the environment: `doppler run -- make <target>`
# (Doppler macp-runtime/dev). Without Doppler, a gitignored root .env is read
# for any key the environment does not already set (.env.example).
LOAD_DOTENV = if [ -f .env ]; then while IFS= read -r line; do \
	  case "$$line" in ''|'\#'*) continue ;; esac; k="$${line%%=*}"; \
	  [ -n "$$(printenv "$$k")" ] || export "$$line"; done < .env; fi;

test-integration-e2e:
	@$(LOAD_DOTENV) cd integration_tests && cargo test -- --ignored --test-threads=1

test-integration-hosted:
	@$(LOAD_DOTENV) cd integration_tests && cargo test -- --test-threads=1

## Push the hosted service's config from Doppler to Railway (dry run; ARGS=--apply to push)
doppler-sync:
	deploy/doppler-sync.sh $(ARGS)
