# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-modes-v0.6.0...macp-modes-v0.7.0) - 2026-08-31

### Added

- implement RFC-MACP-0013 canonical commitment hash and tighten the §7.3.1 supersedes check ([#108](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/108))

### Other

- re-export macp-core, clear RUSTSEC advisories, audit the second lockfile ([#117](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/117))
- *(deps)* bump the minor-and-patch group across 1 directory with 16 updates ([#113](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/113))

## [0.6.0](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-modes-v0.5.0...macp-modes-v0.6.0) - 2026-07-11

### Fixed

- green the new CI gates (otel build, rustdoc, coverage)

### Other

- prune redundant tests, de-flake the harness, fill coverage gaps
