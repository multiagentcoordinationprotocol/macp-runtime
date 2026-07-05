# Workflow Primitives

## What Was Deferred

1. **Trigger rules** — automatic session spawning based on conditions (e.g., "when quorum approves, start a task session")
2. **Guard conditions** — pre-conditions that must hold before a session can start or a message can be accepted
3. **Retry/escalation policies** — automatic retry of failed sessions, escalation to different participants
4. **Cross-session deadlines** — deadlines spanning multiple sessions in a workflow

## Why Deferred

- Requires the session composition layer (see `session_composition.md`) as a foundation
- Policy engine design is a significant undertaking that should be informed by real usage patterns
- Risk of over-engineering: better to observe what composition patterns users actually need before building abstractions

## Prerequisites

- Session composition (parent/child relationships) implemented
- At least 3 real-world workflow patterns identified from users
- Decision on policy expression format (code? DSL? JSON config?)

## Implementation Plan

### Phase 1: Trigger Rules

**File:** `src/triggers.rs` (new)

```rust
pub struct TriggerRule {
    pub name: String,
    pub source_mode: String,
    pub source_event: TriggerEvent,
    pub condition: TriggerCondition,
    pub action: TriggerAction,
}

pub enum TriggerEvent {
    SessionResolved,
    SessionExpired,
    MessageAccepted { message_type: String },
}

pub enum TriggerCondition {
    Always,
    ResolutionContains { field: String, value: String },
    ParticipantCount { min: usize },
}

pub enum TriggerAction {
    SpawnSession {
        mode: String,
        participants_from: ParticipantSource,
        ttl_ms: i64,
    },
    SendSignal {
        target_session_id: String,
        signal_type: String,
    },
}
```

Integration point: after `session.apply_mode_response()` in `runtime.rs`, check trigger rules.

### Phase 2: Guard Conditions

Add to `SessionStartPayload` (proto):
```protobuf
message SessionStartPayload {
  // ... existing fields ...
  repeated GuardCondition guards = 11;
}

message GuardCondition {
  string type = 1;           // "session_resolved", "participant_available"
  string reference_id = 2;   // session_id or participant_id
  string expected_state = 3; // "Resolved", "Open"
}
```

Runtime validates guards before accepting SessionStart.

### Phase 3: Retry/Escalation

```rust
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff_ms: Vec<i64>,        // delay between attempts
    pub escalation: Option<EscalationPolicy>,
}

pub struct EscalationPolicy {
    pub escalate_after_attempts: u32,
    pub escalate_to: Vec<String>,     // additional participants
    pub escalation_mode: String,      // mode for escalation session
}
```

### Phase 4: Cross-Session Deadlines

```rust
pub struct WorkflowDeadline {
    pub correlation_id: String,
    pub deadline_unix_ms: i64,
    pub on_expiry: DeadlineAction,
}

pub enum DeadlineAction {
    CancelAll,
    CancelPending,
    Escalate(EscalationPolicy),
    Signal { signal_type: String },
}
```

### Estimated Effort

- Phase 1: 2 weeks
- Phase 2: 1 week
- Phase 3: 2 weeks
- Phase 4: 1 week

### Risks

- Turing-completeness: trigger rules + conditions can become a programming language — keep it declarative
- Infinite loops: trigger A spawns session that triggers B that triggers A — need cycle detection
- Testing: workflow primitives require end-to-end integration tests with multiple sessions
