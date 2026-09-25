# Progress: parse-contribute-value-192

PR strategy: recommend **one PR** covering Phase 1 (the fix, self-contained, the load-bearing
change) with Phase 2 (filing the cross-repo issue + writing the local cross-repo plan doc)
folded into the same PR as a non-code addition — filing an issue and writing a plan file are
both zero-risk, and splitting them into a second PR would just add process overhead for two
commits that have no independent risk profile from each other. If a reviewer prefers Phase 2
deferred, it can trivially be pulled into its own tiny follow-up PR with no re-work.

## Repo map

Files this plan's phases touch or read, one line each:

- `crates/macp-modes/src/mode/multi_round.rs` — the whole mode: `ContributeJson` (:41-44),
  `parse_contribute_value` (:63-78, the buggy decoder, `#[doc(hidden)] pub`), `MultiRoundMode`
  (:89-113), `handle_contribute` (:163-189, sole call site of `parse_contribute_value`, :174),
  `handle_commitment` (:191-222, recomputes `ResolutionPayload` fresh every call — including
  during replay, not read from stored bytes), `#[cfg(test)] mod tests` (:225-788, 21 existing
  tests — confirmed via `grep -c '#\[test\]'`, not 20 — helpers
  `contribute_env`/`contribute_env_json`/`contribute_env_with_payload`
  :253-279, `session_with_state` :306-311). `prost::Message` is imported only inside the test
  module (`:230`), not at module scope — Phase 1's new module-level code needs its own import
  or a fully-qualified call.
