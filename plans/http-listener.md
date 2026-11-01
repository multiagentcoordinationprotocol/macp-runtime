# HTTP/JSON listener (Wave 5 R4, RFC-MACP-0006 §4)

Branch `feat/http-listener` (suggested), based on `main` a8c3ffd (R1 receipts #264 and R2 otel #268 merged,
workspace version 0.11.0).

## Context

### Inputs and authority

- **Spec (normative, wins over the brief):** spec repo `origin/main` = `99f3cfa`
  (`99f3cfa090b4bfb72b3063ca0134ec661436c3ef`). The HTTP binding landed in `122ad47` (#234), the canonical JSON
  mapping in `018e1fc` (#233). Files that matter, all read at `99f3cfa`:
  - `rfcs/RFC-MACP-0006-transport-bindings.md` §4 (`:291-379`): §4.1 paths (`:297`), §4.2 bodies (`:309`), §4.3
    streaming (`:317`), §4.4 Send two layers (`:333`), §4.5 request-level errors (`:341`), §4.6 auth (`:353`),
    §4.7 Initialize (`:357`), §4.8 retries (`:361`), §4.9 limits (`:365`), §4.10 CORS (`:371`), §4.11 discovery
    (`:375`); §3.2 compaction rule (`:178`, HISTORY_UNAVAILABLE on **both** bindings); §8 (`:413`).
  - `rfcs/RFC-MACP-0001-core.md` §10 (`:491-611`), especially §10.8 deviations (`:543`), §10.9 64-bit (`:557`),
    §10.10 defaults (`:565`), §10.11 structural transforms (`:571`), §10.12 payload selection (`:581`), §10.13
    deterministic conversion (`:590`).
  - `rfcs/RFC-MACP-0005-discovery-and-manifests.md` §6 (transport endpoints) and §7.1 (well-known URL, media type
    `application/macp-manifest+json`).
  - `registries/error-codes.md` (HTTP column; HISTORY_UNAVAILABLE `:33` now carries
    `{"lowest_available_sequence": N}` details "on every binding").
  - `schemas/json/macp-http-error.schema.json` and its negative fixtures `schemas/json/tests/invalid-http-errors/`.
  - `examples/http/{send,errors,stream_session_subscribe,watch_signals}.json` (informative worked exchanges).
  - There are **no** HTTP conformance fixtures: `git diff --stat 0f77753 99f3cfa -- schemas/conformance` is empty.
    The conformance pack is transport-agnostic; running it "over HTTP" means a new HTTP target in the runner.
- **Brief:** `../multiagentcoordinationprotocol/plans/cross-repo/macp-runtime-wave5.md` R4 (`:40-45`) and Seam
  constraints S-a..S-e (`:5-10`).

### Honest current state (read on a8c3ffd, not assumed)

**There is no HTTP surface at all.** The only non-gRPC listener is the hand-rolled Prometheus responder
(`src/main.rs:547-596`), a precedent for a second `tokio::spawn`ed listener but not for TLS, auth or shutdown.

**Server shape** (`src/server.rs`, 5103 lines):
- `MacpServer` (`:36-49`) is `Clone` and holds `Arc<Runtime>`, `SecurityLayer`, `policies_read_only`,
  `policy_engine`. `SecurityLayer` keeps its rate buckets behind `Arc<RateBucket>`
  (`crates/macp-auth/src/security.rs:88`), so a clone handed to a second listener **shares** rate limits.
- Every RPC is a `MacpRuntimeService` trait method (`:1167-2293`) taking `tonic::Request<T>`. Auth reads only
  `request.metadata()` (`SecurityLayer::authenticate_metadata`, `security.rs:408`; bearer parsing `:394-406`, which
  also accepts `x-macp-token` `:402` and matches the scheme case-sensitively, `strip_prefix("Bearer ")`).
- Unauthenticated today: `Initialize` (`:1168`), `GetManifest` (`:1543`), `GetReceiptKeys` (`:1613`),
  `ListModes` (`:1622`), `ListRoots` (`:1631`), `ListExtModes` (`:2002`), `WatchModeRegistry` (`:1665`),
  `WatchRoots` (`:1695`). Every other RPC calls `authenticate_metadata` or `authenticate_session_access`.
- Error mapping is already registry-driven: `coded_status_ref` (`:981-997`) stamps `macp-error-code` and
  `macp-error-details-bin`; `macp_core::error::ERROR_CODE_REGISTRY` already carries the **HTTP column**
  (`crates/macp-core/src/error.rs:183-235`, `registry_entry` `:240`, `MacpError::http_status` `:256`). Two
  `Status`es are uncoded: `policy_engine::require_allow` (`src/policy_engine.rs:60`, `:64`).

**`Send` is single-layer today, and HTTP needs two.** `send` (`:1246-1315`) puts **every** failure in the Ack,
including a bad credential (`authenticate_metadata` inside `authenticate_send_request`, `:289-319`) and the
per-sender message rate limit (`:309-311`). RFC-0006 §4.4 makes a bad credential `401` request-level on HTTP
(as are a body over the cap, a non-`SendRequest` body, and a "request rate limit" — defined as a check "made before
the Envelope is decoded"), while a sender mismatch (`apply_authenticated_sender` returns `Unauthenticated`,
`:278-287`) is a decision about the decoded Envelope and stays in band. **The brief's "call the trait method"
cannot produce §4.4 for `Send`**: classifying by the Ack's code would turn a spoofed sender into a 401. A
small refactor that tags where each refusal came from is needed (P2). **Ordering, too:** gRPC `send` runs
`validate_envelope_shape` (`:1265`) *before* `authenticate_metadata` (`:296-298`), so an unauthenticated
malformed Envelope gets an in-band `INVALID_ENVELOPE`/`PAYLOAD_TOO_LARGE`. §4.4 makes the credential check precede
Envelope decoding, so HTTP authenticates from headers before reading the body and hands the identity to the seam.
The per-sender message and SessionStart buckets are keyed on `message_type` and run after the sender check and
`authorize_mode` (`:299-311`): they are decisions about the decoded Envelope, so on HTTP they stay **in band**
(`200` Ack `RATE_LIMITED`), exactly where gRPC reports them today (the Ack, `:1246-1315`; RFC-0006 §3.1 makes
`SendResponse.ack` the per-message rejection surface). A request-level `429` on `Send` is reserved for a future
pre-decode per-credential limit, of which there is none today. Other RPCs' rate limits (`GetDecisionReceipt`
`:1596`, `WhatIfReplay` `:2176`) are already gRPC `RESOURCE_EXHAUSTED` statuses, which map to HTTP `429` through
the registry (`error.rs:204`).

**The subscribe path does not fit SSE as-is, and gRPC violates §3.2 today.**
- `process_subscribe_frame` (`:608-693`) reports an unknown session as a terminal status (`:627-635`), a
  forbidden caller as an **inline** frame with the stream left open (`:640-648`), and a resume below the
  compacted base as an **inline `INVALID_ENVELOPE`** frame (`:676-688`). RFC-0006 §3.2 at `99f3cfa` requires
  `HISTORY_UNAVAILABLE`, gRPC `FAILED_PRECONDITION`, with `details` = UTF-8 `{"lowest_available_sequence":N}`.
  **gRPC does not do this today**; `MacpError::HistoryUnavailable` is used only by `WhatIfReplay` (`:2195`).
- HTTP needs these as request-level errors **before** the stream opens (§4.3 `:329`). Feeding
  `build_stream_session_stream` (`:695-939`) `once(subscribe).chain(pending())`, as the brief suggests, would
  send `200` and then an inline frame or a mid-stream status.
- SSE `id:` is the Envelope's ordinal. Ordinals exist only on the replay side
  (`LogStore::get_incoming_after` returns `(ordinal, entry)`, `crates/macp-storage/src/log_store.rs:106-137`) and
  are thrown away by `Runtime::get_session_envelopes_after` (`src/runtime.rs:695-724`). The live side
  (`SessionStreamBus`, `src/stream_bus.rs`) carries bare `Envelope`s. A client asking `after_sequence` beyond the
  current count gets an empty replay, so "count from `after_sequence`" mislabels the first live envelope.
- The gRPC subscribe stream never ends on a terminal session (its `inbound` keeps it alive; the bus channel is
  only removed when unused, `stream_bus.rs` `remove_if_unused`). §4.3 requires a normal end after the terminal
  Envelope.

**Other facts the plan relies on:**
- Payload limit is checked on the decoded proto bytes (`validate_envelope_shape`, `:167`), so a JSON payload
  converted to bytes before `send` lands in band naturally (§4.9).
- gRPC body cap: `max_decoding_message_size(max_payload + 64 KiB)` (`src/main.rs:487-506`).
- TLS: `MACP_TLS_CERT_PATH`/`MACP_TLS_KEY_PATH` via tonic `ServerTlsConfig` (`src/main.rs:447-460`); plaintext
  only with `MACP_ALLOW_INSECURE=1`; dev-mode auth also gated on it (`:390-410`).
- Shutdown: `ctrl_c` → `serve_with_shutdown` drain, hard deadline `MACP_SHUTDOWN_DRAIN_SECS`
  (`src/main.rs:496-616`).
- Manifest: `transport_endpoints: vec![]` (`src/server.rs:1565`).
- R2 telemetry: the gRPC SERVER span is a tower-http `TraceLayer` keyed on gRPC paths
  (`src/telemetry/grpc.rs:1-80`, `METHODS` `:71`, wired at `src/main.rs:367,446`). `process` spans use
  `Span::current()` as parent, so any handler that awaits `send` inside an HTTP SERVER span parents correctly.
- `build.rs` writes `macp_descriptor.bin` for `macp.v1` only (used by the `reflection` feature,
  `src/lib.rs:18`); the six mode protos are compiled only in `crates/macp-pb/build.rs`.
- Dependencies: `axum 0.8.8`, `hyper 1.8.1`, `hyper-util 0.1.20`, `tokio-rustls 0.26.4`, `rustls 0.23.46`,
  `tower-http 0.6.8`, `http-body-util 0.1.3` are already in `Cargo.lock` (transitively via tonic; `tower-http`
  directly, optional, under `otel`, `Cargo.toml:115`). **`prost-reflect` and `pbjson` are absent.** The root crate
  already has an optional dependency named `http` (`Cargo.toml:116`), so a Cargo feature must not be called `http`.
- **`macp-conform` lives in this repo** (`crates/macp-conform`), not in the spec repo. `client.rs:17-28` maps
  `http://`/`https://` URLs to gRPC. It is run in CI by the `conform-cli` job (`.github/workflows/ci.yml:781-876`)
  against the GHCR image and against a locally built binary. No cross-repo work is needed for the runner.

### Brief vs spec/code divergences (decided)

1. **Routes "generated from the descriptor" via build.rs:** rejected as codegen. The dispatch table must call 27
   typed trait methods, so it is a hand-written macro table **pinned by a test** against the service descriptor
   and against `telemetry::grpc::METHODS` (P5). A proto that adds an RPC already fails to compile (trait method
   missing), and then fails the test.
2. **`prost-reflect` serde with dialect options:** partly rejected. §10 has five deviations, and several have no
   serde option (original-name-only decoding, unknown enum names left at default, quoting 64-bit values above
   2^53−1, the three structural transforms, the §10.13 byte rules). Use `prost-reflect` for descriptors,
   `DynamicMessage` and wire decoding only, with an in-house JSON mapper and canonical encoder (P3).
3. **axum + tower-http:** rejected for the listener. Not for TLS: `axum::serve` accepts any `Listener`
   (`axum-0.8.8/src/serve/listener.rs:9`) and `axum-server` exists, and the rustls wiring is the same either way.
   The reasons are that routing is one dynamic segment, every status and body is custom to §4.1-§4.5 (axum extractor
   rejections and its default 405 would all be overridden), and
   tower-http's CORS answers a non-allowlisted preflight with `200` **plus** `Allow-Methods`/`Allow-Headers`
   (`tower-http-0.6.8/src/cors/mod.rs:677-681`, empty `Response::new` `:774`), which §4.10 forbids. Use
   `hyper` + `hyper-util` + `http-body-util` directly (all already in the lock).
4. **StreamSession via `build_stream_session_stream` fed `once(..).chain(pending())`:** rejected (above). P2
   extracts a shared subscription core that both bindings use.
5. **"HISTORY_UNAVAILABLE details on both bindings — check gRPC":** gRPC does not comply. P1 fixes it.
6. **"`macp-conform --transport http` (cross-repo?)":** same repo; no cross-repo plan file is written.
7. **Missing `envelope` on gRPC stays `INVALID_ENVELOPE`** (`:1249-1254`) as the brief allows; HTTP answers
   `400 INVALID_REQUEST` per §4.4.

## Design summary (decided)

- **Opt-in, root-crate only.** A non-default Cargo feature **`http-listener`** compiles `src/http/`. At runtime the
  listener starts only when `MACP_HTTP_BIND_ADDR` is set. Unset ⇒ no socket, no task, byte-identical
  `GetManifest`, no log line (zero-impact, P4 acceptance). Set without the feature ⇒ startup error, never a
  silent no-op. The published image builds with `CARGO_FEATURES=otel,http-listener` (P11). No dependency is
  added to `macp-core`, `macp-modes`, `macp-pb` or `macp-policy` (Seam; `deps-isolation` gets a new check, owned
  by P3).
- **One implementation, two bindings.** HTTP handlers build a `tonic::Request` whose `MetadataMap` holds only
  `authorization` (copied verbatim; the bearer scheme stays the exact-match `Bearer ` of `security.rs:394-406` on
  both bindings), `traceparent` and `tracestate` (no `x-macp-token`, §4.6), and call the same `MacpServer`
  (shared `SecurityLayer`, so rate limits are shared, not doubled). Two seams are added inside `server.rs` and
  used by gRPC as well, so neither is HTTP-only code:
  - `send_authenticated(identity, env) -> Ack` takes an **already authenticated** identity; every decision after
    authentication (sender, mode, both rate buckets, ingress policy, kernel) lands in the Ack. HTTP authenticates
    from headers before reading the body (§4.4); gRPC keeps its current order (shape check, then auth) and maps an
    auth failure into the Ack exactly as today.
  - `open_subscription` performs the subscribe checks and returns either an error **before** any stream exists or
    a `SubscriptionCursor` that yields `(ordinal, Envelope)`; gRPC's frame loop uses it and inlines errors as
    today (except HISTORY_UNAVAILABLE, P1).
- **Canonical JSON** (`src/http/json/`): descriptor pool from a second descriptor set covering all nine protos;
  JSON ⇄ `DynamicMessage` mapper implementing §10.8-§10.12; canonical payload encoder implementing §10.13;
  generated `prost` types at the handler boundary (`DynamicMessage::transcode_to`).
- **Codec isolation:** `src/http/json/` imports no `tonic`, `hyper` or `server` items (only `prost`,
  `prost-reflect`, `serde_json`, `base64`, `macp_pb`), enforced by a grep test, so it can be extracted later.
- **Envelope output** (SSE, `WatchSignals`): `payload` as JSON when the `(mode, message_type)` has a schema and the
  bytes round-trip **losslessly** through the canonical encoder; otherwise `payload_b64`. Deterministic per bytes.
- **Request-level errors:** registry HTTP status from `macp-error-code`, body `{"error": MACPError}`,
  `MACP-Error-Code` header, `WWW-Authenticate: Bearer` on 401. Transport statuses 404/405/406/415 carry no body.
- **SSE:** one event per message, `data:` single-line JSON, `id:` = ordinal only on subscribe envelope events,
  `: keepalive` after 30 s idle, `event: error` + close for fatal errors (`STREAM_LAGGED`, and `SESSION_NOT_OPEN`
  when the session ends by cancel/expire, which delivers no Envelope), normal close only after the terminal
  Envelope, `Cache-Control: no-store`, 406 when `Accept` excludes
  `text/event-stream`.
- **Paths:** `POST {prefix}/macp/v1/{Method}` where `{prefix}` is the path of `MACP_HTTP_PUBLIC_URL` (if any);
  `/macp/v1/{Method}` is also served, so a proxy that strips the prefix and one that does not both work.
  `GET /.well-known/macp.json` at the origin root.
- **CORS:** hand-written; allowlist `MACP_HTTP_CORS_ORIGINS` (empty default = closed); non-listed preflight =
  `204` with no `Access-Control-Allow-*`.
- **Same security posture as gRPC:** same TLS cert/key, same `MACP_ALLOW_INSECURE` gate, same payload limit (in
  band), body cap ≥ 4/3·payload + 64 KiB, same rate limits, same request timeout, same drain deadline.
- **gRPC behaviour changes (all in P1):** a subscribe below the compacted base ends the stream with
  `FAILED_PRECONDITION` + `macp-error-code: HISTORY_UNAVAILABLE` + details bytes, keeping the existing sentence
  in the status message (`macp-control-plane` already matches it on the terminal path); the policy engine's
  `PERMISSION_DENIED` gains `macp-error-code: FORBIDDEN` (additive metadata).
- **`SPEC_REV` moves to `99f3cfa`** (P0): R4 needs `examples/http/` and `macp-http-error.schema.json`, absent at
  `0f77753` (a missing input is a failure under the re-pin policy, `DECISIONS.md:963-967`).

## Phases

Each phase leaves `main` releasable with the feature off **and** on. P0-P2 touch only the gRPC surface: P1
carries every gRPC wire change, P2 none (proven by a characterization snapshot captured before it), so they can
ship first.

### P0 — Spec re-pin to `99f3cfa`, vendor the HTTP artifacts
- **Status:** DONE (2026-10-31; also `spec-drift.yml` :324/:348 and a step-5 checklist entry; upstream `invalid-http-errors/README.md` not vendored, as for conformance)
- **Risk:** simple
- **Delivers:** `SPEC_REV` = `99f3cfa090b4bfb72b3063ca0134ec661436c3ef`; the four `examples/http/*.json`, the HTTP
  error schema and its negative fixtures vendored byte-identical and drift-checked; parity contract re-vendored.
- **Depends on:** nothing.
- **Files:**
  - `.github/workflows/ci.yml`: `SPEC_REV` (`:39`, keep the line format `spec-drift.yml:62-65` parses); add
    `check_dir "tests/http/examples" "spec-repo/examples/http"` and
    `check_dir "tests/http/invalid-errors" "spec-repo/schemas/json/tests/invalid-http-errors"` after `:674`; add a
    one-file `check_file` helper next to `check_dir` for `tests/http/macp-http-error.schema.json` (its canonical
    directory `schemas/json/` holds many unrelated files, so `check_dir` would report them `DRIFT`).
  - `tests/http/examples/*.json`, `tests/http/invalid-errors/*.json`, `tests/http/macp-http-error.schema.json`,
    `tests/http/SOURCE.md` (mirrors `tests/conformance/SOURCE.md`).
  - `tests/parity/contract.json` re-vendored (adds `RECEIPT_UNAVAILABLE` to `error_codes.permanent`).
  - `tests/parity_contract.rs:322-346`: add `MacpError::ReceiptUnavailable` to `variants` (the runtime already
    produces it, `crates/macp-core/src/error.rs:110`).
  - Commit/date lines in `tests/conformance/SOURCE.md`, `tests/parity/SOURCE.md`, `tests/vectors/receipt/SOURCE.md`,
    `tests/otel/SOURCE.md`.
  - `.github/workflows/spec-drift.yml`: watch `examples/http` and the two `schemas/json` HTTP paths (`:16-17`,
    `:111`, `:129`).
  - `DECISIONS.md`: one re-pin entry.
- **Approach:** pin spec `main` (`99f3cfa`) rather than `122ad47`. `0f77753..99f3cfa` changes, among vendored
  trees, only `schemas/parity/contract.json` (verified: `git diff --stat 0f77753 99f3cfa -- schemas/conformance
  schemas/parity schemas/json/policy schemas/otel`), and that change is one the runtime already satisfies.
  **Rejected:** `122ad47` (avoids the contract change but pins behind `82c5187`/`50ae62e` that R1 already
  implements, and leaves a known drift issue open).
- **Edge cases:** `error-registry-parity` (conformance-oracle) must stay green: the registry's HTTP/gRPC columns
  are unchanged across the bump (only two description cells changed). `MACP_POLICY_SCHEMAS_DIR` content unchanged.
- **Acceptance criteria:** conformance-oracle green with the new checks; deleting a byte from any vendored HTTP file
  fails with `DRIFT`; `cargo test --test parity_contract` green at the new pin (and red if the new variant line is
  removed).
- **Tests:** existing parity/oracle tests; no new Rust test.
- **Docs:** `tests/http/SOURCE.md`; `docs/testing.md` one line.

### P1 — gRPC: compacted resume is `HISTORY_UNAVAILABLE` (RFC-0006 §3.2)
- **Status:** DONE (2026-10-31; integration test asserts base N=5 from real compaction, the N=41 byte check is a unit test; `docs/API.md` also updated)
- **Risk:** simple
- **Delivers:** a `StreamSession` subscribe whose `after_sequence` is below the compacted base ends the stream with
  gRPC `FAILED_PRECONDITION`, `macp-error-code: HISTORY_UNAVAILABLE`, `macp-error-details-bin` =
  `{"lowest_available_sequence":N}` (compact `serde_json`, byte-equal to the spec example
  `eyJsb3dlc3RfYXZhaWxhYmxlX3NlcXVlbmNlIjo0MX0=` for N=41), and a status message that **keeps** the current
  sentence `session history before ordinal {N} was compacted; …`.
- **Depends on:** P0 (the rule is in the pinned spec).
- **Also delivers:** `policy_engine::require_allow` (`src/policy_engine.rs:60,64`, reached via `server.rs:365`)
  stamps `macp-error-code: FORBIDDEN` on its `PERMISSION_DENIED` (gRPC code unchanged, metadata additive), so every
  `Status` the service returns is coded before HTTP maps them.
- **Files:** `src/server.rs` (`process_subscribe_frame` `:676-688` returns `StreamFailure::Terminal` with a status
  built by a new `history_unavailable_status(n)` helper next to `coded_status_ref` `:981`); `src/policy_engine.rs`;
  tests in `server.rs`;
  `docs/sdk-guide.md:114`, `docs/deployment.md:222`, `MIGRATING.md`, `CHANGELOG` note via commit type.
- **Approach:** a server-local helper, not a new `MacpError` field: `MacpError::HistoryUnavailable` is a unit variant
  of a published `#[non_exhaustive]` enum in `macp-core` (Seam-embedded); turning it into a struct variant breaks
  every matcher. **Rejected:** keeping the inline frame with only the code changed — §3.2 says the gRPC binding
  "reports as status FAILED_PRECONDITION".
- **Edge cases:** `macp-control-plane` matches the sentence on terminal errors
  (`macp-control-plane/src/runs/stream-consumer.service.ts:298-319`) and flags a gap, so keeping the sentence keeps
  it working; its inline-frame branch simply stops firing. A stream that already carried envelope frames and then
  sends a bad subscribe also terminates (acceptable: the frame asks for history that does not exist).
- **Acceptance criteria:** unit test: compacted session, subscribe `after_sequence` below base ⇒ stream yields
  `Err(status)` with code 9, metadata code `HISTORY_UNAVAILABLE`, details bytes exactly as above, message containing
  `N`; at or above base ⇒ replay as before. Integration test in `test_passive_subscribe.rs` over real gRPC.
- **Tests:** the two above, a regression that a **forbidden** subscriber is still an inline frame (unchanged), and
  a policy-engine denial test asserting code 7 plus `macp-error-code: FORBIDDEN`.
- **Docs:** listed in Files. Commit `fix(stream)!:` so release-plz notes the wire change.

### P1b — Shared shutdown on SIGINT or SIGTERM (`fix(shutdown)`)
- **Status:** DONE (2026-10-31; divergence: the old code did NOT exit on a second ctrl-c (tokio took over SIGINT), so the 130/143 second-signal exit is new; SIGTERM was unhandled (no drain, no final snapshot); module is binary-private `src/shutdown.rs`)
- **Risk:** simple
- **Delivers:** a `ShutdownSignal` (`tokio::sync::watch`) fed by `ctrl_c` **or SIGTERM** (`tokio::signal::unix`,
  `cfg(unix)`), replacing the two independent `ctrl_c()` awaits (`src/main.rs:501`, `:602`). The gRPC
  `serve_with_shutdown`, the drain deadline and the Prometheus listener (`:547-596`, whose accept loop now stops on the
  signal) all subscribe to it. P4 adds the HTTP listener as one more subscriber.
- **Depends on:** nothing (independent of P0-P2; same PR).
- **Files:** `src/main.rs` (or a small `src/shutdown.rs`); `tests/shutdown_binary.rs`; `docs/deployment.md` (drain
  section: SIGTERM now triggers the graceful drain); `MIGRATING.md` (gRPC-only deployments now drain on SIGTERM —
  `docker stop`/Kubernetes give in-flight requests up to `MACP_SHUTDOWN_DRAIN_SECS` instead of a hard kill; align
  `terminationGracePeriodSeconds` above it).
- **Approach:** one watch, many subscribers, so no listener can miss the signal or race another for it.
  **Rejected:** a second `tokio::signal` await per listener (each listener would need its own SIGTERM handler, and
  the deadline task would race them).
- **Edge cases:** a second signal during the drain ⇒ immediate exit (current ctrl_c behaviour at `:602` preserved);
  non-unix builds keep ctrl_c only; metrics listener disabled ⇒ nothing to stop.
- **Acceptance criteria:** spawned binary with an in-flight unary request: SIGTERM ⇒ request completes, process
  exits 0 within `MACP_SHUTDOWN_DRAIN_SECS`; same for SIGINT; with `MACP_METRICS_ADDR` set, the metrics port stops
  accepting after the signal; a request still running at the deadline ⇒ exit at the deadline.
- **Tests:** above (unix only, `#[cfg(unix)]`).
- **Docs:** listed in Files. Commit `fix(shutdown): drain on SIGTERM as well as SIGINT`.

### P2 — Server seams shared by both bindings (zero gRPC behaviour change)
- **Status:** DONE (2026-10-31; snapshot af6c924, 23 goldens byte-identical; seams are `pub(crate)`; `send_authenticated` includes the pure shape check; one-session-per-stream check stays in the gRPC frame handler; an over-high `after_sequence` mislabels a window envelope on gRPC as before, P6 filters it; additive `LogStore::get_incoming_after_with_high_water`, `SecurityLayer::with_rate_limits` is `#[doc(hidden)]` test support)
- **Risk:** complex
- **Delivers:**
  0. **First commit, before any refactor:** a characterization test (`tests/grpc_refusal_snapshot.rs`) that drives
     the refusal matrix over in-process gRPC — no/invalid token, spoofed sender, forbidden mode, message and
     SessionStart rate limits, `max_open_sessions`, payload too large, missing envelope, policy-engine denial,
     unknown session, forbidden subscriber, compacted resume, lagged stream — and stores the encoded `Ack` bytes or
     `Status` (code, message, `macp-error-*` metadata) as golden files under `tests/golden/grpc_refusals/`. It is
     captured on the post-P1 tree; P2 must not change a byte of it.
  1. `MacpServer::send_authenticated(&self, identity: &AuthIdentity, env, process) -> Ack`. It runs everything
     after `authenticate_metadata` in `authenticate_send_request` (`:296-319`) plus `runtime.process`, and returns
     every refusal as an error Ack (`make_error_ack`, `:216`). The trait `send` (`:1246`) keeps its order
     (`validate_envelope_shape`, `authenticate_metadata`, then `send_authenticated`) — gRPC output byte-identical.
  2. `MacpServer::open_subscription(&self, identity, session_id, after_sequence) -> Result<SubscriptionCursor,
     SubscribeError>`: bound-session check, `get_session_checked`, read authorization + policy engine, bus subscribe,
     snapshot. `SubscriptionCursor::next()` yields `(ordinal, Envelope)` with the existing subscribe-window dedup
     (`should_skip_replayed` `:376`) and lag → `STREAM_LAGGED`. `process_subscribe_frame` (`:608`) and the event arm
     of `build_stream_session_stream` (`:745-760`, `:812-820`) are rewritten on top of it; inline/terminal mapping
     preserved exactly.
  3. `Runtime::subscribe_snapshot(session_id, after) -> Result<(Vec<(u64, Envelope)>, u64 /*high water*/), u64
     /*base*/>`, read under **one** log read guard, backed by an additive
     `LogStore::get_incoming_after_with_high_water` in `macp-storage` (not a Seam crate). `get_session_envelopes_after`
     (`runtime.rs:695`) stays as a wrapper (public API).
- **Depends on:** P1.
- **Files:** `tests/grpc_refusal_snapshot.rs`, `tests/golden/grpc_refusals/`, `src/server.rs`, `src/runtime.rs`,
  `crates/macp-storage/src/log_store.rs`.
- **Approach:** the high-water mark makes the live ordinal exact: the first live envelope not deduped is
  `high_water + 1`, regardless of `after_sequence`. **Rejected:** (a) carrying the ordinal on the bus
  (`SessionStreamBus<(u64, Envelope)>`) — needs an O(1) ordinal at every publish site (`runtime.rs:1096,1287,1438`)
  and changes a public type; (b) counting from `after_sequence` — wrong when `after_sequence` exceeds the count;
  (c) classifying `Send` refusals by Ack code — a spoofed sender is also `UNAUTHENTICATED` (`:278-287`);
  (d) reordering gRPC `send` to authenticate first — a gRPC behaviour change with no spec requirement (gRPC is
  single-layer, everything is in band), so it stays as is.
  **Rate-limit classification (decided, UNCONFIRMED, OQ1):** both per-sender buckets and `max_open_sessions` are
  decided after decoding, so they are in band on both bindings (`RATE_LIMITED` in the Ack). `GetDecisionReceipt`/
  `WhatIfReplay` already return a `RESOURCE_EXHAUSTED` `Status` (HTTP 429).
- **Edge cases:** cursor `next()` must be cancel-safe inside `tokio::select!` (broadcast `recv` is; mutate dedup
  state only after it returns). Verify publish order equals ordinal order (publish happens after append under the
  per-session lock; check `runtime.rs:1080-1100`, `:1280-1290`, `:1430-1440`) — otherwise high-water + 1 mislabels.
  The process span and metrics record every refusal as `Rejected` (same code path as today).
- **Acceptance criteria:** the characterization snapshot (item 0) is byte-identical before and after the refactor
  commits (falsifiable: any change to an Ack or Status in the matrix fails it); the existing `server.rs` test module,
  `tests/stream_integration.rs` and tier-1 integration suite pass unchanged; new unit tests: (i) `send_authenticated`
  returns an in-band Ack for spoofed sender, forbidden mode, message and SessionStart rate limits, payload too large; (ii) cursor ordinals equal log ordinals for replay-then-live, for `after_sequence` > count,
  for a compacted base, and across the subscribe window (an envelope accepted between bus subscribe and snapshot is
  delivered once with the right id).
- **Tests:** above, plus a `LogStore` unit test for the high-water method with interleaved internal/checkpoint
  entries (mirrors `log_store.rs:192-209`); the snapshot test itself.
- **Docs:** none (internal); doc comments on the seams.

### P3 — Canonical JSON codec (RFC-0001 §10)
- **Status:** DONE (2026-10-31; prost-reflect 0.16.5; divergences: deps-isolation checks *direct* deps since hyper/tokio-rustls arrive via tonic; timestamps emit ms only when non-zero (matches vendored examples, §10.11 allows); `decode_request` uses a raw-text JSON scanner so integer fields are judged on the number text (§10.9, `arbitrary_precision` rejected: feature unification); `chrono` added to the codec allowlist; P5 MUST route bodies through `decode_request`/`decode_message_text`, never the `Value` entry points)
- **Risk:** complex
- **Delivers:** `src/http/json/` behind `http-listener`: `decode_request(method, &[u8]) -> DynamicMessage`,
  `encode_response(&DynamicMessage) -> Vec<u8>`, `envelope_from_json`, `envelope_to_json`, and
  `canonical_payload_bytes(desc, &serde_json::Value) -> Vec<u8>`.
- **Depends on:** P0 (nothing behind the feature needs P1/P2, so P3 may run in parallel with P1-P2).
- **Files:**
  - **Lockfile ownership:** P3 regenerates the root `Cargo.lock` for `prost-reflect` and the newly direct optional
    deps, and regenerates `integration_tests/Cargo.lock` too (`cargo metadata --manifest-path
    integration_tests/Cargo.toml --format-version 1 > /dev/null`), because that lock records the root crate's
    dependency edges (CLAUDE.md, "Two lockfiles"); both `--locked` guards must pass in this phase's commit.
  - `.github/workflows/ci.yml` `deps-isolation` (`:261-371`), **owned by P3:** one step asserting `prost-reflect`,
    `hyper-util` and `tokio-rustls` appear in none of `cargo tree -p macp-core|macp-modes|macp-pb|macp-policy`
    (all features) and not in `cargo tree -p macp-runtime -e normal` with default features.
  - `Cargo.toml`: feature `http-listener = ["dep:prost-reflect", "dep:hyper", "dep:hyper-util",
    "dep:http-body-util", "dep:tokio-rustls", "dep:http", "dep:bytes"]` (hyper/rustls deps are used from P4; declare
    them here once). `prost-reflect` with `default-features = false` (descriptors + `DynamicMessage` only); confirm
    the release that depends on `prost 0.14` (expected 0.16.x) and that `cargo tree -d` shows a single `prost`.
  - `build.rs`: under `#[cfg(feature = "http-listener")]` (`CARGO_FEATURE_HTTP_LISTENER`), a **descriptor-only**
    `protoc` run — `protoc --include_imports --descriptor_set_out=$OUT_DIR/macp_json_descriptor.bin -I <macp-proto
    include dir> <the nine protos listed in crates/macp-pb/build.rs>`, with `protoc` resolved the way prost-build
    does (`PROTOC`, else PATH). No Rust is generated, so the existing codegen and `macp_descriptor.bin`
    (reflection) are untouched and nothing can overwrite the service stubs. A test asserts the pool contains
    `macp.v1.MACPRuntimeService` and every `macp.modes.*.v1` payload message.
  - `src/http/json/{mod,pool,decode,encode,canonical,payload}.rs`; `src/lib.rs` (`#[cfg(feature =
    "http-listener")] pub mod http;`).
  - `tests/http_json.rs` (feature-gated).
- **Approach:**
  - **Decode** (JSON → `DynamicMessage`): walk the descriptor. Keys match the original field name only (a
    lowerCamelCase spelling is an unknown member, ignored, §10.8(1)); unknown members ignored (§10.6); `null` =
    absent (§10.10). Numbers follow proto3 JSON wherever §10.8 lists no deviation (`:545`): every integer type
    (32- and 64-bit) accepts a JSON number, a quoted decimal string, and exponent notation (`1e3`, `"1.5e2"`) when
    the value is integral and in range; non-integral or out-of-range values are rejected, never rounded (§10.9);
    `float`/`double` accept numbers, quoted numbers and `"NaN"`/`"Infinity"`/`"-Infinity"`. The **encoder** stays
    canonical (plain integer numbers, quoting only above 2^53−1); enums accept names or integers, an **unknown name** leaves the field default / drops the
    repeated element (§10.4); `bytes` accept standard and URL-safe, padded or not (§10.3); floats accept
    the special strings. Structural transforms (§10.11): `macp.v1.Envelope.timestamp` (RFC 3339, any
    offset, truncate below ms) → `timestamp_unix_ms`; `payload`/`payload_b64` (§10.12, below);
    `macp.v1.PolicyDescriptor.rules` object **or** string → compact JSON text.
  - **Payload selection** (§10.12): Core types (`Signal`, `Progress`, `SessionStart`, `SessionCancel`,
    `SessionSuspend`, `SessionResume`) → `macp.v1.{T}Payload`; `Commitment` → `macp.v1.CommitmentPayload`; else map
    the Envelope `mode` to its package (`macp.mode.<m>.v1` → `macp.modes.<m>.v1`, `ext.multi_round.v1` →
    `macp.modes.multi_round.v1`) and look up `{package}.{message_type}Payload`. No schema ⇒ the Envelope is
    rejected in band with `INVALID_ENVELOPE`. §10.2 (`RFC-0001:511`) says a JSON Envelope "MUST include exactly one
    of" the two, so **both or neither** ⇒ `INVALID_ENVELOPE` (an empty payload is sent as `"payload_b64": ""` or
    `"payload": {}`); `null` counts as absent. The table is pinned by a test against `crates/macp-conform/src/encode.rs` (which already knows
    every mode's message names, `:68`, `:100`).
  - **Canonical payload encoder** (§10.13): fields ascending by number; omit proto3 defaults except explicit-presence
    fields that are set (`-0.0` is emitted); packed repeated scalars; map entries sorted by key UTF-8 bytes with key
    **and** value always written; NaN canonicalised; unknown members never reach bytes. Written over
    `DynamicMessage` because `prost` skips default map keys/values and `DynamicMessage` maps are unordered.
  - **Encode** (`DynamicMessage` → JSON): original names; every no-presence field emitted with its default
    (`""`, `0`, `false`, zero enum name, `[]`, `{}`); presence fields only when set; int64 as numbers, quoted when
    |v| > 2^53−1; enum names (unknown numeric value → the integer); bytes standard base64 padded; transforms reversed
    (Envelope `timestamp` RFC 3339 UTC `Z` with ms, `rules` inlined as an object; an unparseable `rules` string is
    emitted as the string form). Envelope payload: JSON object iff a schema exists **and**
    `canonical(decode(bytes)) == bytes`, else `payload_b64`.
  - **Isolation:** the module depends only on `prost`, `prost-reflect`, `serde_json`, `base64`, `macp_pb`
    descriptors and `std`; a test greps `src/http/json/` for `tonic`, `hyper`, `crate::server` and fails on a hit.
  - **Rejected:** `prost-reflect`'s `serde` feature with pre/post passes (two layers, library edge cases still need
    pinning, and the pre-pass is as large as the mapper); `pbjson` (camelCase, string int64, no payload selection);
    a descriptor walker over `prost-types` without `prost-reflect` (re-implements generic wire decoding).
