# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
