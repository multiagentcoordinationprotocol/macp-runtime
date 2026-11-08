# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.14.1](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-cli-v0.14.0...macp-cli-v0.14.1) - 2026-11-08

### Fixed

- *(deploy)* set the playground and token-issuer keys in the self-host compose ([#306](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/306))

### Other

- *(deploy)* pin auth-service 2.0.0 and playground 0.3.0 ([#310](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/310))
- *(deploy)* pin control-plane 0.8.0 and ui-console 0.3.0 ([#309](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/309))
- *(integration)* cover 0.14.0 Handoff readiness, FORBIDDEN annotations, 0.13.2 task reassignment code; bump stale 0.10.0 image pins ([#304](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/304))

## [0.14.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-cli-v0.13.2...macp-cli-v0.14.0) - 2026-11-07

### Other

- update Cargo.lock dependencies

## [0.13.2](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-cli-v0.13.0...macp-cli-v0.13.2) - 2026-11-07

### Other

- update Cargo.lock dependencies

## [0.13.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-cli-v0.12.1...macp-cli-v0.13.0) - 2026-11-01

### Other

- update Cargo.lock dependencies

## [0.12.1](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-cli-v0.12.0...macp-cli-v0.12.1) - 2026-11-01

### Other

- *(deploy)* 15s stop grace for the compose runtime ([#277](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/277))

## [0.12.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-cli-v0.11.0...macp-cli-v0.12.0) - 2026-11-01

### Added

- *(http)* HTTP/JSON binding (RFC-MACP-0006 §4) — PR B ([#275](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/275))

## [0.11.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-cli-v0.10.1...macp-cli-v0.11.0) - 2026-10-30

### Added

- *(receipts)* issue and serve decision receipts (RFC-MACP-0013 §13) ([#264](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/264))

## [0.10.1](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-cli-v0.10.0...macp-cli-v0.10.1) - 2026-10-26

### Fixed

- *(deploy)* pin control-plane 0.7.1 ([#260](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/260))
- *(deploy)* pin control-plane 0.7.0 and ui-console 0.2.1 ([#257](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/257))
- *(cli)* `macp up` runs this release's runtime; name empty gRPC errors ([#255](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/255))

## [0.10.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-cli-v0.9.0...macp-cli-v0.10.0) - 2026-10-26

### Added

- *(cli)* `macp replay` — what-if replay from a data dir, a log file or a runtime (W4.4) ([#250](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/250))
- *(cli)* `macp session tail` (W4.4) ([#249](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/249))
- *(server)* WhatIfReplay RPC (RFC-MACP-0012 §8.1) ([#248](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/248))
- *(cli)* `macp` command line — verify, up/down/status, conform (W4.4) ([#246](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/246))