- **Edge cases & failure modes:** recursive messages (none today; cap depth at 64 anyway → 400); very large arrays
  (bounded by the body cap); `1e3` for an int field = 1000, `1.5` = rejected; duplicate JSON keys (serde_json
  keeps the last; document); `SessionStartPayload.extensions` map<string,bytes> (base64 values, UTF-8 ordering);
  strings that are not valid UTF-8 cannot occur in JSON; a decoded payload with unknown fields on the output side
  fails the round-trip check ⇒ `payload_b64` (lossless by construction).
- **Acceptance criteria:**
  - Every `envelope`/`payload` in the vendored `tests/http/examples/*.json` decodes, converts and re-encodes to JSON
    that is equal modulo default fields the example omits.
  - For each mode payload type, `canonical_payload_bytes(json)` equals `prost::Message::encode_to_vec` of the same
    value built with the generated type, for values without maps/NaN; and differs from prost exactly where §10.13
    requires (map with default key/value, map key order `"\u{FFFF}"` vs `"\u{10000}"`, NaN payload, `-0.0`).
  - `sessionId` instead of `session_id` ⇒ field empty; unknown enum name ⇒ default; `"900000"` = `900000`;
    `1e3`, `"1e3"` and `1000.0` all decode to 1000 for `int32` and `int64`; `"2.5"` and `2.5e0` decode for a `double`;
    `1.5` for an int ⇒ error; `2^53` encodes quoted; encoder never emits exponents for integers; `rules` string and
    object decode to the same descriptor.
  - Decode → encode → decode is a fixpoint for every request/response type in the service, over a **hand-rolled
    corpus** (`tests/http_json/corpus/*.json`: the vendored examples, the conformance fixtures' envelopes, and one
    maximal and one all-defaults value per message type). No `proptest`/`quickcheck` dependency is added.
