# http-listener — PROGRESS

Plan: `plans/http-listener.md`. Suggested branch: `feat/http-listener`, based on main a8c3ffd (0.11.0, R1 + R2 merged).
Spec: normative at `99f3cfa` (`99f3cfa090b4bfb72b3063ca0134ec661436c3ef`; HTTP binding #234 = `122ad47`, JSON
mapping #233 = `018e1fc`). `SPEC_REV` moved from `0f77753` to `99f3cfa` in P0 (05da2ad).

## Repo map
- `Cargo.toml`: `[workspace.package]` 0.11.0; `default = []`; `otel` feature (`:78`) uses optional `tower-http`,
  `http`, `tower`, so a feature named `http` would clash with the existing optional `http` dependency (`:116`);
  dev-dep `macp-conform` (`:144`). In the lock already: axum 0.8.8, hyper 1.8.1, hyper-util 0.1.20,
  tokio-rustls 0.26.4, rustls 0.23.46, tower 0.5.3, tower-http 0.6.8, http-body-util 0.1.3. Absent: prost-reflect,
  pbjson.
- `build.rs`: single tonic codegen run for `macp.v1` with `.extern_path(".macp.v1","::macp_pb::pb")`, writes
  `macp_descriptor.bin` (reflection only, `src/lib.rs:18`). P3 adds a second run with its own `out_dir`.
- `crates/macp-pb/build.rs`, `src/lib.rs:14-44`: all nine protos; mode modules `decision_pb` … `multi_round_pb`
  (packages `macp.modes.<mode>.v1`). Seam crate: untouched.
- `crates/macp-core/src/error.rs`: `MacpError` (`#[non_exhaustive]`, `HistoryUnavailable` unit variant `:72-73`),
  `ERROR_CODE_REGISTRY` with the HTTP column (`:183-235`), `registry_entry` `:240`, `http_status` `:256`. Seam: untouched.
- `crates/macp-auth/src/security.rs`: rate buckets in `Arc` (`:88`), `max_payload_bytes` `:90`, `bearer_token`
  `:394-406` (case-sensitive `Bearer `, `x-macp-token` fallback `:402`), `authenticate_metadata` `:408`,
  `has_configured_auth` `:505`, `enforce_rate_limit` `:517` (start vs message buckets).
- `crates/macp-storage/src/log_store.rs:106-137`: `get_incoming_after` → `(ordinal, entry)` or `Err(base)`; P2 adds
  `get_incoming_after_with_high_water`.
- `src/main.rs`: shutdown awaits only `ctrl_c` (`:501`, `:602`; P1b adds SIGTERM via a shared watch, P4 subscribes); `validate_env_config` `:19`; tracing/otel init; `SecurityLayer::from_env` `:388`; insecure/auth gate
  `:390-410`; TLS `:447-460`; payload cap `:487-491`; drain `:496`; serve `:506-510`; Prometheus listener `:547-596`
  (second-listener precedent); drain deadline `:601-616`.
- `src/server.rs`: `MacpServer` `:36-49`; `validate_envelope_shape` `:135-173`; `make_error_ack` `:216`;
  `apply_authenticated_sender` `:278`; `authenticate_send_request` `:289-319` (auth `:296-298` runs after the shape check `:1265`; mode/rate `:299-311`); `authenticate_session_access` `:321`;
  `should_skip_replayed` `:376`; `process_subscribe_frame` `:608-693` (compaction inline INVALID_ENVELOPE `:676-688`,
  fixed in P1); `build_stream_session_stream` `:695-939`; `coded_status_ref` `:981-997`; trait impl `:1167`;
  `initialize` `:1168`; `send` `:1246-1315`; `get_manifest` `:1543-1568` (`transport_endpoints` `:1565`);
  `stream_session` `:1640`; `watch_signals` `:1720`; `what_if_replay` (only HISTORY_UNAVAILABLE producer) `:2195`.
- `src/runtime.rs`: buses `:110-111`; `subscribe_session_stream` `:673`; `subscribe_signals` `:680`;
  `subscribe_session_lifecycle` `:684`; `get_session_envelopes_after` `:695-724` (drops ordinals);
  `publish_accepted_envelope` `:726` (callers `:1096`, `:1287`, `:1438`); `get_session_checked` `:1477`.
- `src/stream_bus.rs`: per-session `broadcast::Sender<Envelope>`, capacity 256, `remove_if_unused`.
- `src/policy_engine.rs:60,64`: uncoded `permission_denied` (coded in P1; reached via `server.rs:365`).
- `src/telemetry/grpc.rs`: gRPC SERVER span layer, `METHODS` `:71` (route table pin target), `HeaderExtractor` `:148`.
  P9 adds `src/telemetry/http.rs`.
- `src/http/` (new, feature `http-listener`): `json/` (P3), `config.rs`/`server.rs`/`tls.rs` (P4),
  `dispatch.rs`/`errors.rs`/`metadata.rs` (P5), `sse.rs` (P6), `cors.rs` (P7), `discovery.rs` (P8).
- `crates/macp-conform/`: `client.rs:17-28` (http(s) URLs mean gRPC), `grpc.rs` (`GrpcTarget` `:47`, history via
  subscribe `:115-167`), `target.rs` (`ConformanceTarget` `:180`), `cli.rs` (`--target` `:75`), `encode.rs:68,100`
  (mode message tables). P10 adds `http.rs` behind a new `http` feature.
- `tests/`: `parity_contract.rs:311-356` (needs `ReceiptUnavailable` on re-pin); `parity/`, `conformance/`,
  `vectors/receipt/`, `otel/` each with `SOURCE.md`; `otel_binary.rs` (binary-test precedent for P4); `golden/`. New: `grpc_refusal_snapshot.rs` + `golden/grpc_refusals/` (P2 item 0),
  `tests/http/` (vendored examples + error schema + negatives), `http_json.rs`, `http_binary.rs`, `http_unary.rs`,
  `http_sse.rs`, `otel_http.rs`, `conformance_http.rs`.
- `integration_tests/`: separate workspace and lock; `tier1_protocol/test_passive_subscribe.rs` (P1 test).
- `.github/workflows/ci.yml`: `SPEC_REV` `:39`; deps-isolation `:261-371` (macp-conform default forbids reqwest
  `:317-323`); features job `:446`; integration `:547`; conformance-oracle `:618` (`check_dir` `:670-674`);
  conform-cli `:781-876`.
- `.github/workflows/spec-drift.yml`: watched paths `:16-17`, `:111`, `:129`.
- `.github/workflows/docker.yml:297-300`, `Dockerfile:21-22`: `CARGO_FEATURES` build arg (becomes `otel,http-listener`).
- `docs/`: `API.md`, `deployment.md` (`:222`, `:534` compaction), `sdk-guide.md:114` (inline compaction frame, P1),
  `examples.md`, `testing.md`, `architecture.md`; `MIGRATING.md`; `CLAUDE.md` (features/env tables).
- Sibling `../macp-control-plane/src/runs/stream-consumer.service.ts:298-320`: matches the compaction sentence (kept
  by P1). Notify only.

## Phase log

| Phase | Risk | Status | Commit | Verified | Notes |
|-------|------|--------|--------|----------|-------|
| P0 spec re-pin + vendoring | simple | DONE | 05da2ad | Opus, 1 round, PASS (batched with P1) | 14 vendored files byte-equal to 99f3cfa; DRIFT on 1-byte mutation |
| P1 gRPC HISTORY_UNAVAILABLE | simple | DONE | ad34084 | Opus, 1 round, PASS (batched with P0) | workspace 1132 pass; tier1 passive_subscribe 7 pass |
| P1b SIGTERM shared shutdown | simple | DONE | 4cc11f7 | Opus, 1 round, GAPS→fixed inline (MIGRATING section pasted into 0.11.0/0.10.0, stale env-table refs) | workspace 1136 pass; shutdown_binary 4/4 x3 |
| P2 server seams | complex | DONE | af6c924 + edf68f6 | Opus, 1 round, PASS (G1 doc(hidden), G2 P6 note closed inline; G3 bound-stream ordinal untested, gRPC discards ordinals) | workspace 1146, otel 441, tier1 137; semver-checks clean vs 0.11.0 |
| P3 canonical JSON codec | complex | DONE | (this commit) | Opus, 2 rounds: R1 GAPS (deps-isolation SIGPIPE false-pass; §10.9 silent rounding; minor), R2 GAPS (ctree swallowed cargo tree failure, fixed inline) | workspace 1146; http-listener 399 (http_json 15, codec 23) |
| P4 listener skeleton | complex | DONE | (this commit) | Opus, 2 rounds: R1 GAPS (idle socket pins permit via un-deadlined auto sniff; quoted content-type params), R2 PASS (accepted risk: PING-answering idle h2 client) | workspace 1148; http-listener 431 (http_binary 9 real-binary) |
| P5 unary + errors + Send layers | complex | DONE | (this commit) | Opus, 2 rounds: R1 GAPS (bad Envelope → 400 vs §4.4 in-band; METHODS pin dead in CI; free_port TOCTOU flake), R2 PASS | workspace 1150; http-listener 454 ×2; http_binary 10/10 ×3 |
| P6 SSE + resume | complex | DONE | (this commit) | Opus, 2 rounds: R1 GAPS (race-test hook inert; keepalive knob silent no-op; sender_authenticated=false untested), R2 PASS (mutation-checked race test) | workspace 1150; http-listener 480 ×2; http_sse 16/16 ×5 |
| P7 CORS | simple | TODO | | | |
| P8 discovery | simple | TODO | | | |
| P9 observability | simple | TODO | | | |
| P10 macp-conform over HTTP | complex | TODO | | | |
| P11 docs, image, release notes | simple | TODO | | | |

Plan review round 1 (REVISE, 17 findings) applied to the plan on 2026-10-31; see `## Plan review` there. Round 2 (A: rate limits in band, B: SIGTERM moved to P1b in PR A, C: stale P2 item dropped) applied; verdict SOUND after 2 rounds.

- 2026-10-31 PR A ship gate (Fable): GAPS 2 (MIGRATING heading must be literal `## 0.12.0`; PROGRESS refs) fixed; one-way door (gRPC compacted resume) ship as-is, no user input needed: spec-mandated at both pins, no deployed consumer breaks.
- pushed feat/http-listener 8a672bf
- PR #269 opened: https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/269
- 2026-10-31 merged #269 (52e05f7) — PR A (P0-P2)
- 2026-10-31 filed #274 (security: WatchPolicies unauthenticated, pre-existing on gRPC; found by P6 verifier)
