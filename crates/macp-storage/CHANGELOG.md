# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.12.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-storage-v0.11.0...macp-storage-v0.12.0) - 2026-11-01

### Added

- *(otel)* OpenTelemetry spans, semconv metrics and durable session anchor (R2) ([#268](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/268))

### Fixed

- *(stream)* [**breaking**] R4 PR A — spec re-pin, gRPC HISTORY_UNAVAILABLE on compacted resume, SIGTERM drain, shared server seams ([#269](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/269))

## [0.11.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-storage-v0.10.1...macp-storage-v0.11.0) - 2026-10-30

### Added

- *(receipts)* issue and serve decision receipts (RFC-MACP-0013 §13) ([#264](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/264))

## [0.8.5](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-storage-v0.8.4...macp-storage-v0.8.5) - 2026-09-29

### Fixed

- *(runtime)* correct SessionResumePayload.banked_ms to the normative quantity ([#208](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/208))

## [0.8.1](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-storage-v0.8.0...macp-storage-v0.8.1) - 2026-09-22

### Other

- use tempfile::TempDir for scratch dirs in policy/storage tests ([#177](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/177))

## [0.8.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-storage-v0.7.6...macp-storage-v0.8.0) - 2026-09-20

### Added

- *(handoff)* [**breaking**] record the implicit accept at semantics_rev 2; mode-state records no longer exhaustively constructible ([#171](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/171))

## [0.7.6](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-storage-v0.7.5...macp-storage-v0.7.6) - 2026-09-12

### Other

- *(server)* bound the WatchSessions initial sync ([#161](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/161))

## [0.7.1](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-storage-v0.7.0...macp-storage-v0.7.1) - 2026-08-31

### Other

- *(changelog)* record the 0.7.0 release ([#122](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/122))

## [0.7.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-storage-v0.6.0...macp-storage-v0.7.0) - 2026-08-31

### Added

- paginate ListSessions (page_size, opaque page_token, next_page_token) ([#116](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/116))

## [0.6.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-storage-v0.5.0...macp-storage-v0.6.0) - 2026-07-11

### Other

- prune redundant tests, de-flake the harness, fill coverage gaps
- *(plans)* prune fully-implemented plans; close the two audit findings