- `crates/macp-core/src/session.rs` — `Session::semantics_rev` (:174) /
  `CURRENT_SEMANTICS_REV` (:99, currently `2`, doc block :67-98 documents revisions 0/1/2 for
  the Handoff mode's implicit-accept timing changes — Phase 1 adds a rev-3 bullet here);
  builder default `semantics_rev: CURRENT_SEMANTICS_REV` (:221, confirms every freshly-built
  `Session` — test or production — binds the current revision unless explicitly overridden).
- `src/replay.rs` — `replay_entry` (:85-174), `EntryKind::Incoming` branch (:92-138):
  reconstructs `Envelope.payload` from `entry.raw_payload.clone()` (:115) and calls
  `mode.on_message_at` (:133) — this is what makes `parse_contribute_value`'s decode behavior
  a replay-determinism concern, not just an accept-time one. `try_replay_from_checkpoint`
  (:36-82) — checkpoint fast path, only replays entries after the checkpoint.
  `replay_from_start` (:279-392) — restores `semantics_rev` from the recorded `SessionStart`
  log entry (`.semantics_rev(start_entry.semantics_rev)`, :349). Existing `semantics_rev`
  legacy-log fixture precedent: `legacy_rev0_handoff_history_replays_under_envelope_clock`
  and siblings (:1171-1310) — the structural model for this plan's new replay fixtures.
- `src/runtime.rs` — live acceptance path stamps the session's bound `semantics_rev` onto
  every accepted log entry (`:501` reads `session.semantics_rev`, `:573` writes it onto the
  entry) — confirms the carrier exists end-to-end, not just at `SessionStart`.
- `crates/macp-modes/Cargo.toml` — `tracing = { workspace = true }` (:19), already a
  dependency; `crates/macp-modes/src/mode/util.rs:54,61,143,152` and
  `crates/macp-modes/src/mode/quorum.rs:398` are the existing `tracing::warn!` precedent
  Phase 1's new `tracing::debug!` call follows.
- `tests/parity_contract.rs` — whole file read. Module doc (:1-35) explains the manifest
  mechanism. `HANDLED`/`ALL_SECTIONS` (:53-80). `parity_contract_path()`/
  `MACP_PARITY_CONTRACT` env override (:82-101). `ContributeVector` struct (:382-391),
  `contribute_vectors()` (:393-397), hand-rolled `hex_decode`/`hex_encode` (:399-438, no `hex`
  crate in the workspace `Cargo.lock`, deliberate). Three
  `parse_contribute_value` call sites needing the new `semantics_rev` argument in Phase 1:
  `:467` (protobuf decode), `:477` (legacy-JSON decode), `:538` (empty-payload rejection).
  `contribute_payload_vectors_round_trip_through_the_real_codec` (:453-505, hardcodes
  `vectors.len() == 4` at :456-461 — must stay 4 until Phase 2's upstream change lands and a
  future re-vendor phase updates it). `contribute_payload_first_byte_markers_match_vectors`
  (:507-530). `contribute_acceptance_empty_payload_is_rejected` (:532-544).
- `tests/parity/contract.json` — this repo's vendored, byte-identical copy of the spec repo's
  canonical manifest. Must NOT be edited in Phase 1 (would break `check_dir` CI byte-identity
  against the pinned `SPEC_REV`). `sections.contribute_payload.vectors` currently has 4
  entries at byte-lengths 6 ("deploy"), 5 ("café", UTF-8), 130, 127 — none collide.
  `sections.contribute_acceptance.empty_payload: "reject"`.
- `tests/parity/SOURCE.md` — documents the vendoring process (`cp` from the spec repo, update
  pinned commit/date) and the `check_dir` CI gate. Read this to write Phase 2's deferred
  re-vendor description.
- `.github/workflows/ci.yml` — `SPEC_REV` env var (`:39`, pinned spec-repo commit, gates
  `conformance-oracle`), `conformance-oracle` job (:580+), `check_dir` byte-diff mechanism.
  Not touched by this plan directly (only referenced/described for Phase 2's deferred
  re-vendor work).
- `crates/macp-storage/src/storage/compaction.rs` — `compact_session_log` (:13-50), invoked
  unconditionally on every terminal-session transition via `Runtime::maybe_compact_log`
  (`src/runtime.rs:1225-1271`, called at `:937,1048,1376`), replacing a session's entire log
  with one `Checkpoint` entry — the reason the replay-risk this plan's fix addresses is
  bounded to `Open` sessions and best-effort-failed compactions, not "every existing session
  forever." Central to the Context section's corrected risk narrative (round-1 review finding).
- `tests/conformance_loader.rs` — `encode_multi_round_payload` (:289-297ish) always encodes
  `Contribute` fixtures as canonical proto from a JSON `"payload": {"value": "..."}` fixture
  field — confirms `tests/conformance/*.json` fixtures are a viable place for a future
  collision-length regression fixture (not used by this plan's Phase 1, which puts its tests
  directly in `multi_round.rs` and `replay.rs` instead — noted as a lower-priority option, not
  required for acceptance).
- `tests/conformance/multi_round_happy_path.json`, `tests/conformance/multi_round_reject_paths.json`
  — existing multi_round conformance fixtures (JSON `value` shape, always proto-encoded by the
  loader). Read for context; not modified by this plan.
- `CONTRIBUTING.md:44-47` — the written ground rule this plan's Phase 1 design directly
  implements: "Changes affecting message acceptance or replay need a regression test, and —
  if they change semantics of persisted histories — a legacy-log fixture proving old logs
  still replay under their original semantics (see `Session::semantics_rev`)."
- `DECISIONS.md` — no existing entry for this issue (grepped, zero hits for
  `multi_round`/`contribute`/`parse_contribute_value`). D7 entry (:141-155+) is the citation-
  style precedent for how `/implement` should log the semantics_rev decision this plan already
  analyzed (see plan's Open questions).
- `ASSUMPTIONS.md` — no existing entry for this issue either (same grep, zero hits). The
  reverse-direction residual (Phase 1 Edge cases) should be logged here by `/implement`.
- `crates/macp-pb/build.rs` — confirms `ContributePayload` is generated via plain
  `tonic_prost_build::configure()` with no unknown-field-retention option — the reason prost's
  decode-drops-unknown-fields behavior makes the round-trip canonicality check correct without
  Python's extra `DiscardUnknownFields()` step.
- `macp-proto` crate (pinned `0.1.10` in root `Cargo.toml:65`) —
  `proto/macp/modes/multi_round/v1/multi_round.proto:16-18`: `ContributePayload { string value
  = 1; }`, single field, confirmed no other fields/oneof/repeated.
- `/Users/Shared/multiagentcoordinationprotocol/macp-sdk-python` (sibling repo, local checkout)
  — commit `3403034` (PR #77, merged fix for the same bug class, issue #69):
  `src/macp_sdk/proto_registry.py`'s `_is_canonical_proto`/`_decode_json_first_then_proto` (the
  tie-break this plan ports), `src/macp_sdk/envelope.py`'s `build_contribute_payload` docstring
  (the proto3 empty-value round-trip hole, already independently handled in macp-runtime by the
  existing empty-payload reject). `ASSUMPTIONS.md`'s "Contribute payload canonicality tie-break
  (option A vs. B)" entry (lines 117-167) — the residual's pricing precedent this plan's Phase 1
  Edge cases section cites directly.
- `/Users/Shared/multiagentcoordinationprotocol/multiagentcoordinationprotocol` (spec repo,
  local checkout) — `schemas/parity/contract.json` (canonical parity manifest;
  `sections.contribute_payload`/`contribute_acceptance` read in full this session),
  `schemas/parity/README.md` (Sections table, Versioning rules, "Open items" section
  explicitly naming this exact gap as tracked-but-unseeded work).
- `plans/parity-contract-176.md` / `plans/parity-contract-176-PROGRESS.md` — structural/style
  precedent this plan follows (Status/Delivers/Depends on/Files/Approach/Edge cases/Acceptance
  criteria/Tests/Docs per phase; `/implement`'s Phase-checkpoint-log and `/ship` sections are
  filled in by those tools, not by `/plan` — not pre-populated here).
- `plans/cross-repo/multiagentcoordinationprotocol-parse-contribute-value-192.md` — does not
  exist yet; created by Phase 2's own execution, not by this `/plan` session (per the
  cross-repo convention: `/plan` describes the phase, `/implement` executes it, including
  actually filing the GitHub issue).

## Checkpoints

### Local toolchain, confirmed broken before Phase 1 started

`cargo build -p macp-modes` and `cargo check -p macp-modes` both fail identically at the
linker step for `macp-pb`'s build script (`ld: multiple errors: tapi error: malformed file
... MacOSX27.0.sdk/usr/lib/libiconv.2.tbd:4:20: error: unknown architecture`). Diagnosed as
a genuine Xcode Command Line Tools / SDK mismatch on this machine (`xcode-select -p` →
`/Library/Developer/CommandLineTools`, CLT package version `26.6.0.0.1781586589`, `sw_vers`
→ macOS `26.6.2` build `25G83` — the installed CLT (26.6) is paired with a `MacOSX27.0.sdk`
it doesn't recognize the `.tbd` architecture-variant strings of). Unrelated to any change in
this plan; not fixed (would need `sudo` and touches the system-wide toolchain outside this
repo's scope). **`cargo fmt --check` works standalone** (pure source-level, no linking) and
was used throughout as the one real local signal; every other check (`build`/`test`/`clippy`)
relies on CI (Linux runners, unaffected) as the actual gate, consistent with how every prior
PR in this session was verified.

### Phase 1 — 2026-09-25 — DONE, implemented and self-reviewed; CI is the verification gate

Files touched: `crates/macp-core/src/session.rs` (`CURRENT_SEMANTICS_REV` 2→3 + rev-3 doc
bullet), `crates/macp-modes/src/mode/multi_round.rs` (`parse_contribute_value`'s
`semantics_rev` parameter + round-trip tie-break + docstring fix + 10 new tests),
`tests/parity_contract.rs` (3 call sites pass `CURRENT_SEMANTICS_REV`, import added),
`src/replay.rs` (2 new legacy-log replay fixtures, matching the existing handoff-fixture
structural precedent at `:1171-1360`).

Every byte-level claim in the plan (the 13/32/123 collision lengths, the reverse-direction
residual's construction, the exhaustive sweep's 5-shape/1..=300 differential behavior, the
non-canonical-proto regression pin) was independently re-derived this session via direct
Python simulation of the exact varint/JSON-whitespace arithmetic before being encoded into
Rust tests — not merely copied from the plan's prose. `cargo fmt --check` passes clean
(`cargo fmt` applied once to fix one formatting diff in the exhaustive-sweep test, an
`if`/`else` expression rustfmt collapses onto fewer lines than my first draft).

No `semantics_rev ==` comparison exists anywhere in the codebase (re-confirmed via a fresh
workspace-wide grep this session, zero hits), so the constant bump is additive for every
other consumer as the plan's Fable consult already established.

`DECISIONS.md` D51 logged (citing the plan's Fable consult, D7-style, per the plan's own
"Open questions" instruction — not routed through `/reconcile`). `ASSUMPTIONS.md` gained one
entry: the reverse-direction residual, `UNCONFIRMED` in the narrow "no evidence yet that zero
real traffic hits it" sense (design question itself is settled, see D51).

**Verification gate:** not yet run as a fresh-Opus `/implement` gate at the time this
checkpoint was written — `cargo test`/`cargo clippy` cannot execute on this machine, so the
gate is deferred until after this PR's CI actually runs the suite, per this session's
established pattern (CI, not local execution, has been the real verification gate for every
PR this whole session). See the next checkpoint for the actual gate outcome once CI is green.

### Phase 2 — 2026-09-25 — DONE

`plans/cross-repo/multiagentcoordinationprotocol-parse-contribute-value-192.md` written, with
three vectors (lengths 13, 32, 123) regenerated from the actual shipped Phase 1 code via a
direct Python re-encoding (not copied from the plan's Context section numbers), each
independently verified against the plan's own derivation. `tests/parity/contract.json` and
`tests/parity_contract.rs` untouched (re-vendor explicitly deferred, per the plan). GitHub
issue filing and its exact URL: see the next checkpoint (filed after the PR branch was pushed,
so the issue could link a real blob URL rather than a placeholder).
