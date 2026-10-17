# Embedding the coordination kernel

This guide is for a system that drives MACP sessions **in-process**, through
the `macp-core` and `macp-modes` crates, with no `macp-runtime` server and no
gRPC. RFC-MACP-0001 §2 counts such a system as a MACP Runtime, so the
acceptance obligations of §7.6 bind it. §7.6 also requires a library to
document every obligation it leaves to its caller, and this page is that
documentation for `macp-modes` 0.9.0.

The server kernel (`src/runtime.rs`) calls the same functions described here,
so an embedder that follows this page and the server cannot drift apart.
Every rule below is pinned by `crates/macp-modes/tests/` (`step_implicit_accept.rs`,
`replay_round_trip.rs`, `coordination_library.rs`) and by the spec conformance
transcripts.

## Crates

| Crate | You use it for |
|-------|----------------|
| `macp-pb` | `Envelope` and the payload messages (prost only, no tonic) |
| `macp-core` | `Session`, `MacpError`, `Clock`, `PolicyDefinition`, `commitment_hash` |
| `macp-modes` | the modes, `ModeRegistry`, `step` (acceptance), `replay` (rebuild) |
| `macp-policy` | `PolicyRegistry` and `DefaultPolicyEvaluator`, if you do not bring your own `PolicyEvaluator` |

Modes evaluate governance through the `macp_core::PolicyEvaluator` trait that
is injected when you construct them: `ModeRegistry::build_default(evaluator)`,
or a mode's own `new(evaluator)`. `macp-modes` never depends on a concrete
evaluator.

## RFC-MACP-0001 §7.6 obligations