- **Tests:** `tests/http_json.rs` as above, plus unit tests per rule in the modules; failure paths: wrong JSON type
  per scalar kind, non-object payload, both payload forms, neither payload form, and a JSON `payload` for a
  **dynamically registered** ext mode (not `ext.multi_round.v1`, which the table maps) ⇒ `INVALID_ENVELOPE`.
- **Docs:** module docs citing §10 subsections.

### P4 — Listener skeleton: config, TLS, gating, shutdown, transport statuses
- **Status:** DONE (2026-10-31; divergences: hyper-util `server-auto` dropped — TLS routes by ALPN (`h2` ⇒ HTTP/2, else HTTP/1.1) and plaintext sniffs the h2 preface under a 10 s deadline, because auto's sniff had no deadline and let a silent socket pin a connection permit; listener binds inline before gRPC serves (bind failure aborts startup) rather than spawning after `:510`; feature-off refusal is tested in the default-feature run; PEM loading via `rustls-pki-types` (tonic `Identity` yields no rustls config); the 501 placeholder reads the capped body, so **P5 must authenticate from headers before reading the body** (§4.4). Accepted risk: an h2 client that answers PINGs but never sends a request holds a permit (hyper has no h2 idle timeout; equivalent to an h1 client re-requesting every 9 s); the sniff/TLS phases are not shutdown-cancellable but are bounded at 10 s each, under the drain deadline)
- **Risk:** complex
- **Delivers:** `src/http/server.rs` serving HTTP/1.1 and HTTP/2 (ALPN on TLS) with every route answering a
  placeholder, the transport statuses of §4.1/§4.2/§4.5 (404 unknown method or path, 405 + `Allow: POST`, 415
  media type/charset), and the zero-impact guarantee.
- **Depends on:** P1b (shutdown signal), P3 (feature and deps).
- **Files:** `src/http/{mod,config,server,tls}.rs`; `src/main.rs` (`validate_env_config` `:19`, spawn after
  `:510`, subscribing the HTTP listener to P1b's `ShutdownSignal`); `tests/http_binary.rs`.
- **Approach:**
  - **Config** (`HttpConfig::from_env`, validated at startup, fatal on error): `MACP_HTTP_BIND_ADDR` (socket addr;
    must differ from `MACP_BIND_ADDR`); `MACP_HTTP_PUBLIC_URL` (optional; absolute, no trailing slash, no
    query/fragment, `https` unless `MACP_ALLOW_INSECURE=1`); `MACP_HTTP_CORS_ORIGINS` (comma list of
    `scheme://host[:port]`, `*` refused); `MACP_HTTP_MAX_BODY_BYTES` (default `ceil(4/3·MACP_MAX_PAYLOAD_BYTES) +
    65536`; a configured value below that floor is a startup error, §4.9); `MACP_HTTP_MAX_CONNECTIONS` (default
    1024). Reused: `MACP_TLS_CERT_PATH`/`KEY_PATH`, `MACP_ALLOW_INSECURE`, `MACP_REQUEST_TIMEOUT_SECS`,
    `MACP_MAX_CONCURRENT_STREAMS` (h2), `MACP_SHUTDOWN_DRAIN_SECS`.
  - **TLS:** `tokio-rustls` with the ring provider explicitly (`ServerConfig::builder_with_provider`), cert/key via
    `rustls-pki-types` PEM parsing, ALPN `h2`,`http/1.1`; 10 s handshake timeout. No TLS and no
    `MACP_ALLOW_INSECURE=1` ⇒ startup error (same rule as gRPC, `src/main.rs:447-460`).
  - **Serving:** accept loop guarded by a semaphore (`MACP_HTTP_MAX_CONNECTIONS`), `hyper_util::server::conn::auto`
    with `http1().header_read_timeout(10 s)`; HTTP/2 bounds `max_concurrent_streams(MACP_MAX_CONCURRENT_STREAMS)`,
    `max_header_list_size(16 KiB)`, `max_pending_accept_reset_streams(…)` (rapid-reset), `keep_alive_interval(30 s)`
    + `keep_alive_timeout(20 s)`, and `max_send_buf_size`; and `hyper_util::server::graceful::GracefulShutdown`; the existing drain deadline force-exits SSE streams that never
    end.
  - **Routing:** strip `{prefix}` or nothing, then `/macp/v1/{Method}` lookup; `OPTIONS` goes to CORS (P7);
    `GET`/`HEAD` `/.well-known/macp.json` goes to discovery (P8).
  - **Rejected:** multiplexing HTTP onto the gRPC port (couples the opt-in surface to the always-on one, breaks
    zero-impact); `axum`/`axum-server` (see divergence 3: one dynamic segment, all statuses custom).
- **Edge cases:** bind failure ⇒ fatal (unlike metrics, which logs); IPv6 addresses; a `Content-Type` with
  parameters (`application/json; charset=UTF-8` accepted, `charset=latin1` ⇒ 415); missing `Content-Type` ⇒ 415;
  path with trailing slash or percent-encoding ⇒ 404; a prefix that is itself `/macp/v1`.
- **Acceptance criteria:**
  - Binary without `MACP_HTTP_BIND_ADDR`: no extra listening socket, `GetManifest` byte-identical to 0.11, no new log
    line (asserted by `tests/http_binary.rs`, precedent `tests/otel_binary.rs`).
  - Built without the feature and `MACP_HTTP_BIND_ADDR` set ⇒ exits non-zero with a clear message.
  - With TLS: HTTP/1.1 and HTTP/2 both answer; plaintext without `MACP_ALLOW_INSECURE` ⇒ refuses to start.
  - `GET /macp/v1/Send` ⇒ 405 `Allow: POST`; `POST /macp/v1/Nope` ⇒ 404, empty body; wrong media type ⇒ 415.
  - SIGINT and SIGTERM each drain the HTTP listener (including an open SSE stream, closed at the deadline) together
    with gRPC within `MACP_SHUTDOWN_DRAIN_SECS` (P1b's binary test extended with the listener on).
  - An HTTP/2 client opening more than `MACP_MAX_CONCURRENT_STREAMS` streams is refused beyond the bound.
- **Tests:** `tests/http_binary.rs` (spawned binary), in-process tests driving the service with
  `hyper::client::conn` over a duplex; failure paths: bad env values each produce their error.
- **Docs:** deferred to P11 (env table drafted here in code comments).

### P5 — Unary RPCs through `MacpServer`; error mapping; `Send` two layers
- **Status:** DONE (2026-10-31; divergences: request timeout is `500 INTERNAL_ERROR` with a §4.5 body (P4's 503 replaced); streaming RPCs answer 501 with no body until P6; any failure inside the `envelope` member is an in-band `INVALID_ENVELOPE` Ack per §4.4's exhaustive "not a SendRequest" list, `{"envelope":null}` stays 400 per §10.10; the `table == METHODS` pin runs in CI under `otel,http-listener`; binary tests bind HTTP/metrics on `:0` and read the bound address (TOCTOU port race), gRPC keeps a probed port with an AddrInUse respawn backstop; the metrics startup log now prints the bound address)
- **Risk:** complex
- **Delivers:** every non-streaming RPC at `POST …/macp/v1/{Method}` with JSON in/out, request-level errors per
  §4.5, `Send` per §4.4.
- **Depends on:** P2, P3, P4.
- **Files:** `src/http/{dispatch,errors,metadata}.rs`; `tests/http_unary.rs`.
- **Approach:**
  - `dispatch!` table: `(Method, ReqType, RespType, kind)` for all 27 RPCs; unary rows call the trait method with
    `tonic::Request::from_parts(metadata, Extensions::new(), req)`. A unit test asserts the table's method set ==
    the descriptor's `macp.v1.MACPRuntimeService` methods == `telemetry::grpc::METHODS` (`grpc.rs:71`), and that each
    row's streaming flags match the descriptor.
  - **Metadata:** copy only `authorization` verbatim (exact-match `Bearer ` as on gRPC, `security.rs:394-406`; no
    normalisation, so both bindings accept the same headers), `traceparent`, `tracestate`. `x-macp-token` is not
    honoured on HTTP (§4.6). No cookie is read and no `Set-Cookie` is ever written.
  - **Body:** collected with `http_body_util::Limited` at the body cap ⇒ `413 PAYLOAD_TOO_LARGE` request-level
    before decoding, inside the same `MACP_REQUEST_TIMEOUT_SECS` deadline as the handler (a slow-loris body times
    out; the deadline starts when headers are complete); not JSON / not an object / empty body ⇒ `400
    INVALID_REQUEST`. Response bodies are always `Content-Type: application/json` (never
    `application/macp-envelope+json`, §4.2).
  - **Send precedence (decided; rate limits UNCONFIRMED, OQ1):** (1) `authenticate_metadata` from headers, **before the body is
    read** ⇒ `401` + `WWW-Authenticate: Bearer`; (2) body cap ⇒ `413`; (3) not a `SendRequest` ⇒ `400`; (4)
    Envelope decode/payload conversion failure ⇒ `200` Ack `INVALID_ENVELOPE` (message_id/session_id copied when
    they were strings); (5) `send_authenticated(identity, env)` ⇒ `200` Ack: sender mismatch, forbidden mode, the
    message/SessionStart buckets (`RATE_LIMITED`), `max_open_sessions`, ingress policy, kernel — in gRPC's order
    (`:299-319`). Request-level `429` on `Send`: **not emitted unless a pre-decode per-credential limit is added**
    (there is none; the existing buckets need `message_type`, so §4.4 classes them as Envelope decisions). The
    precedence is therefore 401 > 413 > 400 > in-band Ack, and a client sees the same rate-limit code in the same
    place on both bindings.
  - **Errors:** `Status` → code from `macp-error-code` → `registry_entry(code).http`; details from
    `macp-error-details-bin` (base64 in JSON); `session_id` filled from the request's `session_id` /
    `subscribe_session_id` when present; `message_id` `""`. `Unimplemented` ⇒ 404 without body (§4.1). An uncoded
    status (should not exist after P2) falls back by gRPC code, else `500 INTERNAL_ERROR`.
  - **Timeout:** `MACP_REQUEST_TIMEOUT_SECS` around the unary future (not around SSE bodies, matching tonic,
    `grpc.rs:34-43`); expiry ⇒ `500 INTERNAL_ERROR` "request timed out" (a `Send` may have been applied; §4.8 retry
    with the same `message_id` is safe).
- **Edge cases:** `Initialize` stateless (§4.7: no per-connection state, nothing required before other RPCs; the
  handler holds no per-connection map, so there is nothing to keep);
  `GetDecisionReceipt.receipt_json` stays a string (not a §10.11 transform); `ListSessions` bad `page_token` ⇒ 400;
  control-plane 409 on retry (§4.8) passes through unchanged; JSON numbers above i64 for int fields ⇒ 400.
- **Acceptance criteria:** each exchange in `tests/http/examples/send.json` and `errors.json` replayed against the
  in-process listener (tokens mapped to configured identities) reproduces status, the required headers
  (`Content-Type`, `MACP-Error-Code`, `WWW-Authenticate`) and body shape, ignoring `message`, timestamps and
  `accepted_at_unix_ms`; every error body passes a hand-written check of `macp-http-error.schema.json` and every
  vendored `invalid-errors/*.json` fails it (no `jsonschema` dependency; a test ties the checked property set to the
  schema file); for every RPC, the HTTP response decodes to the same prost message gRPC returns for the same
  request (golden parity test).
- **Tests:** above; failure paths: 401 (missing, wrong scheme, lowercase `bearer` refused on both bindings, bad
  token, dev-mode refusal); **no token + malformed Envelope ⇒ 401** (and no token + oversize payload ⇒ 401, not an
  Ack); **no token + body over the cap ⇒ 401** (body never read); message bucket exhausted ⇒ **200 Ack `RATE_LIMITED`** (never 429; the same request over gRPC gives the same
  Ack); SessionStart bucket exhausted ⇒ 200 Ack `RATE_LIMITED`; **forbidden mode + exhausted bucket ⇒ 200 Ack
  `FORBIDDEN`, and the bucket is not charged**; rate-limited + malformed Envelope ⇒ 200 Ack `INVALID_ENVELOPE`; a
  rate-limited `WhatIfReplay` ⇒ 429 with `MACP-Error-Code: RATE_LIMITED`; no `Send` response in the suite is a 429; in-band spoofed sender/SessionStart limit/payload-too-large (converted bytes over the limit with
  a body under the cap); 413 body cap; 400 malformed; `x-macp-token` ignored; **slow-loris** body (headers sent, body
  trickled past the timeout) ⇒ the request ends within `MACP_REQUEST_TIMEOUT_SECS` + 1 s; §4.6/§4.7/§4.2 MUSTs:
  a fresh connection's first request is `GetSession`/`Send` and succeeds with no `Initialize`; a request carrying a
  `Cookie` header behaves identically to one without, and no response in the whole suite has `Set-Cookie` (asserted
  in the shared test client); no response in the suite is labelled `application/macp-envelope+json`.
- **Docs:** P11.

### P6 — SSE streams with resume
- **Status:** DONE (2026-10-31; divergences: the runtime sends lifecycle `Resolved` before publishing the Commitment (`runtime.rs:1463` vs `:1476`), so the stream records the last ordinal at `Resolved` and closes once it has delivered it — equivalent to "close after the Commitment" and order-safe; resume opens at min(after_sequence, high water) then drops `<= after_sequence`, retrying at after_sequence below the compacted base; `WatchPolicies`/`WatchModeRegistry`/`WatchRoots` mirror gRPC's per-RPC auth (no token) — `WatchPolicies` bypassing `ListPolicies`' auth is pre-existing, filed as #274; streams end when drain starts, without `event: error`, so clients resume)
- **Risk:** complex
- **Delivers:** `WatchModeRegistry`, `WatchRoots`, `WatchSignals`, `WatchSessions`, `WatchPolicies` and
  `StreamSession` (subscribe only) as `text/event-stream` per §4.3.
- **Depends on:** P2, P5.
- **Files:** `src/http/sse.rs`; `tests/http_sse.rs`.
- **Approach:**
  - **Pre-stream checks** (any failure ⇒ request-level error, no stream): `Accept` present and not admitting
    `text/event-stream` ⇒ 406; auth; for `StreamSession`: body must have non-empty `subscribe_session_id` and no
    `envelope` ⇒ else 400; `Last-Event-ID` must be a non-negative decimal and agree with a non-zero `after_sequence`
    ⇒ else 400; then `open_subscription` ⇒ 404 `SESSION_NOT_FOUND`, 403 `FORBIDDEN`, 409 `HISTORY_UNAVAILABLE` with
    details. Watch RPCs: the trait call's `Err(Status)` is request-level.
  - **Framing:** `id: {ordinal}\n` (subscribe envelopes only) + `data: {json}\n\n`; `: keepalive\n\n` after 30 s
    without an event (`tokio::time::timeout` on the next item); a mid-stream `Err(Status)` ⇒ `event: error\ndata:
    {"error":…}\n\n` then close. Headers `Content-Type: text/event-stream`, `Cache-Control: no-store`. No `retry:`.
  - **Normal end (subscribe):** after yielding an Envelope, if the session is now terminal (it was that Envelope
    that resolved it — a `Commitment`), close normally. §4.3 (`:327`): a client MUST NOT reconnect after a normal end
    following the terminal Envelope, and "any other close without `event: error` is a lost connection".
  - **Non-Envelope terminals (decided, option (a), UNCONFIRMED):** cancel and TTL expiry write runtime bookkeeping
    entries that are neither counted nor delivered (`runtime.rs:1200-1206`, RFC-0006 §3.2), so there is no terminal
    Envelope to end on. A literal normal close would read as a lost connection and drive reconnect loops (each
    resume replays and closes again). §4.3 lets a runtime end any stream fatally with `event: error` and a
    `MACPError`, so on a `Cancelled`/`Expired` lifecycle event for this session (`subscribe_session_lifecycle`,
    `runtime.rs:684`) the stream sends `event: error` with `SESSION_NOT_OPEN` (the registry code for both,
    `crates/macp-core/src/error.rs:89-90`, `:189`) and a message naming the cause, then closes. A subscribe to a
    session already cancelled/expired replays its history and ends the same way; one already resolved replays
    through the Commitment and closes normally. **Rejected:** option (b), the literal close plus a doc note — every
    generic SSE client would loop.
  - **Bus/lifecycle race:** the two channels are independent, so a lifecycle event can arrive before the Envelope
    that caused it. A `Resolved` lifecycle event **never** closes the stream: only the Commitment Envelope does. On
    `Cancelled`/`Expired`, the stream first drains the session bus with `try_recv` until empty, yielding any
    envelopes still buffered (they precede the terminal entry, which is appended under the session lock after them),
    then sends the error event.
  - **`WatchSignals`:** each event is the canonical `WatchSignalsResponse` JSON; `sender_authenticated` is always
    emitted (defaults emitted, §10.10), never inside the Envelope (§4.4).
  - **Rejected:** `EventSource`-style GET (spec: POST only); long-polling (spec forbids).
- **Edge cases:** the cursor labels the first live envelope `high_water + 1` (P2); when the client's `after_sequence` exceeds the high water, drop every item with `ordinal <= after_sequence` so a subscribe-window envelope is never mislabelled or duplicated (P2 ASSUMPTIONS entry); slow consumer ⇒ hyper stops polling ⇒ broadcast lag ⇒ `STREAM_LAGGED` (503 code in the error
  event), as gRPC; client disconnect drops the stream and the bus receiver (`remove_if_unused` reclaims); keepalive
  writes surface dead peers; the SERVER span stays open for the stream's life (as gRPC); shutdown closes streams at
  the drain deadline.
- **Acceptance criteria:** `tests/http/examples/stream_session_subscribe.json` and `watch_signals.json` reproduce
  (ids, keepalive comment under a test-shortened interval, `event: error` for an induced lag, 409 and 400 cases);
  resume test: read N events, drop the connection, resubscribe with `Last-Event-ID: k` for every k in 0..=N while
  new envelopes arrive ⇒ the union of deliveries is gapless and each ordinal maps to the same `message_id` as
  `GetSession`/log order; a terminal Commitment ends the response without `event: error`; 406 when `Accept:
  application/json`, while `Accept: */*`, `Accept: text/*`, `text/event-stream;q=0.5, application/json` and a
  missing `Accept` all open the stream.
- **Tests:** above plus: **race test** — a test hook delays the bus delivery while the lifecycle `Resolved` event is
  delivered first; the stream still yields the Commitment with its `id:` and then closes normally (repeated 200
  times); cancel ⇒ `event: error` `SESSION_NOT_OPEN` after every buffered envelope; TTL expiry likewise; already
  cancelled session ⇒ replay then error event; failure paths: non-integer `Last-Event-ID`, conflicting `after_sequence`, envelope in body,
  empty `subscribe_session_id`, forbidden subscriber, unknown session.
- **Docs:** P11.

### P7 — CORS and preflight
- **Status:** DONE (2026-10-31; divergences: with an allowlist configured `Vary: Origin` goes on every actual response, not only listed ones (Fetch CORS/HTTP-cache guidance; verifier R1); config origins normalise a trailing slash and the default port; every `OPTIONS` is answered before routing and auth)
- **Risk:** simple
- **Delivers:** §4.10 exactly.
- **Depends on:** P4 (may land any time after).
- **Files:** `src/http/cors.rs`; tests in it.
- **Approach:** hand-written middleware. `OPTIONS` from a listed origin ⇒ `204`, `Access-Control-Allow-Origin:
  <origin>`, `Access-Control-Allow-Methods: POST`, `Access-Control-Allow-Headers: Authorization, Content-Type,
  Last-Event-ID, traceparent, tracestate`, `Access-Control-Max-Age: 600`, `Vary: Origin`. Any other `OPTIONS` ⇒
  `204` with no `Access-Control-Allow-*`. Actual responses to a listed origin add `Allow-Origin`, `Vary: Origin`,
  `Access-Control-Expose-Headers: MACP-Error-Code`. Never `Allow-Credentials`. Origins compared exactly
  (scheme+host+port, case-insensitive host). **Rejected:** `tower-http` `CorsLayer` (200 and leaks `Allow-*` on a
  denied preflight, cited above).
- **Edge cases:** `Origin: null`; origin with trailing slash in config (normalised at startup); CORS headers also on
  error and SSE responses; empty allowlist = every preflight denied.
- **Acceptance criteria:** listed preflight carries exactly the headers above; unlisted ⇒ 204 and no
  `Access-Control-Allow-*` header at all; no response ever carries `Access-Control-Allow-Credentials`.
- **Tests:** table-driven unit tests; one in-process request test.
- **Docs:** P11.

### P8 — Discovery: `transport_endpoints` and `/.well-known/macp.json`
- **Status:** DONE (2026-10-31; divergences: other methods on the well-known path ⇒ 405 `Allow: GET, HEAD` (spec silent); the path gets normal CORS handling; the entry carries `"metadata": {}` per §10.10)
- **Risk:** simple
- **Delivers:** when `MACP_HTTP_PUBLIC_URL` is set, `GetManifest` (both bindings) adds
  `{transport: "macp.transport.http.v1", uri: <base>, content_types: ["application/json"]}`; `GET`/`HEAD
  /.well-known/macp.json` serves the `AgentManifest` canonical JSON with `Content-Type: application/macp-manifest+json`.
- **Depends on:** P5.
- **Files:** `src/server.rs` (`MacpServer::with_http_endpoint`, `get_manifest` `:1543-1568`), `src/http/discovery.rs`,
  `src/main.rs`.
- **Approach:** the well-known document is unauthenticated, like `GetManifest` today (`:1543`, no auth call). No
  public URL ⇒ no entry (the runtime cannot know its public origin behind a proxy; §6 makes the field optional), and
  the well-known document is still served. No gRPC endpoint entry (no public gRPC URL configured; out of scope).
- **Edge cases:** the manifest must stay byte-identical when the listener is off (P4 test); `uri` is `{base}` without
  `/macp/v1/`; a prefix path is preserved in `uri` while the well-known path stays at origin root.
- **Acceptance criteria:** with the URL set, gRPC and HTTP `GetManifest` and the well-known document all carry the
  same entry; without it, `transport_endpoints` is `[]` on both.
- **Tests:** unit + in-process HTTP test.
- **Docs:** P11.

### P9 — Observability for HTTP
- **Status:** DONE (2026-10-31; divergences: `http.route` excludes the public prefix; non-standard methods record `_OTHER` and span name `HTTP`; `error.type` = status code on a 5xx and `url.scheme` on the metric (semconv, bounded); `url.path` deliberately omitted (unbounded client input, S-c); the debug event is emitted in every build but never reaches the OTel layer; hyper's own 400/431 get no span)
- **Risk:** simple
- **Delivers:** under `otel`: one SERVER span per HTTP request named `POST /macp/v1/{Method}` (or `POST` for an
  unknown route, so cardinality is bounded), parent extracted from `traceparent`/`tracestate` through the global
  propagator (`HeaderExtractor`, `grpc.rs:148`), attributes `http.request.method`, `http.route`, `url.scheme`,
  `http.response.status_code`, `network.protocol.version`; ERROR only for 5xx; `process` spans become its
  children (they use `Span::current()`); SSE spans last until the stream ends. Instrument
  `http.server.request.duration` (OTel HTTP semconv, seconds) with `http.request.method`, `http.route`,
  `http.response.status_code`. Without `otel`: `tracing` debug event per request (method name, status), never
  headers or bodies.
- **Depends on:** P5, P6.
- **Files:** `src/telemetry/http.rs`, `src/telemetry/mod.rs`, `src/http/server.rs`; `tests/otel_http.rs`.
- **Approach:** a small hand-written wrapper rather than `TraceLayer` (no tower in the HTTP path). S-b/S-c: span is
  `info` in the binary crate only and records ids/route only, never bodies, tokens or payloads. **Deferred:** a
  Prometheus family for HTTP (names are a one-way door; trigger: an operator asks).
- **Edge cases:** malformed `traceparent` ⇒ new root, never an error; preflight/transport-404 requests get a span with
  no route.
- **Acceptance criteria:** in-memory exporter test: a client `traceparent` parents the HTTP SERVER span, `process` is
  its child, attributes match; no span attribute contains a header value other than the method/route; replay
  equivalence unaffected.
- **Tests:** `tests/otel_http.rs` (features `otel,http-listener`).
- **Docs:** P11.

### P10 — Conformance over HTTP (`macp-conform --transport http`)
- **Status:** DONE (2026-10-31; divergences: client is hyper + tokio-rustls (ring), not reqwest — reqwest feature unification with macp-auth broke its JWKS client in workspace test builds; deps-isolation's existing macp-conform check widened to `hyper|tokio-rustls`; HTTP/1.1 only; the in-process test also runs GrpcTarget and asserts identical per-fixture outcomes; a coded non-200 `Send` is a rejection only for 400/401/413 (§4.4); macp-cli enables the `http` feature so `macp conform --transport http` works; the gRPC readiness loop in conform-cli now fails when the port never opens)
- **Risk:** complex
- **Delivers:** an `HttpTarget` in `macp-conform` (new non-default feature `http`), CLI flag `--transport
  grpc|http` (default `grpc`; URL schemes keep their current meaning, `client.rs:17-28`), a CI step running the full
  fixture pack over HTTP against the same locally built runtime, and an in-repo test doing the same in process.
- **Depends on:** P5, P6 (history is read through the subscribe stream).
- **Files:** `crates/macp-conform/{Cargo.toml,src/http.rs,src/cli.rs,src/lib.rs,README.md}`; `Cargo.toml`
  (dev-dependency `macp-conform` with `features = ["http"]`); `tests/conformance_http.rs`;
  `.github/workflows/ci.yml` (`conform-cli`: build runtime with `--features http-listener`, start with
  `MACP_HTTP_BIND_ADDR=127.0.0.1:8080`, run `--transport http --target http://127.0.0.1:8080`, assert the same
  claim string as the gRPC step `:850-856`; the `clippy` job adds `-p macp-conform --features http`). **Lockfile
  ownership:** P10 adds `reqwest` features to `macp-conform` and regenerates **both** `Cargo.lock` and
  `integration_tests/Cargo.lock` (`cargo metadata --manifest-path integration_tests/Cargo.toml --format-version 1 >
  /dev/null`) in the same commit. No new `deps-isolation` step: the existing one (`:317-323`) already proves the
  default `macp-conform` stays free of `reqwest`.
- **Approach:** `HttpTarget` uses `reqwest` (workspace dep, `default-features = false`; add the rustls TLS and
  `stream` features in this crate only) and an **independent minimal encoder**: Envelopes are always sent with
  `payload_b64`, which §10.12 always permits, so the runner does not test the runtime's codec against itself;
  responses are parsed with `serde_json` into the few fields the runner needs (Ack, `SessionMetadata`, SSE
  envelopes decoded from `payload_b64` or re-encoded from `payload` via the generated types). `history()` reads the
  subscribe SSE until it closes (terminal) or goes idle (`with_stream_idle`, as `grpc.rs:76`). Default feature set
  stays transport-free; the existing `deps-isolation` check already forbids `reqwest` in default `macp-conform`
  (`ci.yml:317-323`). **Rejected:** treating `http://` URLs as HTTP (silently changes existing gRPC invocations);
  reusing the runtime's codec in the runner (root crate; and self-validation).
- **Edge cases:** the clocked fixture stays skipped (`needs-clock-control`) exactly as over gRPC; cmt-hash vectors
  via `--hash-vectors in-process`; the published image does not have the feature yet, so the HTTP step runs only
  against the local build until an image with it is released (then add an image step).
- **Acceptance criteria:** `macp-conform --transport http … --level standard --multi-round --strict --hash-vectors
  in-process` reports `Standard modes + multi_round, Server profile (hash vectors in-process)` in CI;
  `tests/conformance_http.rs` passes every fixture the gRPC path passes; a deliberately broken HTTP mapping makes it
  fail — the hook that flips the Ack is `#[cfg(test)]`-only inside `src/http/` (compiled into unit tests of the root
  crate, never into the binary or the integration-test build), and the negative test lives in that unit-test module.
- **Tests:** above; unit tests for the SSE parser (comments, multi-line `data:` rejected, `event: error`).
- **Docs:** `crates/macp-conform/README.md` (flag, feature, what an HTTP run leaves unverified: the JSON `payload`
  path, covered by P3/P5 tests instead).

### P11 — Docs, examples, image, release notes
- **Status:** TODO
- **Risk:** simple
- **Delivers:** operator and SDK-facing documentation; the image built with the feature.
- **Depends on:** P4-P10.
- **Files:** `docs/API.md` (HTTP binding section per RPC group), `docs/deployment.md` (env vars, TLS, CORS, reverse
  proxy notes: disable response buffering for `text/event-stream`, idle timeouts > 30 s, pass `Last-Event-ID`),
  `docs/examples.md` (curl for Send, GetSession, subscribe with resume, WatchSignals), `docs/getting-started.md`,
  `docs/testing.md`, `docs/architecture.md` (diagram adds the HTTP adapter beside gRPC), `README.md`, `MIGRATING.md`
  (P1 wire change; new feature), `CLAUDE.md` (features table, env table, gRPC/HTTP surface, key files),
  `Dockerfile` default/`docker.yml` build-arg `CARGO_FEATURES=otel,http-listener` (`docker.yml:297-300`),
  `docker-compose.yml`/`deploy/` optional port.
- **Approach:** document only what tests pin; every curl example is executed by a doc test script in CI
  (`scripts/check-http-examples.sh` against a local binary) so docs cannot drift.
- **Acceptance criteria:** the curl script passes in CI; `CLAUDE.md` env table lists every new variable; image runs
  with no `MACP_HTTP_*` vars exactly as before (P4 binary test against the image in `docker-build`).
- **Tests:** the curl script.
- **Docs:** this phase.

## PR strategy

Two PRs.

1. **PR A — `fix(stream)!: compacted resume is HISTORY_UNAVAILABLE` (P0 + P1 + P1b + P2).** Spec re-pin, the
   spec-mandated gRPC changes, SIGTERM drain, and the shared seams. Reviewable entirely on the existing surfaces; P2
   is proven by the characterization snapshot. It carries **every** behaviour change visible without the feature
   (P1's wire changes, P1b's shutdown), so it gets its own release notes and can ship (and be rolled back)
   independently.
2. **PR B — `feat(http): RFC-MACP-0006 §4 HTTP/JSON listener` (P3-P11), one commit per phase.** Every runtime
   behaviour change is behind `http-listener` and `MACP_HTTP_BIND_ADDR`; the only changes outside the gate are
   build/CI/docs (P3's descriptor build step runs only with the feature, the image enables the feature, the
   `macp-conform` `http` feature, new CI steps). If review load demands a cut, split after P6 (codec + listener + unary
   + SSE) and land P7-P11 (CORS, discovery, telemetry, conformance, docs) second; P7 and P8 are independent and
   may move into either half.

Both PRs bump the workspace minor via release-plz (new feature; P1's wire change). `semver_check` should report no
breaks: `LogStore::get_incoming_after_with_high_water` and `HttpTarget` are additive, and no published struct gains a
field.

## Long-term posture

**One-way doors (recommendation in bold):**
1. **JSON dialect.** Fixed by RFC-0001 §10; the runtime's own choices are: **emit `payload` as JSON only when the
   bytes round-trip losslessly, else `payload_b64`**; emit every default (SHOULD); quote only above 2^53−1. Clients
   will parse whatever we emit first.
2. **URL scheme.** `POST {base}/macp/v1/{Method}` is fixed by §4.1. Our choice: **serve both the public-URL prefix
   and the bare `/macp/v1/`** (removing one later breaks someone; adding is free, so this is the generous default
   recorded as reversible only in one direction).
3. **SSE format.** Fixed by §4.3. Our choices: **30 s keepalive, no `retry:` field, `id:` only on subscribe
   envelopes, normal close only after the
   terminal Envelope** (cancel/expire: door 8).
4. **New dependency `prost-reflect`** (root crate, optional, `default-features = false`). **Accept**: de-facto
   prost reflection crate, same maintainer ecosystem as prost; scoped to the feature. `hyper`, `hyper-util`,
   `http-body-util`, `tokio-rustls`, `bytes` become direct (optional) deps but are already in the lock. `reqwest`
   becomes an optional dep of `macp-conform` (`http` feature).
5. **gRPC behaviour change (P1).** **Accept**: spec MUST; status message keeps the old sentence. Missing envelope on
   gRPC **stays `INVALID_ENVELOPE`**.
6. **Names:** env vars `MACP_HTTP_BIND_ADDR`, `MACP_HTTP_PUBLIC_URL`, `MACP_HTTP_CORS_ORIGINS` (brief),
   `MACP_HTTP_MAX_BODY_BYTES`, `MACP_HTTP_MAX_CONNECTIONS` (new); Cargo features **`http-listener`** (root) and
   **`http`** (macp-conform); CLI flag **`--transport http`**.
7. **Send rate-limit classification:** **both per-sender buckets and `max_open_sessions` in band (`200` Ack
   `RATE_LIMITED`), as on gRPC; request-level `429` on `Send` not emitted until a pre-decode per-credential limit
   exists. Precedence 401 > 413 > 400 > Ack.** Moving a limit between layers later changes client-visible status
   codes.
8. **Non-Envelope stream terminals:** **`event: error` `SESSION_NOT_OPEN` on cancel/expire.** Clients will key their
   reconnect logic on it.

**Reversible:** body-cap default formula above the floor, connection cap, keepalive interval, HTTP span shape and
the duration instrument, CORS `Max-Age`, the timeout mapping to `INTERNAL_ERROR`, empty body = 400, refusing `*` in
the CORS list, feature default-off, HTTP/2 tuning values.

**Codec crate extraction (deferred, deliberately):** the codec stays in the root crate as a self-contained module
(`src/http/json/`, no tonic/hyper/server imports, grep-tested). A transport-free `macp-json` crate waits for a real
second consumer (Seam, the A2A binding, an SDK): publishing a new crate name is permanent, it would join the
seven-crate lockstep and semver gate, and there is no consumer yet. Extraction is then a move, not a rewrite.

**Future additive work:** a gRPC entry in `transport_endpoints` (needs a public gRPC URL var); Prometheus HTTP
families; publishing an image-based HTTP conformance step once an image with the feature exists; spec vectors for
§10.13 conversion (none exist; P3's tests are runtime-local).

## Enterprise concerns

- **Auth parity:** same `SecurityLayer` instance, same resolvers, same dev-mode gate; only `Authorization: Bearer`
  (no `x-macp-token`, no cookies); RPCs unauthenticated on gRPC are unauthenticated on HTTP, nothing more.
- **Rate and payload limits:** buckets shared across bindings (one `Arc<RateBucket>`); payload limit on converted
  bytes in band; body cap request-level with the 4/3 floor enforced at startup.
- **CORS safety:** closed by default, exact-origin allowlist, no credentials, `*` refused, denied preflight leaks no
  `Allow-*`.
- **TLS:** same cert/key as gRPC, ALPN h2/http1.1, handshake timeout, plaintext only under `MACP_ALLOW_INSECURE=1`.
- **Timeouts:** unary bounded by `MACP_REQUEST_TIMEOUT_SECS`; HTTP/1 header read timeout; TLS handshake timeout; SSE
  unbounded by design with 30 s keepalive.
- **SSE backpressure / slow consumers:** no unbounded buffering; lag on the per-session (256) or signal broadcast
  terminates the stream with `STREAM_LAGGED` as gRPC does; disconnects free receivers.
- **Connection limits:** global semaphore (`MACP_HTTP_MAX_CONNECTIONS`), h2 `max_concurrent_streams`.
- **Graceful shutdown:** one shutdown signal (SIGINT or SIGTERM, P1b) for gRPC, metrics and HTTP; hyper graceful
  drain; shared hard deadline.
- **Observability:** HTTP SERVER spans and `http.server.request.duration` under `otel`; `process` spans and existing
  Envelope metrics count HTTP traffic identically to gRPC (same code path); no bodies or tokens in telemetry.
- **Zero impact when unconfigured:** no socket, no task, manifest unchanged; the only behaviour changes without the
  feature are PR A's (P1 wire changes, P1b SIGTERM drain), none from PR B; feature off ⇒ the listener code is not
  compiled.

## Open questions

Decided defaults below are what `/implement` builds; each is logged UNCONFIRMED for `/reconcile`.

1. **OQ1 rate-limit layer (decided, one-way door 7, UNCONFIRMED).** Options recorded: **(i, chosen)** both per-sender
   buckets in band on HTTP (`200` Ack `RATE_LIMITED`), because they depend on `message_type` and §4.4 defines
   request-level as "made before the Envelope is decoded"; request-level 429 reserved for a future pre-decode limit.
   (ii) message bucket as request-level 429 decided after decode — rejected: contradicts §4.4's definition and makes
   the same refusal look different on the two bindings. 401 is always before decode.
2. **OQ2 envelope output form (decided, one-way door 1).** Lossless-gated JSON `payload`.
3. **OQ3 path prefix (decided).** Serve both prefixed and bare paths.
4. **OQ4 feature default (decided).** `http-listener` non-default, image enables it; env set without feature is fatal.
5. **OQ5 envelope decode failures (decided).** Envelope-content failures in band (`INVALID_ENVELOPE`), including
   both-or-neither `payload`/`payload_b64`; only the three
   §4.4 body failures are 400.
6. **OQ6 timeout status (decided, reversible).** `500 INTERNAL_ERROR`.
7. **OQ7 subscribe end on cancel/expire (decided, option (a), one-way door 8, UNCONFIRMED).** `event: error`
   `SESSION_NOT_OPEN`, because §4.3 (`:327`) makes any other close without `event: error` a lost connection.
8. **OQ8 gRPC P1 wire change (decided by the spec; one-way door 5).** Route to Fable in the `/reconcile` one-way-door
   tier for confirmation only.
9. **OQ9 SIGTERM (decided, reversible).** Shared shutdown watch on SIGINT or SIGTERM, shipped in PR A as P1b for gRPC
   and metrics, reused by the HTTP listener in P4; gRPC-only deployments now drain on SIGTERM (MIGRATING entry).

**No critical fork without a defensible default.** Nothing needs Fable before implementation.

**Notify (not blocking):** `macp-control-plane` may switch its compacted-resume detection to the coded
`HISTORY_UNAVAILABLE` status once PR A ships (suggested issue title: "Detect compacted StreamSession resume by
macp-error-code HISTORY_UNAVAILABLE (runtime ≥ 0.12)"). Spec repo, optional: "Add RFC-MACP-0001 §10.13 JSON→proto
conversion vectors to schemas/conformance".

## Plan review

**Verdict: SOUND after 2 rounds, with remaining items decided by Opus.**

### Round 1 — REVISE (coordinator review), all applied

1. **HIGH, Send auth order.** gRPC shape-checks (`server.rs:1265`) before `authenticate_metadata` (`:296-298`).
   Changed: HTTP authenticates from headers before reading the body; `send_classified` takes the identity; gRPC
   order kept (rejected alternative (d) in P2). Tests: no token + malformed/oversize/over-cap ⇒ 401.
2. **HIGH, 429 timing.** The message bucket needs `message_type` after the sender/mode checks (`:299-311`). Changed:
   precedence 401 > 413 > 400 > in-band decode > in-band sender/mode > 429 > Ack, with HTTP 429 decided after decode
   (one-way door 7, OQ1). Test: forbidden mode + exhausted bucket ⇒ in-band FORBIDDEN, bucket not charged.
3. **MED, P2 not zero-change.** Policy-engine `macp-error-code` moved to P1. P2 now starts with a byte-level gRPC
   refusal-matrix snapshot captured before the refactor (item 0).
4. **MED, §10.8 numbers.** Decoder accepts quoted strings and integral exponent notation for every integer type, and
   quoted floats; encoder unchanged. Tests added; the "`1e3` rejected" edge case is reversed.
5. **MED, cancel/expire end.** §4.3 `:327` read: a close without `event: error` that does not follow a terminal
   Envelope is a lost connection, and the spec lets any stream end fatally. Chose (a): `event: error`
   `SESSION_NOT_OPEN` (one-way door 8, OQ7, UNCONFIRMED). `Resolved` never closes the stream; cancel/expire drain the
   bus first. Race test added.
6. **MED, lockfile/deps-isolation ownership.** P3 owns both lockfiles for its deps and the new deps-isolation step;
   P10 owns both lockfiles for `reqwest` and adds no deps-isolation step.
7. **MED, slow bodies.** Body collection runs inside `MACP_REQUEST_TIMEOUT_SECS`; HTTP/2 bounds listed (streams,
   header list, pending resets, keepalive). Slow-loris and stream-cap tests added.
8. **LOW, axum rationale.** Restated: `axum::serve` takes any `Listener` and `axum-server` exists; hand-rolled because
   of one dynamic segment and all-custom statuses.
9. **LOW, payload forms.** §10.2 (`RFC-0001:511`) says "MUST include exactly one of", so neither and both ⇒
   `INVALID_ENVELOPE`. P3 and OQ5 updated.
10. **LOW.** The ext-mode test now says "dynamically registered ext mode".
11. **LOW.** Property test replaced by a hand-rolled corpus (no new dev dependency). The P10 flip-the-Ack hook is
    `#[cfg(test)]`-only.
12. **LOW, bearer scheme.** Exact-match `Bearer ` kept on both bindings, with no `SecurityLayer` change; the
    case-insensitive normalisation was removed from P5 and from the reversible list.
13. **LOW, SIGTERM.** P4's shutdown watch is fed by ctrl_c or SIGTERM for both listeners; tested (OQ9).
14. **LOW, stale refs.** §4.3 is at `:329`; `METHODS` is at `grpc.rs:71`.
15. **LOW, descriptor build.** A descriptor-only `protoc --include_imports --descriptor_set_out` run replaces the
    second `tonic_prost_build`.
16. **Q7, `macp-json` crate.** Not now. The codec is a self-contained, grep-tested module; extraction waits for a
    second consumer (Long-term posture).
17. **Weak MUSTs.** Tests added: no prior Initialize, no cookies read or set, never `macp-envelope+json`, and `*/*`,
    `text/*` and a missing `Accept` admitted for SSE.

### Round 2 — 13 of 15 closed; remaining 3 applied

A. **429 precedence.** §4.4 defines request-level as checks "made before the Envelope is decoded", and both buckets
   need `message_type`. Changed: on HTTP the message and SessionStart limits are `200` Ack `RATE_LIMITED`; `Send`
   emits no request-level 429 unless a pre-decode limit is added. Precedence is now 401 > 413 > 400 > Ack. Door 7 and
   OQ1 updated (still UNCONFIRMED, both options recorded), and the P2 seam is simplified to `send_authenticated ->
   Ack`. Tests: an exhausted bucket gives an Ack, never a 429; a rate-limited `WhatIfReplay` gives 429. gRPC today:
   `Send` rate limits are in the Ack (`RATE_LIMITED`), and `GetDecisionReceipt`/`WhatIfReplay` return
   `RESOURCE_EXHAUSTED` statuses. The bindings are now consistent with RFC-0006 §3.1/§4.4.
B. **SIGTERM.** Moved to a new phase, P1b `fix(shutdown)` in PR A, covering gRPC, the metrics listener and the drain
   deadline, with a docs/deployment.md note and a MIGRATING.md entry; P4 only subscribes to it. The PR B statement
   ("every runtime behaviour change is behind the gate") and the Enterprise zero-impact line are corrected.
C. **Stale P2 item (iii)** (policy-engine code, now in P1) is dropped.
