# OpenTelemetry spans and semconv metrics (Wave 5 R2, `macp-semconv/0.1`)

Branch `feat/otel-spans`, based on `main` 611d001 (release 0.11.0).

## Context

### Inputs and authority

- **Brief:** `../multiagentcoordinationprotocol/plans/cross-repo/macp-runtime-wave5.md`, Seam constraints S-a..S-e
  (`:5-10`) and section R2 (`:34-38`).
- **Spec (wins over the brief):** spec repo `origin/main` = `554ee9e` (parent `6c6e3a2`; re-checked at revision
  time, it has moved several times during planning). Spec PR #239 has since merged as `99f3cfa`
  (`docs/opentelemetry.md` `_OTHER` rule); `schemas/otel/macp-semconv.json` is byte-identical there, so the pin
  below stays `0f77753` (P0). The files that matter last changed in
  `0f77753` ("docs(otel): MACP OpenTelemetry semantic conventions macp-semconv/0.1 (W5 P2.1)", #231):
  - `docs/opentelemetry.md` (informative, experimental);
  - `schemas/otel/macp-semconv.json` (the single list of names);
  - RFC-MACP-0006 §2.1 (normative propagation rule);
  - `docs/security.md` "Trace Propagation".
- **The current pin does not contain the semconv file.** CI pins the spec at
  `SPEC_REV = 6bb4426` (`.github/workflows/ci.yml:39`), which is the parent side of `0f77753`.
  `git diff --stat 6bb4426 0f77753` touches only README, `docs/opentelemetry.md`, `docs/security.md`, RFC-0006,
  `schemas/json/macp-semconv.schema.json`, `schemas/otel/macp-semconv.json` and two scripts. It changes nothing
  under `schemas/conformance`, `schemas/parity` or `schemas/json/policy`. Bumping to `0f77753` therefore needs no
  re-vendoring. Bumping to `origin/main` instead would also change `schemas/parity/contract.json` and add
  HTTP-binding fixtures, which is out of scope here.

### What the spec requires of a runtime

**Normative rules (RFC-MACP-0006 §2.1, `docs/security.md`):**
- SHOULD propagate W3C `traceparent`/`tracestate` in gRPC metadata.
- MUST NOT read or write trace context in Envelope fields or in `SessionStartPayload.extensions`.
- `tracestate` MUST NOT carry MACP data.
- Trace context never enters accepted history and has no effect on acceptance or on replay (RFC-MACP-0003).
- On a `StreamSession`, every Envelope inherits the stream-open request's context.
- Trace propagation MUST NOT compromise session isolation.

**Spans:**
- Span name is `{macp.operation.name} {macp.mode.id}`, or the bare operation when the mode is empty.
- `process` is INTERNAL and covers one Envelope "from receipt to the accept/reject decision".
- `resolve|expire|cancel|suspend|resume` are INTERNAL, one per session transition.
- `process` is a child of the transport SERVER span. The runtime creates that span after extracting
  `traceparent`, and it is a child of the client's `send` span.
- `expire` is a timer-driven **root** span.
- `send` (CLIENT) is SDK-only, so the runtime emits no `send` span.

**Per-span attributes** (from `macp-semconv.json`):

| Span | Required | Conditionally required | Recommended |
|---|---|---|---|
| `span.macp.process` | `macp.operation.name`, `macp.message.id`, `macp.message.type`, `macp.envelope.outcome` | `macp.session.id` (session_id non-empty), `macp.mode.id` (mode non-empty), `error.type` (outcome = rejected) | `macp.sender.id`, `macp.version`, `macp.mode.version`, `macp.configuration.version`, `macp.policy.id` (when a policy is bound) |
| `span.macp.resolve` | `macp.operation.name`, `macp.session.id`, `macp.mode.id`, `macp.session.state` (state entered) | | `macp.commitment.hash`, `macp.mode.version`, `macp.configuration.version`, `macp.policy.id` |
| `span.macp.expire`, `.cancel`, `.suspend`, `.resume` | `macp.operation.name`, `macp.session.id`, `macp.mode.id`, `macp.session.state` (state entered) | | |

Value rules:
- `macp.session.state` uses the `SESSION_STATE_*` wire strings.
- `macp.envelope.outcome` is one of `accepted|rejected|duplicate`.
- `error.type` is the MACPError registry code.

**Errors and status:**
- A rejection leaves span status UNSET.
- Status is ERROR only on an implementation failure (storage, panic, transport).
- The status description is the code only. `MACPError.message` and exception text are never recorded.

**Anchor:**
- The anchor is the span context of the accepted SessionStart's `process` span.
- It is stored outside the session's history and never hashed, replayed or sent in an Envelope.
- Every later span of the session links to it, including root `expire`.
- Exposing it on `SessionMetadata` is deferred.

**Metrics:**

| Name | Instrument | Unit | Attributes |
|---|---|---|---|
| `macp.envelope.processed` | counter | `{envelope}` | `macp.mode.id` (conditionally required), `macp.message.type`, `macp.envelope.outcome` |
| `macp.envelope.process.duration` | histogram | `s` | same as above |
| `macp.session.transitions` | counter | `{transition}` | `macp.mode.id`, `macp.operation.name` |
| `macp.session.duration` | histogram | `s` | `macp.mode.id`, `macp.session.state` (terminal) |

- Identifiers are never metric attributes.
- Existing names stay as a compatibility rendering for one minor release.

**Resource:** `service.name` (required), `service.version` (recommended).

**Privacy:**
- Never record by default: payloads, `reason`/`rationale`/`intent` text, extension values, error or exception
  messages.
- `context_id` is opt-in and has no name in this version.
- A deployment MAY hash `macp.sender.id`.

**Embedded libraries:**
- Library spans are debug/trace only and carry ids and types only.
- Stepping or replaying behaves identically with tracing on or off.

### Brief vs spec divergences (the spec wins)

1. **Anchor store** (confirmed by the spec session). The brief says only "session metadata store" (with `#[serde(default)]`); it does not name a
   type. One plausible reading is a field on `Session`/`PersistedSession`, and that reading would violate the spec: `PersistedSession` is serialized
   **into the log** as the checkpoint entry (`crates/macp-storage/src/registry.rs:14` "for the storage backends and
   the checkpoint entry"; `src/replay.rs:41-52` restores from it), and the spec says the anchor is "never replayed".
   This plan stores the anchor in a **side store** next to the session snapshot, through new defaulted
   `StorageBackend` methods, never in `Session`, `PersistedSession` or a `LogEntry` (P5). The anchor record carries
   a version field, which is the intent of the brief's `serde(default)`.
2. **"Spans created inside macp-core or macp-modes".** The brief (S-b) assumes library spans exist. The spec makes
   them conditional ("Spans created inside the embedded libraries … are emitted at debug or trace"). Every name the
   spec defines can be produced in the root crate, so this plan creates **no spans in macp-core or macp-modes**.
   S-b and S-c then hold trivially, and Seam's in-process `step`/`replay` cost does not change.
3. **Span-name cardinality for unregistered modes.** The template would put a client-chosen, unregistered `mode`
   into the span name. The spec's own rationale ("keeps span-name cardinality bounded by the number of modes")
   forbids that. This plan names the span `process _OTHER` when the mode is non-empty but not in the registry
   (OTel's conventional out-of-set token, as used for `http.request.method`), and keeps the raw value only as the
   (truncated) `macp.mode.id` attribute. An empty mode still gets the bare `process` the spec defines. Confirmed by
   the spec session (spec PR #239, `docs/opentelemetry.md`), which extends it to every op templated on mode and to
   metric attributes (`_OTHER` for an unsupported mode or message type). OQ3 is resolved.
4. **Exporter trigger.** The brief says "exporter only when `OTEL_EXPORTER_OTLP_ENDPOINT` is set". This plan also
   activates on the signal-specific `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`/`_METRICS_ENDPOINT`, because an operator
   who sets only those would otherwise get silent nothing. It is a superset of the brief (OQ4).
5. **Parent of `process`** (confirmed by the spec session). The brief has the client's `traceparent` parenting the `process` span directly. The
   spec makes `process` a child of the transport SERVER span, which is itself the child of the client's `send`
   span. This plan follows the spec: client `send` → SERVER (P2) → `process` (P3). Before P2 lands, `process` is
   parented by nothing (a root); it is never parented directly by the extracted remote context.

### Current code (read, not assumed)

- **The `otel` feature exists** (`Cargo.toml:78`):
  - It pulls opentelemetry 0.32, opentelemetry_sdk 0.32 (lock 0.32.1), opentelemetry-otlp 0.32 with
    `default-features=false, features=["grpc-tonic","trace"]` (`Cargo.toml:107-118`), and tracing-opentelemetry 0.33.
  - The only use is `src/main.rs:194-224`.
- **Defects in the current otel code:**
  - It **defaults the endpoint to `http://localhost:4317`** (`src/main.rs:201-202`), so an `otel` build always
    exports. That contradicts "exporter only when set".
  - `.with_endpoint()` is programmatic (`:205`), which overrides `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`.
  - No resource is set, so `service.name` becomes the SDK's `unknown_service` unless `OTEL_SERVICE_NAME` is set.
  - No propagator is installed, and no span is ever created.
  - `EnvFilter` is a **global** layer (`:214-215`), so `RUST_LOG=warn` would silently disable info-level telemetry
    spans.
  - Provider shutdown is unbounded (`:609-612`).
- **No spans exist anywhere.**
  - `grep` for `instrument|info_span|debug_span|span!|Span::` over `src/` and `crates/` matches nothing (one comment
    in `src/watch_sync.rs:21`).
  - `tracing` is a dependency of `macp-modes` (`crates/macp-modes/Cargo.toml:19`), `macp-storage` and `macp-auth`.
    It is **not** a dependency of `macp-core`.
- **Privacy hazard in existing logs.**
  - `cancel_session`/`suspend_session`/`resume_session` log `reason` at info (`src/runtime.rs:1391`, `:1448`,
    `:1544`).
  - tracing-opentelemetry exports `tracing` events inside a span as span events by default. Without a spans-only
    filter on the otel layer, the client's free-text `reason` would leave in telemetry.
- **Server surface:**
  - `Send` is `src/server.rs:1134-1178`:
    1. validate shape (`:1143`);
    2. authenticate, authorize mode, rate limit and ingress policy (`authenticate_send_request` `:268-293`);
    3. `runtime.process` (`:1145`);
    4. rejections become an `Ack` via `make_error_ack` (`:195-215`), which puts `e.to_string()` in the Ack. That
       message must not reach telemetry.
  - `StreamSession`: the handler authenticates at `src/server.rs:1503-1516`, and the body is an `async_stream`
    built in `build_stream_session_stream` (`:607-…`). It is polled by hyper **outside** the handler's future, so
    the stream-open span must be captured explicitly.
  - `process_stream_request` (`:399-516`) and `StreamFailure` (`:1022-1034`): `Terminal(Status)` loses the
    `MacpError`, which telemetry needs for `error.type`.
  - The tonic server is built at `src/main.rs:413-418`, with services added at `:475-480`.
- **Kernel** (`src/runtime.rs`):
  - `process` dispatch is at `:593-603`.
  - `process_session_start` is `:605-835`: commit point `:758-779`, `record_session_start` `:802`.
  - `process_message` is `:1002-1140`: Resolved at `:1116-1123`.
  - `process_signal` is `:1146-1172`.
  - Every live state transition sits next to exactly one `record_session_*` call:

    | Transition | Site | Function |
    |---|---|---|
    | expire | `:581` | `maybe_expire_session` `:562-591` |
    | resolve | `:1117` | `process_message` |
    | cancel | `:1390` | `cancel_session` `:1335` |
    | suspend | `:1447` | `suspend_session` `:1407` |
    | resume | `:1543` | `resume_session` `:1464` |
    | expire (resume force-expire) | `:1576` | `resume_session` |
    | expire (background sweep) | `:1741` | `cleanup_expired_sessions` `:1709-1758` |

  - `maybe_expire_session` is also reached from `get_session_checked` (`:1174-1185`), so a **read RPC** can expire a
    session. `:749` (start rollback) is not a transition.
  - Eviction is `evict_stale_sessions` `:1944` and disk GC is `gc_disk_sessions` `:1885`.
  - `ProcessResult` (`:20-23`) is a public, exhaustive struct. Do not add fields to it.
- **Background loop:** `src/main.rs:500-522`. **Prometheus endpoint:** `src/main.rs:528-570`, hand-rolled, rendering
  `ModeMetrics::prometheus_lines` (`src/metrics.rs:241-258`): `macp_*_total{mode=…}` plus replay and receipt lines.
  The per-mode cardinality cap is `MAX_MODE_CARDINALITY = 1000` (`src/metrics.rs:43`).
- **Storage trait:** `crates/macp-storage/src/storage/mod.rs:45-89`. The receipts precedent added defaulted methods
  (`load_receipt`, `store_receipt_if_absent`, `receipts_durable`) with per-backend overrides:
  - file `receipt.json`: `file.rs:35`;
  - rocksdb CF `receipts`: `rocksdb.rs:12,29`, `create_missing_column_families(true)` `:25`;
  - redis key `:receipt:`: `redis_backend.rs:42`, deleted in `delete_session` `:103-111`.

  `FileBackend::delete_session` removes the whole session directory (`file.rs:203-210`).
- **CI:**
  - The `features` job runs **clippy only** for `--features otel` (`ci.yml:491-493`), never tests.
  - The `conformance-oracle` job's `check_dir` (`ci.yml:637-669`) byte-compares vendored dirs against the spec at
    `SPEC_REV`.
  - `deps-isolation` (`ci.yml:283-330`) greps `cargo tree -p macp-core` for `tonic|tokio|…` and
    `-p macp-modes` for `tonic|macp-policy|…`. macp-modes already depends on tokio, which the job allows.
- **Docker:**
  - `Dockerfile` runs `cargo build --release` with no features and no build-arg.
  - `docker.yml:293` builds amd64 and arm64.
  - Pre-existing drift: the Dockerfile uses `rust:1.98-bookworm` while `rust-toolchain.toml` pins `1.96.1`, despite
    the "keep in sync" comment. Measure on the Dockerfile as it is and mention the drift, but do not fix it here.
- **The spec's SDK-sync script** maps `schemas/otel` to `tests/otel` (`scripts/check-sdk-sync.py:28`). That is the
  layout to mirror.

### Design summary (decided)

**Where the code lives:**
- Everything lives in the root crate, in a new `src/telemetry/` module.
- Library crates gain no spans and no dependencies.
- `macp-storage` gains two defaulted trait methods for the durable anchor (P5).

**Which runtime switch:**
- "Active" means the `otel` feature is compiled **and** an OTLP endpoint variable is set **and**
  `OTEL_SDK_DISABLED` is not `true`.
- When inactive (always so with the feature off), every telemetry entry point returns `Span::none()`/no-op: no
  layer, no propagator, no anchor writes, no new log lines.
- The switch is a `Telemetry` value injected into `Runtime`/`MacpServer`, not a global static, so tests can run
  active and inactive side by side.

**Span factory:**
- `info_span!` uses a fixed `target: "macp_runtime::telemetry"` plus the tracing-opentelemetry special fields
  `otel.name`, `otel.kind`, `otel.status_code` and `otel.status_description` (`tracing-opentelemetry-0.33.0/src/layer.rs:30-33`).
- Parenting uses `OpenTelemetrySpanExt::set_parent`, and links use `add_link` (`span_ext.rs:60,98`).

**Subscriber layout when active:**
- `fmt` keeps `EnvFilter` as a **per-layer** filter, built from the same `RUST_LOG`/`"info"` directive as today
  **plus the directive `macp_runtime::telemetry=off`**. Without that directive the fmt layer *would* see telemetry
  spans: `EnvFilter::new("info")` enables every target at info, including `macp_runtime::telemetry`, so every log
  line inside a request would gain a `grpc{..}:process{..}:` span-context prefix. With it, log output is unchanged
  while telemetry is active (P1 test). An operator `RUST_LOG` that itself names `macp_runtime::telemetry=info`
  would tie with our directive on specificity, so do not rely on directive precedence: compose the fmt filter as
  `env_filter.and(filter_fn(|m| m.target() != telemetry::TARGET))` (`tracing_subscriber::filter::FilterExt::and`),
  which excludes the target whatever `RUST_LOG` says. Adding `macp_runtime::telemetry=off` to the `EnvFilter` as
  well is harmless and documents intent.
- The otel layer gets its own filter: spans only, target `macp_runtime::telemetry` only, with
  `with_threads(false)`, `with_location(false)` and `with_tracked_inactivity(false)`.
- `RUST_LOG` therefore cannot silence telemetry, and no `tracing` event (with its `reason` or `error` text) is ever
  exported.

**Span placement:**

| Span | Created in | Parent |
|---|---|---|
| gRPC SERVER | tower-http `TraceLayer` (main) | extracted remote context |
| `process` | `server.rs` (`send`, `process_stream_request`) | current span (Send) or the stream-open span (stream) |
| `resolve` | `runtime.rs` `process_message` | current span (`process`) |
| `cancel`/`suspend`/`resume` | `runtime.rs` | current span (the RPC's SERVER span) |
| `expire` | `runtime.rs` (all 3 expire sites) | **none (root)**, even when observed lazily inside a request |

**Anchor:**
- Taken from the `process` span's context right after the SessionStart commit point.
- Cached in memory in `Runtime`. P5 persists it through the storage side store as
  `{"v":1,"traceparent":"00-…"}` and loads it lazily.
- Lookups happen **only for sessions resident in the registry** (`registry.get_shared(sid)` is `Some`). An id that
  is not resident is never looked up and never cached, so a client cannot grow the cache by sending rejected
  envelopes with arbitrary `session_id`s. A negative result is cached only for a resident session, and is dropped
  with it on eviction or GC.

**Metrics:**
- One recorder in `RuntimeMetrics` feeds both paths:
  - Prometheus rendering of the semconv names, always on (dependency-free, on the existing `/metrics`);
  - OTel instruments when active (OTLP `PeriodicReader`).
- The old `macp_*_total{mode=}` lines are unchanged and slated for removal in the next minor after this ships.

**CURRENT_SEMANTICS_REV stays 4.** Nothing about acceptance changes.

## Phases

The phases are ordered for independent shippability. Each one leaves `main` releasable with the feature off
and with the feature on.

### P0 — Spec pin, vendored semconv, otel tests in CI
- **Status:** DONE (2026-10-31). One addition beyond the plan: `spec-drift.yml` watches `schemas/otel`, so the new vendored tree has a drift watcher. SOURCE.md dates are the import date, by convention.
- **Risk:** simple
- **Delivers:**
  - `SPEC_REV` moves to a spec commit that contains `schemas/otel/macp-semconv.json`.
  - The file is vendored byte-identical under `tests/otel/` and drift-checked.
  - A semconv loader test helper exists.
  - CI compiles **and tests** `--features otel`.
- **Depends on:** nothing.
- **Pin: `0f77753`, unconditionally.** Spec PR #239 merged as `99f3cfa`. It is cited only as the source of the
  `_OTHER` naming rule (P3, P4); the semconv file is byte-identical between `0f77753` and `99f3cfa`. `99f3cfa`
  also changes `schemas/parity/contract.json` (`RECEIPT_UNAVAILABLE` becomes permanent, which would fail
  `error_codes_permanent_set_matches_runtime`), `registries/error-codes.md` and the HTTP fixtures. Re-vendoring
  that contract is out of scope for R2.
- **Files:**
  - `.github/workflows/ci.yml`:
    - `SPEC_REV` (`:39`) set to `0f77753c…` (full SHA from `git rev-parse 0f77753`).
    - Add `check_dir "tests/otel" "spec-repo/schemas/otel"` after `:669`.
    - `features` job: add `cargo test -p macp-runtime --features otel` after `:493`.
  - `tests/otel/macp-semconv.json`: `git show 0f77753:schemas/otel/macp-semconv.json`, byte-identical.
  - `tests/otel/SOURCE.md`, mirroring `tests/conformance/SOURCE.md`: the commit and date, "gated by `check_dir`",
    and re-vendor steps.
  - Update the commit and date in `tests/conformance/SOURCE.md`, `tests/parity/SOURCE.md` and
    `tests/vectors/receipt/SOURCE.md` (they state "must stay equal to `SPEC_REV`"). Their bytes do not change.
  - `tests/otel_semconv.rs` (always compiled):
    - a small `Semconv` loader (serde_json over the vendored file) exposing span groups (name template, kind,
      attribute refs and requirement levels), metric groups (name, instrument, unit, attributes) and enum members;
    - a test that asserts the file's `schema == "macp-semconv/0.1"` and that the set of span and metric group ids
      equals the set this runtime implements (`process`, the five transitions, the four metrics; `send` listed as
      SDK-only).
  - Put the loader in `tests/common/otel_semconv.rs` (or `tests/support/`) so P2-P4 tests reuse it.
- **Approach:**
  - Pin `0f77753`, not spec `main`. `0f77753` changes nothing this repo vendors except the new otel file (verified
    with `git diff --stat 6bb4426 0f77753 -- schemas/conformance schemas/parity schemas/json/policy`, which is empty).
  - **Rejected:** spec `main`. It drags in the parity-contract change and HTTP-binding fixtures, which are another
    wave's work.
- **Edge cases & failure modes:**
  - The `spec-drift.yml` workflow parses `SPEC_REV` (`spec-drift.yml:62-65`), so keep the line's format.
  - The parity test reads `MACP_POLICY_SCHEMAS_DIR` at `SPEC_REV`. The policy schemas do not change across the bump.
- **Acceptance criteria:**
  - The `conformance-oracle` job passes with the new `check_dir` line.
  - Deleting a byte from `tests/otel/macp-semconv.json` makes it fail with `DRIFT`.
  - `cargo test -p macp-runtime --features otel` runs in CI and is green.
  - `cargo test --test otel_semconv` is green on the default features.
- **Tests:** `tests/otel_semconv.rs` as above.
- **Docs:** `tests/otel/SOURCE.md`. `docs/testing.md` gets one line saying the features job now tests `otel`.

### P1 — Telemetry bootstrap: activation, resource, propagator, exporter, subscriber layout
- **Status:** DONE (2026-10-31). Divergences, each recorded in ASSUMPTIONS.md:
  - The endpoint is resolved in code, using the exporter's precedence, and passed to `with_endpoint`.
  - No `macp_runtime::telemetry=off` directive. EnvFilter matches by prefix and would silence `telemetry::init`, so the exact `filter_fn` does the exclusion alone.
  - `init` installs the fallback subscriber.
  - A configured `OTEL_EXPORTER_OTLP_CERTIFICATE` is the only trust anchor; native roots are used only without it.
  - Shutdown runs on a detached `std::thread`, bounded at 5s.
  - The endpoint userinfo is redacted in errors.
  - The test hooks are `active_for_tests` and `Guard::from_tracer_provider`, both `#[doc(hidden)]`.
- **Risk:** complex
- **Delivers:**
  - Correct, opt-in OTel initialization, including the fixes to the four defects listed in Context.
  - No MACP spans yet. The pipeline is ready and inert unless configured.
- **Depends on:** P0 (for the CI test step). It can ship alone.
- **Also delivers the `Telemetry` plumbing** (moved here from P4 so that P3 ships without P4): the `Runtime`
  field, `with_telemetry`, and the `main.rs`/`MacpServer` wiring. With no spans yet, the injected value is unused
  apart from tests.
- **Files:**
  - `src/telemetry/mod.rs` (new, `pub mod telemetry` in `src/lib.rs`):
    - `pub struct Telemetry`. With the feature on it holds an `enabled: bool` and optional instruments. With it off
      it is a ZST.
    - `Telemetry::disabled()`.
    - `pub const TARGET: &str = "macp_runtime::telemetry"`.
  - `src/telemetry/init.rs` (`#[cfg(feature = "otel")]`):
    - `pub fn activation(env: &impl Fn(&str) -> Option<String>) -> Activation`, a pure function for unit tests.
    - `pub fn resource(env) -> Resource`.
    - `pub fn init(env_filter) -> Result<Option<Guard>, String>`. It returns `None` when inactive, and the caller then
      installs today's exact `tracing_subscriber::fmt().with_env_filter(..).init()`.
    - `Guard` owns the providers and implements bounded shutdown, and exposes `telemetry() -> Telemetry`.
  - `src/runtime.rs` (moved from P4): `Runtime` gains a private `telemetry: Telemetry` field, defaulting to
    `Telemetry::disabled()` in every existing constructor, and `pub fn with_telemetry(self, Telemetry) -> Self`
    plus a `pub(crate) fn telemetry(&self) -> &Telemetry`. Both are additive.
  - `src/server.rs` (moved from P4): `MacpServer` reads `Telemetry` from `runtime`, so there is no new
    constructor parameter and no test churn.
  - `src/main.rs`:
    - Replace `:190-229` with a call into `telemetry::init`. The feature-off arm is literally unchanged.
    - `Runtime::with_registries(..).with_telemetry(guard.map(Guard::telemetry).unwrap_or_else(Telemetry::disabled))`
      (moved from P4).
    - Replace `:607-612` with `guard.shutdown()`, bounded.
    - `validate_env_config` (`:18-127`) gains the OTEL checks listed below.
  - `Cargo.toml`:
    - opentelemetry-otlp features add `"metrics"` (P3 uses it) and `"tls-ring","tls-roots"` (OQ6). Note that
      `tls-roots` only **compiles** native-roots support (it maps to tonic's `tls-native-roots`); it loads nothing
      on its own. otlp 0.32 builds an https channel with a bare `ClientTlsConfig::new()`
      (`opentelemetry-otlp-0.32.0/src/exporter/tonic/mod.rs:323-324`), and tonic only loads native roots when the
      config asks (`tonic-0.14.6/src/transport/channel/service/tls.rs:86`). See "TLS" under Approach.
    - New optional `tower-http = { version = "0.6", features = ["trace"] }`, `http = "1"` and
      `tower = { version = "0.5", features = ["util"] }` under `otel` (P2 uses them; the root crate has **no**
      direct `tower` dependency today, so `option_layer` needs this one).
    - `[dev-dependencies]` adds `opentelemetry_sdk = { version = "0.32", features = ["testing"] }`, `opentelemetry`
      and `tracing-opentelemetry` for the in-memory exporters.
  - `integration_tests/Cargo.lock`: regenerate (`cargo metadata --manifest-path integration_tests/Cargo.toml
    --format-version 1`), per CLAUDE.md "Two lockfiles".
- **Approach:**
  - **Activation:**
    - Active iff:
      1. the feature is compiled; and
      2. any of `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` or
         `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` is set and non-empty; and
      3. `OTEL_SDK_DISABLED` is not `true` (case-insensitive).
    - Traces export iff the generic or traces endpoint is set. Metrics export iff the generic or metrics endpoint
      is set.
    - Do **not** call `.with_endpoint()`. The exporter then reads the generic and signal-specific variables itself,
      in the standard precedence (`opentelemetry-otlp-0.32.0/src/lib.rs:315-316`).
  - **Resource:**
    - `service.name`: `OTEL_SERVICE_NAME`, else `service.name` from `OTEL_RESOURCE_ATTRIBUTES`, else
      `"macp-runtime"`.
    - `service.version = env!("CARGO_PKG_VERSION")`.
    - Build it from `Resource::builder_empty()`: the default `service.name`, then the env detector, then an explicit
      `OTEL_SERVICE_NAME` override. The SDK default would otherwise yield `unknown_service`.
  - **Propagator:** `global::set_text_map_propagator(TraceContextPropagator::new())` only when active. No baggage
    propagator, because the spec names `traceparent`/`tracestate` only.
  - **Sampler:** the SDK default (`parentbased_always_on`), which honours `OTEL_TRACES_SAMPLER`/`_ARG`. Verify that
    0.32's `trace/config.rs` reads them, and document it.
  - **Subscriber layout:** described in the Design summary.
  - **TLS:**
    - For an `https://` endpoint, `init` calls `.with_tls_config(ClientTlsConfig::new().with_enabled_roots())`
      (`opentelemetry_otlp::tonic_types::transport::ClientTlsConfig`, re-exported at
      `opentelemetry-otlp-0.32.0/src/lib.rs:856`; `with_enabled_roots` at `tonic-0.14.6/src/transport/channel/tls.rs:119`).
      Without that call an https export fails the handshake for every public CA.
    - `OTEL_EXPORTER_OTLP_CERTIFICATE` (and `_TRACES_`/`_METRICS_`) is **not honoured** by otlp 0.32 (no reference
      in its source). The plan implements it: when set, read the PEM file and add it with
      `ClientTlsConfig::ca_certificate(Certificate::from_pem(..))` in addition to the enabled roots. An unreadable
      file is handled like the protocol case below (error logged, telemetry inactive). Client-certificate variables
      (`OTEL_EXPORTER_OTLP_CLIENT_KEY`/`_CLIENT_CERTIFICATE`) stay unsupported and documented as such.
  - **Startup checks** (feature on). None of them aborts startup; telemetry never decides whether the runtime runs:
    - `OTEL_EXPORTER_OTLP_PROTOCOL` (or `_TRACES_`/`_METRICS_`) set to anything other than `grpc` logs one
      `error!` naming the variable and leaves telemetry **inactive** (the fmt-only subscriber is installed). Only
      gRPC is compiled, and exporting with the wrong protocol would fail silently.
    - An endpoint that does not parse as an `http(s)://` URI: same handling (error logged, inactive).
    - These checks live in `telemetry::init`, not in `validate_env_config`, because that function's failures are
      fatal.
  - **Feature off and an OTEL endpoint variable set:** one `warn!` at startup ("this binary was built without the
    `otel` feature; OTEL_* ignored"). It is emitted only when such a variable is set, so the no-vars output is
    unchanged.
  - **Shutdown:** use `shutdown_with_timeout(5s)` if 0.32 has it, else run `shutdown()` in `spawn_blocking` under
    `tokio::time::timeout(5s)`. Either way, a dead collector cannot extend `MACP_SHUTDOWN_DRAIN_SECS`.
  - **Rejected:**
    - Keeping the localhost default. It contradicts the brief, and an image built with `otel` would dial 4317 on
      every start.
    - A global `static ENABLED`. Tests cannot run active and inactive in one process.
    - Keeping `EnvFilter` global. `RUST_LOG` would silently disable telemetry.
- **Edge cases & failure modes:**
  - An unreachable collector at startup is not fatal: the export fails in the background, and the OTel
    internal-logs reach the fmt layer at warn, filtered by `RUST_LOG`.
  - An internal-logs feedback loop is impossible, because the otel layer only admits target
    `macp_runtime::telemetry`.
  - `OTEL_SDK_DISABLED=true` together with an endpoint means inactive.
  - Empty-string endpoint variables are treated as unset.
- **Acceptance criteria:**
  - The unit tests for `activation` cover: no vars → inactive; generic → traces+metrics; traces-only → traces
    only; `OTEL_SDK_DISABLED=TRUE` → inactive; empty string → inactive.
  - The unit tests for `resource` cover: no vars → `service.name=macp-runtime`, `service.version=<pkg>`;
    `OTEL_SERVICE_NAME=x` → `x`; `OTEL_RESOURCE_ATTRIBUTES=service.name=y` → `y`; both set → `x`.
  - A binary test (`tests/otel_bootstrap.rs`, `#![cfg(feature = "otel")]`, `env!("CARGO_BIN_EXE_macp-runtime")`
    like `tests/policies_dry_run.rs`):
    - `MACP_POLICIES_DRY_RUN=1` with no OTEL vars gives stdout and stderr byte-identical to a **checked-in
      golden** (`tests/golden/policies_dry_run.txt`, timestamps and ANSI stripped). The same golden is asserted by
      the default-feature run of the test, so both builds are pinned to one file; there is no "compare with a
      feature-off run" step, which a single `cargo test` invocation cannot perform.
    - `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf` plus an endpoint: the process **starts** (health Check
      SERVING), stderr carries one error line naming the variable, and the fake collector of P6 receives nothing.
  - **Log lines unchanged while telemetry is active** (`tests/otel_bootstrap.rs`, in-process): install the active
    subscriber layout with an in-memory span exporter and a fmt layer writing to a buffer
    (`with_writer(MakeWriter)`, `without_time()`), then run one Send through a server that emits P2/P3 spans. The
    buffer is byte-identical to the same scenario with the fmt-only subscriber, and contains neither `grpc{` nor
    `process{`. Also run it with `RUST_LOG=macp_runtime::telemetry=info` to pin the `filter_fn` exclusion. (P1
    ships the test with a hand-made `info_span!(target: TARGET, ..)` standing in for P2/P3 spans; P3 switches it to
    the real ones.)
  - **TLS** (`tests/otel_bootstrap.rs`): an `https://127.0.0.1:<port>` fake OTLP collector serving a certificate
    from a test CA (generated with `rcgen`, a dev-dependency; the repo's TLS integration tests already use test
    certificates). With `OTEL_EXPORTER_OTLP_CERTIFICATE=<ca.pem>` the collector receives a span; without it, the
    export fails the handshake (no span received, runtime unaffected). This proves both the `with_enabled_roots`
    code path is taken for https and the certificate variable is honoured. A public-CA case is not testable
    offline; it is covered by the code path and by P6's manual check.
  - `cargo clippy -p macp-runtime --features otel --all-targets -D warnings` is green, and so is the default build.
- **Tests:** as above, plus a `#[cfg(not(feature = "otel"))]` unit test that `Telemetry` is a ZST.
- **Docs:**
  - `docs/deployment.md` "Environment variables" gets an OTEL subsection: `OTEL_EXPORTER_OTLP_ENDPOINT`,
    `_TRACES_ENDPOINT`, `_METRICS_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME`,
    `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_TRACES_SAMPLER`/`_ARG`, `OTEL_METRIC_EXPORT_INTERVAL`, `OTEL_SDK_DISABLED`,
    `OTEL_EXPORTER_OTLP_PROTOCOL` (grpc only; anything else logs an error and leaves telemetry off),
    `OTEL_EXPORTER_OTLP_CERTIFICATE` (private CA PEM, read by the runtime), and a note that client-certificate
    variables are unsupported.
  - The same section notes that the variables take effect only in an `otel` build.
  - An upgrade note: an `otel` build no longer exports to `localhost:4317` by default.

### P2 — gRPC SERVER span with W3C extraction
- **Status:** DONE (2026-10-31). Divergences, each recorded in ASSUMPTIONS.md:
  - `on_response` and `on_eos` record the code. `on_failure` handles only body/transport errors, because tower-http never fires it for trailer codes.
  - ERROR is set through `otel.status_description` alone.
  - Unknown methods are named `_OTHER` in both the span name and `rpc.method`.
  - The body is re-boxed so `option_layer` type-checks.
  - The RPC attributes use the pre-1.37 OTel names.
  - Known limitation: tonic's request timeout sits outside `Server::layer` and answers CANCELLED before the headers go out. Such a span loses only `rpc.grpc.status_code=1`; its status would be Unset either way.
- **Delivers:**
  - Each `macp.v1.MACPRuntimeService` RPC gets a SERVER span whose parent is the client's propagated context.
  - The span lasts until the response body ends, which matters for streams.
- **Depends on:** P1.
- **Files:**
  - `src/telemetry/grpc.rs` (`#[cfg(feature = "otel")]`):
    - `pub fn server_layer() -> TraceLayer<GrpcMakeClassifier, MakeMacpSpan, (), (), (), OnEos, OnFailure>`, or an
      opaque `impl Layer` wrapper.
    - A `HeaderExtractor` over `http::HeaderMap` (`opentelemetry::propagation::Extractor`).
  - `src/main.rs`: when active, `Server::builder()...layer(server_layer())` before `add_service` (`:413-480`).
    Use `tower::util::option_layer(active.then(server_layer))` (needs the direct optional `tower` dependency added
    in P1; the root crate has none today). If its `Either` service fails tonic 0.14's `Server::layer` bounds, fall
    back to two code paths. Either way, the inactive path's behaviour is unchanged, and the feature-off build does
    not reference `tower` at all.
- **Approach:**
  - **Library:** tower-http `TraceLayer::new_for_grpc()` with:
    - `make_span_with`: return `Span::none()` unless the path starts with `/macp.v1.MACPRuntimeService/`. This
      excludes `grpc.health.v1` and reflection.
    - Otherwise create `info_span!(target: TARGET, "grpc", otel.name = "macp.v1.MACPRuntimeService/{Method}",
      otel.kind = "server", rpc.system = "grpc", rpc.service = "macp.v1.MACPRuntimeService", rpc.method = "{Method}",
      rpc.grpc.status_code = Empty, otel.status_code = Empty)`.
    - Then `span.set_parent(global::get_text_map_propagator(|p| p.extract(&HeaderExtractor(req.headers()))))`.
      `set_parent` returns `Result<(), SetParentError>` (`tracing-opentelemetry-0.33.0/src/span_ext.rs:60`); it
      fails only when the span is disabled or already started. Handle it with `if let Err(e) = .. { debug!(..) }`
      (target **not** `TARGET`, so the event never reaches the otel layer) and continue with a root span. The same
      handling applies to every other `set_parent` call in this plan.
    - `on_request`, `on_response` and `on_body_chunk` are `()`, so tower-http emits no log events.
    - `on_eos` and `on_failure` record `rpc.grpc.status_code` from the trailers or the trailers-only headers.
      `otel.status_code = "error"` is set only for the OTel gRPC semconv's SERVER-side error set:
      `UNKNOWN|DEADLINE_EXCEEDED|UNIMPLEMENTED|INTERNAL|UNAVAILABLE|DATA_LOSS`. Every other status
      (`NOT_FOUND`, `INVALID_ARGUMENT`, `PERMISSION_DENIED`, …) stays UNSET. `otel.status_description` is the
      status code name only, never the `grpc-message`.
  - No peer-address attributes: an IP is personal data under several regimes, and the spec does not ask for it.
  - tonic handlers run inside the tower-http span (its `ResponseFuture` enters the span on poll), so
    `Span::current()` in `send` is the SERVER span.
  - **Rejected:**
    - tonic's `Server::trace_fn`. It ends the span when the handler future resolves, before a stream's body ends,
      and it cannot see the grpc-status in trailers.
    - A hand-written tower `Service` with its own body wrapper. It duplicates tower-http's eos handling. Keep it as
      the fallback if tower-http's types will not satisfy tonic 0.14's `Server::layer` bounds.
    - An interceptor. It cannot wrap the response future or the body.
- **Edge cases & failure modes:**
  - A malformed or absent `traceparent` gives a new root trace. Never an error.
  - A `tracestate` is carried inbound into the child context only. The runtime makes no outbound calls that
    inject it (the JWKS fetch in `macp-auth` does not inject), so the runtime never writes MACP data into
    `tracestate`.
  - Long-lived `Watch*` streams keep their SERVER span open until the stream ends. The batch processor exports it
    then, and it is dropped on hard shutdown, which is acceptable.
  - The layer must not change `max_decoding_message_size`, the timeout or concurrency semantics. It sits outside
    `add_service`, like tonic's own layers.
- **Acceptance criteria:**
  - `tests/otel_grpc.rs` (`#![cfg(feature = "otel")]`) starts a tonic server in-process on `127.0.0.1:0`, built
    with `server_layer()` and an `InMemorySpanExporter`-backed provider in a scoped subscriber, and checks:
    1. With a client `traceparent` `00-<T>-<S>-01`, the exported SERVER span has trace id `T`, parent span id `S`,
       kind `Server` and name `macp.v1.MACPRuntimeService/Send`.
    2. Without a header, the SERVER span is a root.
    3. A `grpc.health.v1.Health/Check` call exports no span.
    4. A `GetSession` on an unknown session records `rpc.grpc.status_code = 5` (NOT_FOUND) with status Unset.
    5. A unit test of the status classifier: each of codes 2, 4, 12, 13, 14, 15 maps to ERROR and every other code
       (0-16) maps to Unset. An unknown-method call through the layer (tonic answers UNIMPLEMENTED, code 12)
       exports a SERVER span with status Error.
  - Inactive (no layer) gives identical responses for the same calls. Compare the decoded `SendResponse`s,
    excluding `accepted_at_unix_ms`.
- **Tests:** as above.
- **Docs:** `docs/deployment.md` Monitoring: a "Tracing" paragraph naming the SERVER span and W3C propagation in
  gRPC metadata only (RFC-MACP-0006 §2.1).

### P3 — `process` spans and the Envelope metrics
- **Status:** DONE (2026-10-31). Divergences, each recorded in ASSUMPTIONS.md:
  - `StreamFailure::Terminal` carries a non-optional `MacpError`.
  - Tracer and meter shutdown run concurrently, so the 5s bound still holds.
  - ERROR is set through `otel.status_description` alone. A dropped or panicked span also records `error.type=_OTHER`.
  - The mode-label cache is read-mostly: the snapshot is rebuilt outside the lock and swapped in.
  - One recorder feeds both Prometheus and OTLP, so the 2,000-label-set cap applies to both.
  - `macp.sender.id` is recorded only after full authentication.
  - Known pin lag: the `_OTHER` rule comes from spec 99f3cfa, which is newer than `SPEC_REV` 0f77753. The vendored semconv is byte-identical at both.
- **Delivers:**
  - One `process {mode}` INTERNAL span per Envelope on `Send` and `StreamSession`, with every required and
    conditionally-required attribute and the recommended ones.
  - Outcome, `error.type` and status follow the spec.
  - `macp.envelope.processed` and `macp.envelope.process.duration` are emitted both as OTel instruments (when
    active) and on the Prometheus endpoint (always).
- **Depends on:** P1 (the `Telemetry` plumbing in `Runtime`/`MacpServer` now lands there, so P3 does not need
  P4). P2 only for the parent relationship; without P2 the span works as a root.
- **Files:**
  - `src/telemetry/process.rs`:
    - `ProcessSpan::start(t: &Telemetry, env: &Envelope, registered_mode: bool, parent: Option<&Span>) ->
      ProcessSpan`. It holds `Span` (`Span::none()` when inactive) and `Instant`.
    - `record_sender(&str)`.
    - `finish(self, metrics: &RuntimeMetrics, outcome: Outcome)`, where `Outcome` is `Accepted | Duplicate |
      Rejected(&MacpError)`.
    - `record_session_attrs(span: &Span, session: &Session)`: `macp.mode.version`, `macp.configuration.version` and
      `macp.policy.id` from `session.policy_definition.policy_id` (`crates/macp-core/src/policy/mod.rs:27`), falling
      back to `session.policy_version` when it is non-empty.
  - `src/server.rs`:
    - `send` (`:1134-1178`): create the span at entry, once the envelope exists. Wrap the existing `result` async
      block in `.instrument(span.clone())`. Call `record_sender` after `authenticate_send_request` succeeds: the
      **authenticated** sender only, never the self-asserted `env.sender`. Call `finish` before building the Ack.
    - `stream_session` (`:1503-1516`): capture `Span::current()` (the SERVER span) and pass it into
      `build_stream_session_stream` (`:607`).
    - `process_stream_request` (`:399-516`): for frames carrying an envelope, create the span with that explicit
      parent, instrument the body and finish it on every return path.
    - The stream path today discards `ProcessResult`: `self.runtime.process(&envelope, max_open).await
      .map_err(rejected)?; Ok(vec![])` (`:510-514`), so `duplicate` never reaches the caller. Change it to bind the
      `ProcessResult` and pass `duplicate` to `finish` (the private helper's return type changes to carry it; the
      wire behaviour, an empty reply for both accepted and duplicate, is unchanged).
    - `StreamFailure::Terminal` (`:1022`) gains the originating `MacpError` (for example
      `Terminal { status, err: Option<MacpError> }`), so `error.type` is the registry code. It is a private enum,
      so this is not an API change.
    - **Stream not-open refusals.** `:467-476` builds `Terminal(Self::coded_status(MacpError::InvalidEnvelope,
      "SESSION_NOT_OPEN"))`: the carried error is `InvalidEnvelope`, and `SESSION_NOT_OPEN` is only the status
      message. `error.type` records **`INVALID_ENVELOPE`**, the code of the `MacpError` the runtime actually
      carries (and already sends as `macp-error-code`), never the message text. This differs from the unary `Send`
      path, where the same situation yields `SESSION_NOT_OPEN`; the plan records the mismatch in OQ10 and does not
      change wire behaviour here.
  - `src/runtime.rs`: in `process_session_start` after `validated.accept` (`:697`), and in `process_message` after
    obtaining the session (`:1014`), call `record_session_attrs(&Span::current(), session)` (cfg-gated, no-op
    inactive).
  - `src/metrics.rs`:
    - `record_envelope(mode: &str, message_type: &str, outcome: &str, seconds: f64)`.
    - An internal `LabelledCounter` and `LabelledHistogram` keyed by `(mode, message_type, outcome)`.
    - **Label normalization (spec #239), applied by the caller before recording, for both the OTel instruments and
      the Prometheus families:**
      - mode label = the mode if registered, `_OTHER` if non-empty and unregistered, absent if empty;
      - message-type label = the type if it is supported, else `_OTHER`. "Supported" means listed in the
        registered mode's `ModeDescriptor.message_types` (`mode_registry.rs:155`), **or `Signal`/`Progress` in
        any mode**. A session-scoped `Progress` passes the shape check (`src/server.rs:132-141`), but no built-in
        mode lists it in `message_types` (`crates/macp-modes/src/mode/mod.rs:198-263`), so without this rule every
        session `Progress` would be recorded as `_OTHER`. Every message type under an `_OTHER` mode is still
        `_OTHER`.
        `descriptor()` clones, so cache the per-mode type set in the recorder and invalidate it on registry change
        (the registry already emits change events for `WatchModeRegistry`).
      - This replaces the earlier `_overflow` fold as the cardinality control: with it, label sets are bounded by
        registered modes × their declared types × 3 outcomes. A hard backstop cap stays (2,000 label sets, beyond
        which new sets fold to `("_OTHER","_OTHER",outcome)`), because extension modes register at runtime up to
        `MAX_MODE_CARDINALITY = 1000`. There is no separate `_overflow` token any more.
    - `semconv_prometheus_lines(&mut String)`.
    - When the `otel` feature is compiled, an optional `OnceLock<Instruments>` set from `Telemetry` (counter
      `u64`, histogram `f64` with explicit boundaries).
  - `src/main.rs` (`:546-556`): append `semconv_prometheus_lines` to the `/metrics` body after the existing lines.
  - `src/telemetry/init.rs`: build `SdkMeterProvider` with `PeriodicReader` + OTLP `MetricExporter` when metrics
    export is active, and create the instruments with the semconv names, units and descriptions (the `brief`).
- **Approach:**
  - **Span:**
    - `info_span!(target: TARGET, parent: <explicit|current>, "process", otel.name = <name>, otel.kind =
      "internal", macp.operation.name = "process", macp.message.id, macp.message.type, macp.version,
      macp.session.id = <if non-empty>, macp.mode.id = <if non-empty>, macp.sender.id = Empty,
      macp.mode.version = Empty, macp.configuration.version = Empty, macp.policy.id = Empty,
      macp.envelope.outcome = Empty, error.type = Empty, otel.status_code = Empty, otel.status_description = Empty)`.
    - `<name>` is `process {mode}` when `mode` is non-empty **and registered**
      (`runtime.mode_registry().get_mode(mode).is_some()`), `process _OTHER` when `mode` is non-empty but not
      registered, and the bare `process` only when `mode` is empty (Signal, Progress). Confirmed by spec #239
      (Divergence 3). The `macp.mode.id` attribute records the mode **as received** (truncated only).
    - Every client-supplied string attribute is truncated to 256 bytes on a char boundary.
  - **Outcome:**
    - `ProcessResult.duplicate` → `duplicate`; `Ok` → `accepted`; `Err(e)` → `rejected` with
      `error.type = e.error_code()`.
    - Status stays UNSET, except `MacpError::StorageFailed` (`INTERNAL_ERROR`, `crates/macp-core/src/error.rs:100`):
      `otel.status_code = "error"` and `otel.status_description = "INTERNAL_ERROR"`.
    - `e.to_string()` is **never** recorded.
  - **Unfinished span (panic or cancelled future).** `ProcessSpan` implements `Drop`: if `finish` was never
    called, it records `otel.status_code = "error"`, `otel.status_description = "INTERNAL_ERROR"` and no outcome
    attribute, and records **no** metric (the outcome is unknown). `finish` sets a flag that disarms the `Drop`.
    A client disconnect that drops the `Send` future mid-flight therefore also shows as ERROR; that is accepted,
    because the runtime cannot tell it apart from a panic at that point, and it is rare.
  - **Synthetic implicit accepts get no `process` span.** The handoff synthetic accept (RFC-MACP-0010 §5.1(2),
    `Runtime::synthesize_due_accept`) is runtime-originated: no Envelope is received, so there is no "receipt" for
    `process` to cover, and it is **not** counted in `macp.envelope.processed`. The legacy
    `macp_messages_accepted_total` counter **does** count it, because `synthesize_due_accept` calls
    `metrics.record_message_accepted` (`src/runtime.rs:962`, and the sweep path at `:1091`). The two counters
    therefore legitimately differ by the number of synthetic accepts; document that in `docs/deployment.md` so
    nobody reconciles them as a bug. If the synthetic accept resolves the session, the `resolve` span (P4) is
    still emitted, parented on whatever span is current (the triggering `process` span, or none on the sweep).
  - **Signals and ambient Progress:** session and mode are empty, so the name is `process` and there is no
    session attribute.
  - **Metrics:**
    - Recorded in `finish` for every Envelope. Before this, only session-scoped Send rejections were counted
      (`src/server.rs:1163-1168`); the old counters keep their exact semantics.
    - Duration uses `Instant`, which is monotonic and does not depend on the injectable runtime clock.
    - **Prometheus rendering** follows the OTel→Prometheus compatibility rules:
      - `macp_envelope_processed_total{macp_mode_id,macp_message_type,macp_envelope_outcome}`;
      - `macp_envelope_process_duration_seconds_{bucket,sum,count}`;
      - `# HELP`/`# TYPE` lines for each new family;
      - the `macp_mode_id` label is omitted when the mode is empty.
    - **Histogram boundaries (s):** `[0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5,
      10]`, the same for OTLP (`with_boundaries`) and Prometheus.
  - **Rejected:**
    - Creating `process` inside `Runtime::process`. It would miss server-side rejections (shape, auth, rate limit,
      ingress policy), and the spec says "from receipt to the accept/reject decision".
    - Creating it in macp-modes (Divergence 2).
    - Recording `macp.sender.id` from the envelope before auth. That is a self-asserted identity, against the
      CLAUDE.md freeze invariant.
- **Edge cases & failure modes:**
  - An envelope-less `SendRequest` gets no process span (no `message_id`). The SERVER span carries the status.
  - A stream subscribe frame gets no process span.
  - Stream `Inline` refusals carry no envelope, so no process span either.
  - A rejection before authentication has no `macp.sender.id`.
  - Duplicate SessionStart → `duplicate`.
  - `PolicyDenied` reasons are not recorded; only the code is.
  - A client-chosen `mode` or `message_type` cannot blow up metric cardinality: unsupported values are recorded
    as `_OTHER` (spec #239), and the backstop cap folds anything beyond it into `_OTHER` too. The span attribute
    still carries the received value.
  - Truncation never splits UTF-8.
- **Acceptance criteria** (`tests/otel_process.rs`, `#![cfg(feature = "otel")]`, in-process server + in-memory
  span and metric exporters):
  1. **Semconv conformance.** For a scripted Decision session (accepted SessionStart, accepted Proposal, duplicate
     Proposal, rejected Vote by a non-participant, accepted ambient Signal), every exported `process` span:
     - has a name equal to the template rendered with its `macp.mode.id`, or bare for the Signal; an extra
       envelope with an unregistered `mode = "ext.bogus.v1"` yields `process _OTHER` with
       `macp.mode.id = "ext.bogus.v1"`, and its metric point carries `macp.mode.id = _OTHER`,
       `macp.message.type = _OTHER`;
     - has kind `Internal`;
     - carries every `required` attribute of `span.macp.process` from `tests/otel/macp-semconv.json`;
     - carries every `conditionally_required` attribute whose condition holds, and none whose condition fails;
     - uses enum values from the file's `members`;
     - carries **no** `macp.*` attribute outside the group's refs.
  2. **Brief acceptance item.** With client `traceparent` `00-<T>-<S>-01` on `Send`, the `process` span's parent
     is the SERVER span, whose parent is `S`, and all share trace `T`. On `StreamSession`, two envelopes sent on one
     stream both have the stream-open SERVER span as parent.
  3. **Negative propagation (RFC-0006 §2.1 MUST NOT).** A SessionStart whose `extensions` map holds
     `"traceparent" → "00-<T2>-<S2>-01"` with no metadata header produces a span **not** in trace `T2`.
  4. **Rejection.** `error.type = "FORBIDDEN"`, status Unset, and no attribute or event contains the MacpError
     message text (assert on a sentinel string).
  5. **Privacy sentinel.** A Proposal payload containing `SENTINEL-PAYLOAD-7f3c` and a CancelSession reason
     `SENTINEL-REASON-9a51` never appear in any exported span name, attribute value, event or link attribute.
  6. **Metrics.** The `InMemoryMetricExporter` shows exactly `macp.envelope.processed` (Sum, monotonic, unit
     `{envelope}`) and `macp.envelope.process.duration` (Histogram, unit `s`). Name, instrument and unit come from
     the vendored file, and attribute keys are a subset of the file's refs with no identifiers.
  7. **Prometheus** — a **unit test inside `src/metrics.rs`'s `#[cfg(test)] mod tests` (`:261`)**, because
     `assert_valid_prometheus_line` (`:285`) is private to that module. It records traffic for two modes and two
     message types, then asserts:
     - every line of `semconv_prometheus_lines` passes `assert_valid_prometheus_line`;
     - **each new family renders contiguously**: its `# HELP` and `# TYPE` lines, then all of its samples, with no
       other family's line in between. The existing `prometheus_lines` loop iterates per mode and interleaves
       families (`src/metrics.rs:241-258`); the new renderer must iterate per family, then per label set, and the
       test checks it by scanning for a family name reappearing after a different one;
     - the old `macp_messages_*_total` lines are byte-identical to before for the same traffic (the old renderer
       is not touched).
  8. **Unfinished span.** A unit test drops a started `ProcessSpan` without `finish` (and an in-process test
     panics inside an instrumented block under `catch_unwind`): the exported span has status Error with
     description `INTERNAL_ERROR`, no `macp.envelope.outcome`, and no metric point is recorded.
  9. **Stream outcome and refusal code.** On `StreamSession`, a re-sent envelope yields a `process` span with
     outcome `duplicate`, and an envelope for a resolved session yields `error.type = INVALID_ENVELOPE`.
- **Tests:** as above, plus unit tests for truncation, label normalization (`_OTHER`, including a session-scoped
  `Progress` under a registered mode, which keeps `macp.message.type = Progress`), the backstop cap and
  outcome mapping.
- **Docs:**
  - `docs/deployment.md` Monitoring: process spans, attributes (link to the spec doc, without repeating the list),
    privacy defaults, and the new Prometheus families.
  - A **deprecation notice**: the `macp_*_total{mode=}` families are a compatibility rendering until the next minor
    release.

### P4 — Session anchor (in memory), transition spans, session metrics
- **Status:** DONE (2026-10-31). Divergences, each recorded in ASSUMPTIONS.md:
  - The anchor cache is a std `RwLock` with no negative entries, not tokio `RwLock<Option<_>>`.
  - `observe_transition` is synchronous and creates a zero-length span after the state change.
  - `process` span links are added in server.rs, which also covers server-side rejections.
  - The duration argument is in ms and is exported in seconds.
  - Anchors are pruned on every eviction pass.
  - The synthetic-resolve branch has no positive test, since no built-in mode reaches it.
  - The replay-equivalence test sorts `seen_message_ids` and clears the compaction timestamps. Both differ between runs even with telemetry off: HashSet order, and `chrono::Utc::now()` at `macp-storage compaction.rs:23`.
  - The anchor encode/decode helpers landed early and are unused until P5.
- **Delivers:**
  - An anchor taken from the accepted SessionStart's `process` span.
  - Links from every later `process` span and every transition span.
  - `resolve|expire|cancel|suspend|resume {mode}` spans, with `expire` as a root.
  - `macp.session.transitions` and `macp.session.duration`.
  - The "replay identical with otel on/off" guarantee, under test.
- **Depends on:** P3.
- **Files:**
  - `src/telemetry/anchor.rs` (`#[cfg(feature="otel")]`):
    - `AnchorCache`: `tokio::sync::RwLock<HashMap<String, Option<SpanContext>>>`;
    - `get(&sid)`, `put(sid, SpanContext)`, `forget(&sid)`;
    - `encode(SpanContext) -> Vec<u8>` and `decode(&[u8]) -> Option<SpanContext>`, with the format
      `{"v":1,"traceparent":"00-…"}` (P5 persists it; P4 keeps it in memory).
  - `src/telemetry/transition.rs`: `transition_span(t, op, &Session, state_entered, anchor: Option<SpanContext>,
    root: bool) -> Span`.
  - `src/runtime.rs` (the `telemetry` field and `with_telemetry` already exist from P1):
    - A private `async fn observe_transition(&self, op: TransitionOp, session: &Session, now_ms: i64)` replaces
      each of the seven `record_session_{resolved,expired,cancelled,suspended,resumed}` call sites (`:581`, `:1117`,
      `:1390`, `:1447`, `:1543`, `:1576`, `:1741`). It:
      - calls the existing per-mode counter (old Prometheus names unchanged);
      - calls `metrics.record_transition(mode, op)`;
      - for terminal ops, calls `metrics.record_session_duration(mode, state, (now_ms - session.started_at_unix_ms)
        .max(0) as f64 / 1000.0)`;
      - creates the transition span, named `{op} {mode}`, or `{op} _OTHER` if the session's mode is no longer
        registered (an extension mode unregistered mid-session; spec #239 applies the rule to every op templated
        on mode). Metric labels use the same `_OTHER` normalization as P3.
      - `resolve` also gets `macp.commitment.hash` via
        `macp_core::commitment_hash::commitment_hash_of_wire(session.resolution)`, gated on **what resolved the
        session, not on whether the bytes happen to decode**: only when the resolving message's `message_type` is
        `Commitment` **and** the session's mode is one of the five standards-track modes
        (`macp_modes::mode::standard_mode_names()`, `crates/macp-modes/src/mode/mod.rs:177`). Passthrough's JSON
        resolution (S-e) and every `ext.*` mode are skipped, even if their bytes would parse as a
        `CommitmentPayload`. `observe_transition` takes the resolving `message_type` as an argument for this.
    - The span is created and entered around the append+mutate block where that is simple. Otherwise it is created
      right after the state change: the span measures the transition, but its existence matters more than its
      duration.
    - `process_session_start` after the commit point (`:779`): when active, `anchors.put(sid,
      Span::current().context().span().span_context())`, only if the context is valid.
    - `process_message` after obtaining the session: link the anchor onto `Span::current()`. Also add a
      `pub(crate) async fn trace_anchor(&self, sid) -> Option<SpanContext>` that `ProcessSpan::finish` uses to link
      server-side rejections and duplicate SessionStarts of a known session.
    - **`trace_anchor` is bounded:** it first checks `registry.get_shared(sid)`; if the session is not resident it
      returns `None` without touching the cache or storage. Server-side rejections (auth, rate limit, shape) can
      carry any client-chosen `session_id`, so without this guard each rejected envelope would add a negative
      cache entry and, after P5, a storage read: unbounded growth and a cheap amplification. Only resident
      sessions get entries, and those are removed with the session.
    - `evict_stale_sessions` (`:1944`) and `gc_disk_sessions` (`:1885`): `anchors.forget(id)`.
  - `src/metrics.rs`:
    - `record_transition(mode, op)` → `macp_session_transitions_total{macp_mode_id,macp_operation_name}`;
    - `record_session_duration(mode, state, s)` → `macp_session_duration_seconds_*{macp_mode_id,macp_session_state}`,
      with boundaries `[1, 5, 15, 30, 60, 300, 900, 1800, 3600, 7200, 21600, 86400]`;
    - the matching OTel instruments.
- **Approach:**
  - **Expire is always a root** (`parent: None`, then `add_link(anchor)`), at all three sites. That includes the
    lazy `Precheck::Expired` path inside a `process` span (`:1036-1045`), `get_session_checked` from a read RPC,
    and the resume force-expire.
    - Rationale: the spec defines `expire` as having "no request behind it". The session expired at its deadline,
      not because of the request that observed it.
    - The observing `process` span already records `error.type = SESSION_NOT_OPEN`.
  - **Resolve** is a child of the `process` span of the Commitment (or whichever message resolved it).
  - **Cancel/suspend/resume** are children of their RPC's SERVER span. These RPCs carry no Envelope, so there is
    no `process` span.
  - **Links:** every transition span and every non-anchor `process` span of a session with a known anchor gets
    exactly one link to it. The anchor's own span gets none.
  - **Session duration** is measured from `session.started_at_unix_ms`, the RFC-0003 timeline the runtime already
    binds, on the injectable clock. It is deterministic under `ManualClock`. A negative value (client clock skew)
    clamps to 0.
  - **Rejected:**
    - An anchor field on `Session`/`PersistedSession`. It would enter checkpoint log entries and replay
      (Divergence 1), and it would also break Seam's sealed record equality.
    - Making `expire` a child of the observing request. That contradicts the spec.
    - A `SessionLifecycleEvent` subscriber that emits the spans. It runs on another task, after the fact, and loses
      the parent context.
- **Edge cases & failure modes:**
  - A session started while telemetry was inactive, or before restart (until P5), has no anchor, so no link. Never
    an error.
  - A sampled-out anchor span is still a valid `SpanContext` (flags `00`). Store and link it anyway, because it is
    harmless.
  - `cleanup_expired_sessions` runs with no current span, which is already a root.
  - A duplicate SessionStart links to the anchor of the session it duplicates.
  - Concurrency: anchor `put` happens under the session mutex before the guard drops (`:815`), so any later
    message's link sees it.
  - Cache growth is bounded by the resident-session set (eviction and GC remove entries; non-resident ids are
    never inserted). Each entry is about 100 bytes.
  - A unit test sends 10,000 rejected envelopes (unauthorized sender) with distinct random `session_id`s and
    asserts the anchor cache length stays 0 and, after P5, `load_trace_anchor` is never called (counting mock
    backend).
  - Neither the anchor nor anything trace-related is written to `LogEntry`, `PersistedSession`, the Envelope or
    `Session`.
- **Acceptance criteria:**
  1. **Transition conformance** (`tests/otel_transitions.rs`): a Decision session resolved by Commitment, a session
     cancelled, a session suspended then resumed, and a session that TTL-expires under `ManualClock` via
     `cleanup_expired_sessions`. Each transition span:
     - matches its semconv group (name, kind `Internal`, required attributes);
     - has `macp.session.state` equal to the entered state (`SESSION_STATE_RESOLVED|CANCELLED|SUSPENDED|OPEN|
       EXPIRED`);
     - carries exactly one link whose span id equals the SessionStart `process` span's span id.
  2. **Expire root.** The expire span has no parent, on the sweep path **and** on the lazy path (a message sent
     after the deadline): its parent span id is invalid and its trace id differs from the observing `process`
     span's.
  3. **Resolve.** The `resolve` span's parent is the Commitment's `process` span. `macp.commitment.hash` equals
     `commitment_hash_of_wire(payload)` and matches `sha256:[0-9a-f]{64}`.
  4. **Metrics.** `macp.session.transitions` and `macp.session.duration` match the vendored file's name, instrument
     and unit. The duration for the TTL-expired session equals `(now_ms - session.started_at_unix_ms) / 1000`
     exactly, where `now_ms` is the `ManualClock` value at which `cleanup_expired_sessions` observed the expiry.
     It is **not** `ttl_ms/1000`: expiry fires on `now_ms > ttl_expiry` (`crates/macp-core/src/session.rs` ~`:387`)
     and the sweep observes it later, so the test advances the clock past the deadline by a known amount (e.g.
     `ttl_ms + 2_500`) and asserts that value. A resolve case checks the same formula at the Commitment's time.
     A Passthrough or `ext.*` resolve carries no `macp.commitment.hash`, even with a decodable payload.
  5. **Replay is identical with otel on and off** (`tests/otel_replay_equivalence.rs`). Run the same scripted
     session, with `ManualClock` and a `FileBackend` in two tempdirs, once with `Telemetry::disabled()` and once
     active with an in-memory exporter, and check:
     - `log.jsonl` is byte-identical;
     - `session.json` is byte-identical;
     - `replay::replay_session` of each log yields `PersistedSession`s whose `serde_json` bytes are equal;
     - the only extra artifact is in P5 (`anchor.json`), and nothing under the session directory names
       "trace"/"traceparent" except that file.
  6. **Inventory pin.** A unit test greps `src/runtime.rs` (`include_str!`) and asserts that no
     `metrics.record_session_(resolved|expired|cancelled|suspended|resumed)` call appears outside
     `observe_transition`. A future transition site cannot then skip its span.
- **Tests:** as above.
- **Docs:**
  - `docs/deployment.md` Monitoring: transition spans, the anchor and its links ("search by the anchor trace to
    find every trace of a session"), expire as a root, and the session metrics.
  - `docs/embedding.md`: one paragraph saying the library crates create no spans, so stepping and replay are
    unaffected (the spec's "Implementations that embed" section), and that embedders may wrap `step` in their own
    debug spans.

### P5 — Durable anchor across restart
- **Status:** DONE (2026-10-31). Divergences, each recorded in ASSUMPTIONS.md:
  - There is no renamed `macp-storage =0.11.0` dev-dependency for the rollback test, because it would break the deps-isolation lockstep check. Instead, the pre-P5 storage sources were confirmed byte-identical to published 0.11.0, and tests simulate 0.11 deletes. The CI-worktree fallback was not done.
  - Redis EX is disk retention + `MAX_TTL_MS` + 1h, because the trait doesn't carry the session's ttl_ms. It is never shorter than the planned formula.
  - `observe_transition` is async so it can lazy-load the anchor after a restart.
  - The stored anchor is written only after `save_session` succeeds, which closes a race between the startup sweep and a new SessionStart.
  - The RocksDB sweep runs in `spawn_blocking` and deletes in batches of 1,000. `RocksDbBackend.db` is now a private `Arc<DB>`.
  - The sweep runs only when disk retention is above 0 and storage is not memory-only.
- **Delivers:** the anchor survives a restart. After recovery, later spans and the `expire` root still link to the
  pre-restart SessionStart span.
- **Depends on:** P4. It is independently droppable: without it, links simply stop after a restart.
- **Files:**
  - `crates/macp-storage/src/storage/mod.rs`, new **defaulted** trait methods (additive for semver):
    - `async fn store_trace_anchor(&self, session_id: &str, anchor: &[u8]) -> io::Result<()> { Ok(()) }`;
    - `async fn load_trace_anchor(&self, session_id: &str) -> io::Result<Option<Vec<u8>>> { Ok(None) }`.

    Storage treats the bytes as opaque.
  - `file.rs`: `sessions/<id>/anchor.json` through the existing `atomic_write` (`:70`), guarded by `Self::check`.
    `delete_session` already removes the whole directory (`:203-210`), so there is no orphan in any version.
  - `rocksdb.rs`: **no new column family.** The anchor lives in the existing `receipts` CF under the key
    `{session_id}\x00anchor`.
    - Precondition, verified at 0.11.0 and at `main`: the `receipts` CF is only ever accessed by point operations,
      `get_cf` in `load_receipt` (`:170-178`), `put_cf` in `store_receipt_if_absent` (`:180-198`) and
      `batch.delete_cf(&receipts_cf, session_id)` in `delete_session` (`:150-155`). Every iterator in the file is
      on the `logs` or `sessions` CF (`:70`, `:159`, `:210`, `:247`, `:282`). A 0.11 binary therefore never sees
      the extra key.
    - The key cannot collide with a receipt key: session ids never contain NUL (`is_storage_safe_session_id`
      refuses `\0`, `storage/mod.rs:28-33`, and `validate_session_id_for_acceptance` admits only UUID or
      `[A-Za-z0-9_-]` tokens).
    - The new `delete_session` adds `batch.delete_cf(&receipts_cf, anchor_key(session_id))` to the same batch.
    - Add a comment on `CF_RECEIPTS` stating the point-ops-only invariant and why, and a unit test that fails if
      a receipts-CF iterator is introduced without handling `\x00anchor` keys (a grep over the file via
      `include_str!`, like the P4 inventory pin).
    - If the precondition ever stops holding, the fallback is to make RocksDB a documented no-op (default trait
      method), never a new CF.
  - `redis_backend.rs`: key `{prefix}:anchor:{id}`, added to the `delete_session` pipeline (`:103-111`).
  - `memory.rs`: the default no-op.
  - `src/runtime.rs`:
    - **The anchor write is spawned off the SessionStart path.** After the commit point, the in-memory cache entry
      is set synchronously under the session guard (P4), then `tokio::spawn` runs `store_trace_anchor` with a
      cloned `Arc<dyn StorageBackend>`. SessionStart never awaits it. A failure logs at `debug` (non-telemetry
      target) and never affects the start.
    - **Cost, stated:** one spawned task and one small write (~60 bytes plus backend overhead: a file
      create+rename+fsync via `atomic_write`, a RocksDB `put_cf`, or a Redis `SET`) per accepted SessionStart,
      only when telemetry is active. Zero cost when inactive. The latency added to SessionStart is the spawn only
      (sub-microsecond). The durability cost is that a crash between the ack and the spawned write loses that
      session's anchor; the session itself is unaffected and its post-restart spans simply have no link. A GC
      racing the spawned write could recreate an `anchor.json`/key for a just-deleted session; this needs a
      session to be terminal and past retention within the same few milliseconds as its own start, which cannot
      happen with retention ≥ 1 s, so it is not handled beyond this note.
    - `trace_anchor(sid)`: only for resident sessions (P4). On a cache miss for a resident session,
      `storage.load_trace_anchor` and cache the result, **including `None`** (negative caching avoids a storage
      read per envelope for pre-telemetry sessions; it is bounded because only resident sessions are cached).
- **Approach:**
  - Mirror the receipts store precedent (`storage/mod.rs:62-88`), but with no first-writer-wins: there is one
    writer, exactly once per session.
  - Store only `traceparent`, never `tracestate`. The client's `tracestate` is not ours to persist, and links do
    not need it.
  - **Rejected:**
    - A new RocksDB CF `trace_anchors`. A 0.11 binary cannot open a DB with an unknown CF, so it would add a
      rollback hazard for an optional, spec-non-required feature.
    - A sidecar written by the root crate directly. It only works for `FileBackend`.
    - A field in `session.json`. Same objection as Divergence 1, since `PersistedSession` doubles as the checkpoint
      body.
    - Awaiting the write before returning the SessionStart ack. It puts telemetry storage latency on the
      acceptance path for no correctness gain.
- **Rollback to 0.11 (all three backends keep working):**
  - **File:** 0.11 ignores `anchor.json`. `list_session_ids` only lists directories (`file.rs:212-226`),
    `load_session` reads `session.json`, and `delete_session` removes the whole directory. No orphan.
  - **RocksDB:** opens normally (no new CF). 0.11's `delete_session` deletes only the `{id}` receipts key, so a
    session garbage-collected **by 0.11** leaves its `{id}\x00anchor` key behind (about 100 bytes each, never
    read). When 0.12 runs again, a new **defaulted** trait method
    `StorageBackend::sweep_orphan_trace_anchors(&self) -> io::Result<usize>` (default `Ok(0)`; RocksDB override)
    removes them. It scans the receipts CF, which 0.12 may do because it knows the key layout, and deletes every
    `{id}\x00anchor` key whose session is absent from the sessions CF, returning the count. It runs once at startup,
    after recovery and off the serving path, only when `MACP_SESSION_DISK_RETENTION_SECS > 0` (otherwise nothing
    is ever GC'd, so nothing is orphaned). Cost O(receipts CF size). If 0.11 ran with retention > 0 and 0.12 then
    runs with retention 0, the sweep never runs and those keys stay; they are harmless (never read, ~100 bytes).
  - **Redis:** 0.11's `delete_session` pipeline deletes the session, log and receipt keys and the index member,
    not `:anchor:`. Handling: when `MACP_SESSION_DISK_RETENTION_SECS > 0`, `store_trace_anchor` writes the key with
    `SET .. EX <disk_retention_secs + ttl_ms/1000 + 3600>`, so an orphaned key expires by itself whichever binary
    did the GC. The expiry is not a bound on the session's lifetime: see edge cases. With retention 0 (the default), sessions are never
    GC'd, so no orphan arises and the key has no expiry. The retention value is passed into the backend at
    construction (a new builder method, additive).
  - Document all three in the "Upgrading" note (P7).
- **Edge cases & failure modes:**
  - A corrupt or unknown-version anchor decodes to `None` (no link), logged at debug.
  - A path-like session id is refused by `FileBackend::check`, as for receipts.
  - `MACP_MEMORY_ONLY=1` gives an in-memory anchor only.
  - Disk GC deletes the anchor with the session on all three backends (0.12 and later).
  - Recovery loads no anchors eagerly; they are loaded on first use, for resident sessions only.
  - **Redis expiry vs. long suspensions.** The EX is computed at SessionStart, so a session suspended for longer
    than the slack (retention + TTL + 1 h) can lose its anchor key while still live. The cost is only a lost span
    link on its later spans, never correctness. Refreshing the expiry on suspend/resume was rejected (decided by
    Opus): it would add a write path to two un-rate-limited RPCs for a non-essential link.
- **Acceptance criteria:**
  - A storage contract test, shared like `receipt_contract` (`storage/mod.rs:109-150`), runs for file, memory
    (no-op semantics), rocksdb (features job) and redis (features job, live service). It covers store, load,
    delete-removes-anchor, path-like id refused, and (rocksdb) that `load_receipt(id)` is still `None` after
    `store_trace_anchor(id, ..)` and vice versa.
  - **Rollback test** (`tests/otel_anchor_rollback.rs`): add the published 0.11 storage crate as a renamed
    dev-dependency (`macp_storage_011 = { package = "macp-storage", version = "=0.11.0" }`, published on
    crates.io), write a session plus anchor with the current backend, then open the same directory (and, in the
    features job, the same RocksDB path) with 0.11's `FileBackend`/`RocksDbBackend` and assert that
    `list_session_ids`, `load_all_sessions` and `load_receipt` return exactly what they return for the same data
    without an anchor. UNCONFIRMED: two `macp-storage` versions in one build may hit a `links` conflict through
    `macp-proto`/`librocksdb-sys` if their requirements do not unify. Fallback: a CI step that checks out tag
    `macp-storage-v0.11.0` in a worktree and runs a tiny example binary against a fixture directory produced by
    the current code.
  - `tests/otel_anchor_restart.rs`: start a session with telemetry active on a `FileBackend` tempdir, wait for
    the spawned write (poll `load_trace_anchor` under a bounded timeout), then drop the runtime. Build a new
    runtime over the same dir (`recovery::recover_sessions`) and let the TTL lapse under `ManualClock`. The
    `expire` span links to the **pre-restart** SessionStart span id.
  - A Redis test (features job) asserts the anchor key has a TTL when retention > 0 and none when it is 0.
  - With telemetry inactive, no `anchor.json` is ever created (assert the directory listing equals exactly
    `log.jsonl` + `session.json`).
- **Tests:** as above.
- **Docs:**
  - `docs/deployment.md` "Durability matrix" (`:340`) gains a trace-anchor row (file/rocksdb/redis durable, memory
    not, best-effort: a crash right after SessionStart can lose it).
  - The rollback notes above.

### P6 — Real-binary check, image decision, measurement
- **Status:** DONE (2026-10-31). The decision rule PASSED and was applied: docker.yml and the ci.yml docker-build gate pass `CARGO_FEATURES=otel`. Divergences, each recorded in ASSUMPTIONS.md:
  - Measured on an Apple M4 Pro: arm64 native, amd64 under emulation (only deltas are meaningful there). No CI runner was measured.
  - The memory rule uses the larger of the `docker stats` and process VmRSS deltas.
  - Case 1 ends with CancelSession so the duration metric records.
  - The SIGINT bound includes `MACP_SHUTDOWN_DRAIN_SECS=1`.
  - The zero-egress case also watches `[::1]:4317` and `127.0.0.1:4318`.
  - The binary tests fail fast if the child exits before SERVING.
- **Delivers:**
  - End-to-end proof through the real binary and exporter.
  - The measured cost of `--features otel` in the GHCR image.
  - The build decision, applied.
- **Depends on:** P1-P4 (P5 optional).
- **Files:**
  - `tests/otel_binary.rs` (`#![cfg(feature = "otel")]`):
    - spawns `CARGO_BIN_EXE_macp-runtime` with `MACP_ALLOW_INSECURE=1 MACP_MEMORY_ONLY=1`;
    - runs a **fake OTLP collector**: a tonic server implementing
      `opentelemetry_proto::tonic::collector::trace::v1::trace_service_server` and the metrics service, as a
      dev-dependency `opentelemetry-proto` with `gen-tonic`;
    - runs a tonic client against the binary.
  - `Dockerfile`: `ARG CARGO_FEATURES=""` and `RUN cargo build --release ${CARGO_FEATURES:+--features
    "$CARGO_FEATURES"}`.
  - `.github/workflows/docker.yml` and the `docker-build` gate (`ci.yml:872-890`): pass
    `build-args: CARGO_FEATURES=otel` **only if** the decision rule passes.
  - `scripts/measure-otel-image.sh` (new, dev-only, excluded from the crate by `Cargo.toml:13`): builds both images,
    then prints image size, binary size, median startup-to-SERVING over 20 runs, and idle RSS.
- **Approach:**
  - **Binary test cases:**
    1. With `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:<collector>` and `OTEL_SERVICE_NAME` unset, the
       collector receives spans whose resource has `service.name=macp-runtime` and `service.version=<pkg>`, a
       `process macp.mode.decision.v1` span and the SERVER span. It also receives the four metrics within
       `OTEL_METRIC_EXPORT_INTERVAL=500`.
    2. With **no OTEL vars**, a TCP listener bound on literal `127.0.0.1:4317` sees **zero** connections during a
       scripted session and shutdown. This catches any hardcoded default. If 4317 is already taken on the host,
       the case is skipped with a printed notice (never a false pass). A listener on a random port proves nothing,
       since no code would dial it, so that case is dropped.
    3. On SIGINT, exported spans are flushed within the 5 s bound (the collector has the final spans).
  - **Measurement protocol** (on one host, record the numbers in the PR and in PROGRESS):
    1. Build `docker buildx build --load -t macp:plain .` and `--build-arg CARGO_FEATURES=otel -t macp:otel .` on
       linux/amd64. Also arm64 if buildx and QEMU are available; note when they are not.
    2. Sizes: `docker image inspect -f '{{.Size}}'` and the binary's `stat -c %s`.
    3. Startup: 20 runs of `docker run --rm -e MACP_ALLOW_INSECURE=1 -e MACP_MEMORY_ONLY=1`. Time from container
       start to the first successful `grpc.health.v1` Check (from the host, polling every 10 ms). Report the median
       and p90, with no OTEL vars on both images, plus the otel image with an endpoint pointing at a blackhole.
    4. Idle RSS after 10 s from `docker stats --no-stream`.
    5. **Behaviour equivalence:** a scripted session (start, proposal, commitment) against each image with no OTEL
       vars. The stdout log lines are identical after stripping timestamps, and so are the gRPC responses after
       dropping `accepted_at_unix_ms`.
  - **Decision rule.** Build the published image with `--features otel` iff all of the following hold, else keep
    the plain image and document `--build-arg CARGO_FEATURES=otel` for self-builders:
    - image size Δ ≤ max(8 MiB, 10%);
    - median startup Δ ≤ 50 ms;
    - idle RSS Δ ≤ 5 MiB;
    - step 5 shows no difference.
- **Edge cases & failure modes:**
  - The fake collector must answer `Export` with OK, or the batch exporter retries and the test flakes. Use bounded
    waits (`tokio::time::timeout`), never sleeps, per the repo's test style.
  - Startup measurement is noisy, hence the median of 20 and the same host for both images.
- **Acceptance criteria:**
  - `tests/otel_binary.rs` passes in the CI features job.
  - The measurements are recorded in `plans/otel-spans-PROGRESS.md` with the rule's verdict.
  - If adopted, `docker.yml` and the gate build with otel, and `docs/deployment.md` "Published image tags" says
    so. If not, the doc names the build-arg.
- **Tests:** as above.
- **Docs:** `docs/deployment.md` "Container deployment": whether the image includes otel, and the zero-egress
  guarantee without `OTEL_*`.

### P7 — Documentation sweep and release notes
- **Status:** TODO
- **Delivers:** consistent docs across files, plus the semver and changelog posture.
- **Depends on:** P1-P6.
- **Files:**
  - `CLAUDE.md`:
    - the env-var table gets the `OTEL_*` rows;
    - the Cargo features table's `otel` row: "OpenTelemetry traces + metrics (OTLP/gRPC), active only when an OTLP
      endpoint variable is set";
    - Key files gets `src/telemetry/`;
    - Integration tests gets the otel tests.
  - `docs/deployment.md`: the "Upgrading to 0.12.0" (or the computed next version) section:
    1. new `/metrics` families;
    2. the deprecation of `macp_*_total{mode=}` with the removal version;
    3. an `otel` build no longer exports by default;
    4. the new `anchor.json`, RocksDB `receipts`-CF key and Redis `:anchor:` key;
    5. the rollback notes (no hazard; per-backend orphan handling after a 0.11 GC);
    6. synthetic implicit accepts are counted by `macp_messages_accepted_total` but not by
       `macp_envelope_processed_total`;
    7. an unsupported `OTEL_EXPORTER_OTLP_PROTOCOL` or unreadable `OTEL_EXPORTER_OTLP_CERTIFICATE` logs an error
       and leaves telemetry off; it never stops the runtime.
  - `docs/testing.md`: the otel tests and the features-job step.
  - `docs/stability.md`: the semconv names follow `macp-semconv/0.1` (experimental), and the Prometheus
    compatibility names are deprecated.
  - `MIGRATING.md`: **no entry required.**
    - No breaking change lands in macp-core, macp-modes, macp-pb or macp-policy.
    - macp-storage only gains defaulted trait methods.
    - Add an entry only if `cargo semver-checks` reports a break (procedure below).
  - `README.md`: one line under features, if it lists them.
- **Approach:**
  - **Semver check:**
    `RUSTC_WRAPPER="" cargo semver-checks check-release --workspace --baseline-version 0.11.0 2>&1 | grep -E
    '^--- failure |^  (field|struct) '`.
  - **Expected result:** no failures. The additions are:
    - `macp_runtime::telemetry` (new module, mostly `cfg(feature="otel")`);
    - `Runtime::with_telemetry`;
    - new `RuntimeMetrics` methods;
    - three defaulted `StorageBackend` methods (`store_trace_anchor`, `load_trace_anchor`,
      `sweep_orphan_trace_anchors`).
  - `trait_method_added` fires only for methods without a default. Verify that, and that no public exhaustive
    struct gained a field: `ProcessResult`, `MetricsSnapshot` and `ModeMetrics` are untouched.
- **Acceptance criteria:**
  - The semver-check output is recorded, with zero failures (or each failure documented in MIGRATING.md).
  - `grep -rn OTEL_ docs/deployment.md CLAUDE.md` lists every variable `src/telemetry/init.rs` reads.
  - Every file path cited in the docs exists.
- **Tests:** none new.
- **Docs:** this phase.

### PR strategy

Use **one PR from `feat/otel-spans` with one commit per phase**, like decision-receipts #264. The feature is off by
default, every phase is inert without both the feature and the endpoint variables, and the phases share test
scaffolding. P5 is the only phase touching a published library crate, and it is additive. If review load argues
for a split, cut after P3 (bootstrap + SERVER + process spans and metrics) and after P5 (anchor + transitions +
durability), and land P6-P7 with the second PR.

## Long-term posture

- **One-way doors:**
  - **The on-disk anchor format and location.** These are `sessions/<id>/anchor.json`, the RocksDB `receipts`
    CF key `{id}\x00anchor`, and Redis `:anchor:` (P5).
    - Readers tolerate absence and corruption, so changing the format later costs only lost links for old
      sessions.
    - **No rollback hazard:** no new RocksDB CF, and 0.11 ignores all three locations (P5 rollback test). The only
      rollback residue is orphaned anchors for sessions that 0.11 garbage-collects, bounded and handled per
      backend in P5.
    - The RocksDB key choice commits future code to keeping the `receipts` CF point-access-only, or to skipping
      `\x00anchor` keys in any scan it adds (pinned by a test in P5).
  - **Three `StorageBackend` trait method names** (`store_trace_anchor`, `load_trace_anchor`,
    `sweep_orphan_trace_anchors`) in the published `macp-storage`. Removing them later is a breaking
    change.
  - **Prometheus semconv family names and label names** (`macp_envelope_processed_total{macp_mode_id,…}`).
    Dashboards will bind to them. They follow the OTel→Prometheus rules, so they are the names a collector would
    produce anyway.
  - **Removing the old Prometheus names** in the following minor is a deliberate, announced break for operators.
- **Reversible:**
  - span placement, levels and filters;
  - the activation rule;
  - histogram boundaries (backends tolerate boundary changes on new series);
  - the image build flag;
  - truncation length;
  - the registered-mode span-name rule.
- **Future additive work the spec already anticipates:**
  - the anchor on `SessionMetadata`;
  - per-message context on `StreamSession`;
  - library debug spans for embedders (only if Seam asks);
  - opt-in hashed `macp.sender.id`;
  - opt-in `macp.<impl>.context_id`.

## Enterprise concerns

- **Privacy and SIEM:**
  - The otel layer exports **spans only**, from our target only. No `tracing` event leaves (the existing
    `reason`-bearing logs at `src/runtime.rs:1391,1448,1544` stay local).
  - Error messages are never recorded, only codes.
  - A sentinel test (P3 #5) guards this.
  - `macp.sender.id` is recorded raw (recommended attribute). The hashing MAY is deferred to an opt-in variable
    (OQ5).
- **Identity integrity:** `macp.sender.id` is the authenticated identity only, never the self-asserted
  `Envelope.sender`.
- **Cardinality and cost:**
  - span names are bounded by registered modes (Divergence 3);
  - unsupported modes and message types are recorded as `_OTHER` on metrics (spec #239), with a 2,000 label-set
    backstop that also folds to `_OTHER`;
  - attribute strings are truncated (256 B);
  - no identifiers appear on metrics.
- **Egress:**
  - Zero network egress without `OTEL_*` (tested in P6).
  - TLS to the collector uses native roots, loaded explicitly with `with_enabled_roots()` (`ca-certificates` is
    already in the image), plus an optional private CA through `OTEL_EXPORTER_OTLP_CERTIFICATE`, which this
    runtime reads itself because otlp 0.32 does not. Client certificates (mTLS to the collector) are unsupported.
  - Headers or auth for SaaS collectors come through `OTEL_EXPORTER_OTLP_HEADERS`, which the exporter reads.
- **Availability:**
  - A dead collector never blocks acceptance: export is async and batch.
  - Shutdown is bounded at 5 s.
  - Telemetry storage writes (P5) are best-effort, spawned off the SessionStart path, never awaited.
  - Telemetry configuration errors never abort startup; they log and leave telemetry inactive.
- **Sampling:** `OTEL_TRACES_SAMPLER` is honoured. Parent-based by default, so a client's sampled-out decision
  propagates.
- **Multi-tenant isolation:**
  - Anchors are per session and never exposed over the wire.
  - `tracestate` is never written by the runtime.
  - Trace context cannot affect acceptance (P4 #5).

## Open questions

The defaults below are what `/implement` builds. Each one is logged as UNCONFIRMED for `/reconcile`.

1. **OQ1, durable anchor storage (decided by the coordinator).** P5 uses no new RocksDB CF: the `receipts` CF
   key `{id}\x00anchor`, valid because that CF is point-access-only (verified). If that ever stops holding, RocksDB
   becomes a documented no-op. The anchor write is spawned. Remaining UNCONFIRMED item: whether the 0.11
   rollback test can depend on `macp-storage =0.11.0` without a `links` conflict (fallback in P5).
2. **OQ2, semconv Prometheus families always on (pending confirmation).**
   - Default: rendered on `/metrics` in every build, because it is the only metrics surface of the default image.
   - With no `MACP_METRICS_ADDR`, nothing changes. With it set, new lines are appended and the old ones are
     untouched.
   - This reads "runs with no OTEL vars exactly as before" as covering egress, files, logs and the gRPC surface,
     with additive metric lines allowed.
3. **OQ3, span name for unregistered modes (RESOLVED, spec #239).** `{op} _OTHER` for every op templated on
   mode; the bare `process` stays reserved for an empty mode (Signal, Progress). `macp.mode.id` records the value
   as received. Metric attributes record an unsupported mode or message type as `_OTHER` (P3 label
   normalization). Divergences 1 (anchor in its own store) and 5 (`process` under the SERVER span) are likewise
   confirmed by the spec session.
4. **OQ4, activation on signal-specific endpoint variables (decided, low risk).** Superset of the brief.
5. **OQ5, hashed sender (deferred).** Default raw. Later, an opt-in `MACP_OTEL_SENDER_ID=hash` would add an
   attribute value change only.
6. **OQ6, OTLP TLS (decided).** Enable `tls-ring` and `tls-roots` on opentelemetry-otlp **and** call
   `with_tls_config(ClientTlsConfig::new().with_enabled_roots())` for https endpoints (the features alone load no
   roots). `OTEL_EXPORTER_OTLP_CERTIFICATE` is implemented by the runtime (UNCONFIRMED that the extra code is
   wanted; the alternative is documenting it as unsupported). Run `cargo audit`
   and regenerate `integration_tests/Cargo.lock`.
7. **OQ7, image build (decided by the measured rule in P6).** No owner input needed unless the numbers are
   borderline.
8. **OQ8, Seam library spans (decided: none).** Confirm with the spec or Seam session that "no library spans" is
   acceptable. The spec makes them conditional, and adding them later is additive.
9. **OQ9, process span on server-side rejections with no session (decided).** It is emitted (outcome `rejected`)
   without `macp.session.id` when `session_id` is empty, and with it when non-empty even if the session is unknown.
   The attribute rule keys on the field, not on whether the session exists.
10. **OQ10, stream not-open code mismatch (pending, out of scope).** A `StreamSession` envelope for a non-open
    session is refused with `MacpError::InvalidEnvelope` and the message `SESSION_NOT_OPEN`
    (`src/server.rs:467-476`), while unary `Send` returns `SESSION_NOT_OPEN` as the code. Telemetry records the
    carried code, `INVALID_ENVELOPE`. Whether the stream path should carry `SessionNotOpen` is a wire-behaviour
    question for a separate issue; this plan does not change it.
11. **OQ11, `SPEC_REV` and #239 (decided).** Pin `0f77753` unconditionally. #239 (`99f3cfa`) is cited only for the
    `_OTHER` rule; pinning it would drag in the parity-contract change (P0).

There is no critical question without a default.

## Plan review

- **Round 1 (fresh Opus): REVISE.** Items A–T, plus coordinator decisions L (`process _OTHER`), M (a bad OTLP
  protocol logs an error and leaves telemetry inactive), N (DEADLINE_EXCEEDED/UNIMPLEMENTED are SERVER-span errors)
  and R (no new RocksDB CF; spawned anchor write; rollback test; Redis orphans).
- **Spec session:** confirmed the `_OTHER` rule (#239, merged as `99f3cfa`) for span names and metric attributes,
  and Divergences 1 (anchor in its own store) and 5 (`process` is a child of the SERVER span).
- **Round 2 (Opus, closure check):** A–T closed. Four further fixes applied: P0 pins `0f77753` unconditionally;
  `Signal`/`Progress` are supported label types in every mode; the Redis EX claim is corrected (a long suspension
  can lose a link); the RocksDB sweep is the named defaulted method `sweep_orphan_trace_anchors`.
- **Verdict: SOUND** after the round-2 fixes, applied without a third round because they are concrete and
  code-cited.
- **What changed as a result:** telemetry wiring moved to P1 so P3 ships alone, the durable anchor adds no RocksDB
  column family and no rollback hazard, metrics fold unsupported modes and types into `_OTHER`, and telemetry
  configuration can never stop the runtime.