| # | Obligation | Discharged by | Caller's job |
|---|------------|---------------|--------------|
| 1 | **Version.** An unsupported `macp_version` is rejected with `UNSUPPORTED_PROTOCOL_VERSION` | `step::check_envelope_shape`, called by `check_preconditions`, `step` and `validate_session_start`. It returns `MacpError::InvalidMacpVersion`, whose `error_code()` is `UNSUPPORTED_PROTOCOL_VERSION` | Nothing, unless you negotiate a version other than `macp_core::MACP_VERSION` |
| 2 | **Session identifier.** A `session_id` that fails RFC-MACP-0004 §11.2 is rejected with `INVALID_SESSION_ID`, and the accepted formats are documented | `validate_session_start` → `session::validate_session_id_for_acceptance`, or your own `StartOptions::with_session_id_validator`. See [Session identifiers](#session-identifiers) | A custom validator may only be **stricter**, and must not cap ids below 128 bytes |
| 3 | **Policy binding.** RFC-MACP-0012 §6.1 applies at every `SessionStart`, including step 5 (a definition bound to another mode is rejected with `INVALID_POLICY_DEFINITION`) | `ValidatedStart::accept` / `accept_session_start` check the definition you pass against the requested version (`UNKNOWN_POLICY_VERSION`) and the session's mode (`INVALID_POLICY_DEFINITION`), and bind it to the session | **Resolve** the definition: look up `ValidatedStart::requested_policy_version()` in your registry (`PolicyRegistry::resolve`). When it is empty, pass your default (the server passes `policy.default`, `macp_policy::defaults::DEFAULT_POLICY_ID`), or `None` for no governance |
| 4 | **Acceptance time.** Every time-dependent rule runs against the recorded acceptance time, never `Envelope.timestamp_unix_ms`, and that time is recorded for replay | Every `step` function takes `now_ms` and dispatches modes through `Mode::on_message_at` at that clock. `validate_session_start` bases the initial TTL deadline on the `SessionStart` envelope's timestamp, the RFC-MACP-0003 §2 exception that item 4 keeps | **Supply** a real clock (see [The clock rule](#the-clock-rule)) and **record** it with each accepted envelope (see [Durable embedders](#durable-embedders)) |
| 5 | **Due synthetic envelopes.** Before evaluating an envelope, append every synthetic envelope a mode says is due (RFC-MACP-0010 §5.1's implicit `HandoffAccept`) | `step` (returned in `StepOutcome::Accepted { synthesized }`), `advance`, or the `next_synthetic` → `commit_synthetic` pair | A durable embedder **appends** each synthetic envelope before the trigger. A timer that calls `advance` is RECOMMENDED but optional: observation at evaluation time is enough |
| 6 | **Duplicates.** A duplicate is reported distinctly from a first acceptance and from every rejection | `check_preconditions` → `Precheck::Duplicate`; `step` → `StepOutcome::Duplicate`. A duplicate changes nothing | **Report** it to the submitter, as `DUPLICATE_MESSAGE` or as a success the submitter can recognise as a replay |

### Duties outside §7.6 that stay with the caller

The library has no transport, storage or identity, so these duties are always
yours:

- **Authentication.** `Envelope.sender` must come from your authenticated
  identity, never from the client's self-assertion (RFC-MACP-0004). The library
  authorizes the sender it is given (`Mode::authorize_sender`), but it cannot
  authenticate them.
- **Session existence.** A second `SessionStart` for a `session_id` that
  already exists is rejected with `SESSION_ALREADY_EXISTS` (RFC-MACP-0001 §8.2),
  whatever its `message_id`. Only you hold the session map.
- **Per-session serialization.** Accept one envelope at a time per session
  (§8.1). `step` takes `&mut Session`, so holding a per-session lock across a
  step is enough.
- **Durability ordering.** Append to your log before you `commit`, so that a
  failed write consumes no dedup slot. See [Durable embedders](#durable-embedders).
- **Expiry bookkeeping.** When a deadline passes, `step` sets the session to
  `Expired` and returns `TtlExpired`. Record that expiry yourself (the server
  writes an internal `TtlExpired` entry) so replay reproduces it.
- **Rate and size limits, signals and lifecycle RPCs.** These are deployment
  policy. Signals (the ambient plane) never touch a `Session`.

## The acceptance API

### `SessionStart`

```rust
use macp_modes::step::{accept_session_start, StartOptions};

let session = accept_session_start(&env, now_ms, mode.as_ref(), policy, &StartOptions::default())?;
```

`accept_session_start(env, now_ms, mode, policy, opts)` performs these steps in
order:

1. envelope shape and version;
2. `message_type == "SessionStart"`;
3. the session-id format;
4. payload decode;
5. the canonical payload contract for standards-track modes (participants,
   `mode_version`, `configuration_version`, a positive `ttl_ms`);
6. `mode_version` against the descriptor;
7. TTL and suspension-cap resolution;
8. the policy binding;
9. `on_session_start`.

The session is built at `CURRENT_SEMANTICS_REV`, with the mode's initial state
applied and the start's dedup slot consumed.

To run your own checks between validation and acceptance, split it into its
two halves, as the server does to check for an existing session and resolve
the policy: `validate_session_start(env, now_ms, &opts)` returns a
`ValidatedStart`, and `ValidatedStart::accept(env, mode, policy)` finishes it.
The error a doubly-invalid start reports is the same either way.

`StartOptions` (all optional, set with the `with_*` methods):

| Option | Default | Use |
|--------|---------|-----|
| `with_strict(bool)` | `session::requires_strict_session_start(mode)`, so the five standards-track modes are strict | Applies the canonical contract to an extension mode, or lifts it from one |
| `with_descriptor_mode_version(v)` | none | Refuses a payload that names another version, and binds `v` when the payload names none. Record the result, `ValidatedStart::bound_mode_version`, for replay |
| `with_default_max_suspend_ms(ms)` | `session::MAX_SUSPEND_MS` (7 days) | The suspension cap to bind when the payload sets none. Resolution matches `session::resolve_max_suspend_ms` |
| `with_max_ttl_ms(ms)` | `session::MAX_TTL_MS` (24 hours) only | A deployment cap on `ttl_ms`. A start asking for more gets `INVALID_TTL`. It can only tighten the protocol bound. Replay needs no copy of it, because every recorded start was accepted under it |
| `with_session_id_validator(f)` | `validate_session_id_for_acceptance` | A stricter id rule for your deployment |

Do not build a client's session with `Session::builder`. It does no
validation, and exists for replay, tests and sessions you have already
validated.

### Messages

For an in-memory embedder, one call does everything:

```rust
use macp_modes::step::{step, StepOutcome};

match step(&mut session, &env, mode.as_ref(), now_ms)? {
    StepOutcome::Duplicate => { /* §7.6(6): report it as a duplicate */ }
    StepOutcome::Accepted { state, synthesized, .. } => { /* `synthesized` entered history before `env` */ }
    _ => {}
}
```

`step` runs the server kernel's exact order:

1. `check_preconditions`: shape and version → dedup → mode and session binding
   → deadline (`Session::deadline_passed`, which covers TTL and the
   suspension cap) → the OPEN gate.
2. `admit`: sender authorization, then the client boundary. The reserved
   `implicit-accept:` id namespace is refused here.
3. `advance`: any due synthetic envelopes.
4. `dispatch` at `now_ms`.
5. `commit`.

A rejected message consumes no dedup slot and applies no state. The one
exception is a synthetic envelope that was already due: it stays applied even
when its trigger is then rejected, because it was due independently of the
trigger. `step` returns synthetic envelopes only on success. If you must
persist them whatever happens to the trigger, call `advance(&mut session,
mode, now_ms)` first with the same clock.

Both `StepOutcome` and its `Accepted` variant are `#[non_exhaustive]`, so a
`match` on them needs a wildcard arm.

### Durable embedders

If you write each accepted envelope to an append-only log before applying it,
drive the phases yourself so that a failed write never consumes a dedup slot:

```text
check_preconditions ─┬─ Duplicate → report (§7.6(6))
                     ├─ Expired   → session.expire(); record expiry; reject TtlExpired
                     └─ Proceed   → admit
                                    loop { next_synthetic → append → commit_synthetic }
                                    dispatch (or validate_message = admit + dispatch)
                                    append → commit
```

- `commit(session, env, response, now_ms)` consumes the dedup slot, records
  participant activity at `now_ms`, and applies the response.
- `commit_synthetic(session, syn)` consumes the dedup slot and applies the
  response, but records **no** participant activity: the synthetic's sender did
  not act.
- Record a synthetic envelope with an acceptance time equal to its
  `timestamp_unix_ms`, which is the computed deadline (RFC-MACP-0010 §5.1(3)),
  never the observation time. Replay dispatches each entry at its recorded
  time, and any other stamp makes the replayed state diverge from the live one.

## The clock rule

Pass the **recorded acceptance time** (Unix milliseconds) as `now_ms`, and
never pass `0`.

- `0` is not a clock. At `semantics_rev >= 1` modes use `now_ms` as given,
  with no fallback, so every elapsed-time rule would measure from the epoch.
  A handoff offer recorded at `0` reads as "no offer time" and never becomes
  due for its implicit accept (`HandoffMode::due_synthetic_envelope` needs
  `offered_at_ms > 0`).
- Use the same value for every phase of one envelope, and record it with the
  envelope. Replay re-runs each entry at that value.
- In-process, read it from a `macp_core::Clock`: `SystemClock` in production,
  `ManualClock` in tests (`ManualClock::new(t)`, `.set(t)`, `.advance(d)`). The
  server takes the same trait through `Runtime::with_clock(Arc<dyn Clock>)`.
- The initial TTL deadline is the one place where a client clock matters.
  RFC-MACP-0003 §2 bases it on the `SessionStart` envelope's
  `timestamp_unix_ms`, and falls back to `now_ms` only when that is `0`.
  `ValidatedStart::ttl_expiry` holds the result. Every deadline **check**
  still uses `now_ms`.

## `semantics_rev`

Every session records the behavioural revision it was accepted under
(`Session::semantics_rev`). Replay honours the recorded revision, so a runtime
upgrade never changes what an old history rebuilds to.

- `accept_session_start` and `Session::builder` both default to
  `CURRENT_SEMANTICS_REV`, which is **4** in 0.9.0.
- A legacy record that carries no revision replays as `0`.
- Set a lower revision only to replay a history recorded under it.

| Rev | Gates |
|-----|-------|
| 1 | Handoff's implicit-accept clock is the acceptance clock, not the envelope's |
| 2 | Handoff's synthetic implicit accept; the `MAX_SUSPENSION_CYCLES` force-expire on resume (every mode) |
| 3 | The `multi_round` `Contribute` canonical tie-break |
| 4 | Three validation tightenings (0.9.0; `deployment.md`'s "Revision 4"). Decision wire enums are strict: `Vote.vote` and `Evaluation.recommendation` are UPPER_CASE, `Objection.severity` is lowercase, and an empty severity is refused. Handoff cannot re-offer to a target that declined. A Task `TaskReject` before assignment is a personal decline |

## Session identifiers

The default validator, `session::validate_session_id_for_acceptance`,
accepts:

- a UUID v4 or v7 in lowercase hyphenated form. Uppercase is refused, which is
  a local rule stricter than RFC-MACP-0004 §11.2;
- a base64url token of at least 22 characters.

Both are capped at `session::MAX_SESSION_ID_BYTES` (128). That cap is the
§11.2 floor every peer must accept, so ids within it are portable. The format
check runs only at `SessionStart`, and a persisted session with a legacy id
still replays.

## Replay

`macp_modes::replay` rebuilds a `Session` from what you recorded, with no
storage, transport or registry. It is the same code the server's log replay
(`src/replay.rs`) maps onto.

```rust
use macp_modes::replay::{replay, RecordedStart, ReplayEvent, ReplayOptions};

// At acceptance:
let start = RecordedStart::from_session(start_env, &session, validated.bound_mode_version.clone());
// Per accepted event, in order:
events.push(ReplayEvent::Synthetic { envelope: syn });                 // each `synthesized` envelope first
events.push(ReplayEvent::Message { envelope: env, accepted_at_ms: now_ms });
events.push(ReplayEvent::Suspended { at_ms });                         // Session::suspend
events.push(ReplayEvent::Resumed { at_ms });                           // Session::resume
events.push(ReplayEvent::Cancelled);                                   // Session::cancel
events.push(ReplayEvent::Expired);                                     // TtlExpired / cap expiry

// On restart:
let session = replay(&start, events, mode.as_ref(), &ReplayOptions::default())?;
```

- **Record bindings, not configuration.** `RecordedStart` carries
  `semantics_rev`, the resolved `max_suspend_ms` (where `0` means a legacy
  record, and the default cap applies), `bound_mode_version` and the bound
  `policy` definition. Replay never consults live configuration or the policy
  registry (RFC-MACP-0003 §2, RFC-MACP-0012 §8). `RecordedStart::new(env,
  accepted_at_ms)` plus the `with_*` setters builds the same record by hand.
- **No client boundary.** Recorded entries already passed `admit`'s client
  check, and some legal history would fail it (the `implicit-accept:` id).
  Replay still authorizes each sender and dispatches at the recorded time.
- **Activity.** `ReplayEvent::Message` credits participant activity, as
  `commit` did, while `Synthetic` never does. A recorder that cannot tell the
  two apart sets `ReplayOptions::with_credit_participant_activity(false)`. The
  server's log is one: it records both as `Incoming`.
- **After a terminal state,** a recorded message only takes back its dedup
  slot.
- **Checkpoints.** Persist any rebuilt session, reload it, and apply the
  remaining events with `replay::apply_event`. `replay_start` gives you the
  session before any event.
- **Refusals are errors.** If the mode refuses a recorded event, replay
  returns the error rather than build a different session: the history and the
  mode disagree, through corruption or a behaviour change without a
  `semantics_rev` gate.

## Other public building blocks (I17)

These were already public before 0.9.0. Use them instead of reimplementing
them:

| Need | Use |
|------|-----|
| A duplicate submission | `StepOutcome::Duplicate`, `Precheck::Duplicate` |
| The session-id rule on its own | `macp_core::session::validate_session_id_for_acceptance` |
| The RFC-MACP-0013 commitment hash | `macp_core::commitment_hash::commitment_hash(&CommitmentPayload)`. To verify received bytes: `verify_commitment_hash(wire, claimed) -> VerifyOutcome`, `commitment_hash_of_wire`, `is_canonical_commitment_hash` |
| An error's registry code and transport mapping | `MacpError::error_code()`, `transport_status()`, `http_status()`, and `deny_reasons()` for a structured `POLICY_DENIED` |
| Wildcard (`mode: "*"`) policies | `PolicyRegistry::register` validates them against every standards-track mode's rule schema |
| Decision `voting` bounds | `PolicyRegistry::register` refuses `threshold` outside (0, 1], and `weights` that are ≤ 0 or NaN |
| The suspension cap a session runs under | `Session::effective_max_suspend_ms()`, or `session::resolve_max_suspend_ms(payload_value)` at start |
| Deadline checks and expiry | `Session::deadline_passed(now_ms)`, `Session::expire()` |
