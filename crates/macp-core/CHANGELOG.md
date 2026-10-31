# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.11.1](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.10.1...macp-core-v0.11.1) - 2026-10-31

### Added

- *(receipts)* issue and serve decision receipts (RFC-MACP-0013 §13) ([#264](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/264))

### Other

- release v0.11.0 ([#262](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/262))

## [0.11.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.10.1...macp-core-v0.11.0) - 2026-10-30

### Added

- *(receipts)* issue and serve decision receipts (RFC-MACP-0013 §13) ([#264](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/264))

## [0.10.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.9.0...macp-core-v0.10.0) - 2026-10-26

### Added

- *(server)* WhatIfReplay RPC (RFC-MACP-0012 §8.1) ([#248](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/248))

### Other

- *(spec)* pin SPEC_REV to 60262b8 (spec #196); parity contract 1.4.0 ([#232](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/232))

## [0.9.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.8.8...macp-core-v0.9.0) - 2026-10-17

### Added

- [**breaking**] Seam upstream 0.9.0 — library acceptance/step/replay, S1/S4/S5/S6/S7/S12/S13 ([#229](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/229))

### Fixed

- fix(policy)+feat(core): Seam upstream 0.8.9 slice — value-domain admission, schema_version, RFC-0013 verify ([#228](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/228))

## [0.8.8](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.8.7...macp-core-v0.8.8) - 2026-10-04

### Other

- document the second resume force-expire trigger (semantics_rev 2) ([#222](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/222))

## [0.8.3](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.8.2...macp-core-v0.8.3) - 2026-09-25

### Fixed

- *(multi_round)* tie-break canonical-proto/JSON Contribute collision ([#192](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/192)) ([#196](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/196))

## [0.8.1](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.8.0...macp-core-v0.8.1) - 2026-09-22

### Added

- feat/parity-contract-176 — proof-of-concept parity-contract consumer (issue #176) ([#179](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/179))

### Fixed

- *(macp-core)* silence assertions_on_constants at session.rs:1402 ([#181](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/181))

## [0.8.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.7.6...macp-core-v0.8.0) - 2026-09-20

### Added

- *(handoff)* [**breaking**] record the implicit accept at semantics_rev 2; mode-state records no longer exhaustively constructible ([#171](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/171))

## [0.7.6](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.7.5...macp-core-v0.7.6) - 2026-09-12

### Fixed

- *(policy)* align with spec #99 schema_version 3 and the #109-#113 follow-ups ([#165](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/165))

## [0.7.5](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.7.4...macp-core-v0.7.5) - 2026-09-11

### Fixed

- *(policy)* close five fail-open defects in policy evaluation ([#159](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/159))

## [0.7.1](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.7.0...macp-core-v0.7.1) - 2026-08-31

### Other

- *(changelog)* record the 0.7.0 release ([#122](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/122))

## [0.7.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.6.0...macp-core-v0.7.0) - 2026-08-31

### Added

- implement RFC-MACP-0013 canonical commitment hash and tighten the §7.3.1 supersedes check ([#108](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/108))

### Other

- release v0.6.1 ([#101](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/101))
- release v0.6.1 ([#100](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/100))

## [0.6.1](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-core-v0.6.0...macp-core-v0.6.1) - 2026-07-12

### Other

- release v0.6.1 ([#100](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/100))
