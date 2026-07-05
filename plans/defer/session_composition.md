# Session Composition & Cross-Session Orchestration

## What Was Deferred

1. **Parent/child session relationships** — ability to spawn a child session from a parent session's resolution, maintaining a causal link
2. **Causal links / correlation IDs** — `SignalPayload.correlation_session_id` exists in the proto but Signal processing is minimal
3. **Cross-session auditability** — end-to-end trace of multi-session workflows

## Why Deferred

- Requires RFC proto changes to define parent/child session semantics
- Cross-session state management is a significant architectural change to `Runtime`
- The composition model should be designed at the spec level first, not runtime-first
- No current user demand — single-session flows cover existing use cases

## Prerequisites

- v1.0 spec freeze for single-session semantics
- RFC consensus on composition primitives (parent_session_id field, spawn semantics, failure propagation)
- Signal processing must be more than a no-op (Tier 3 implemented basic logging, but no session lookup)

## Implementation Plan

### Phase 1: Proto Extensions (RFC work)

Add to `core.proto`:
```protobuf
message SessionStartPayload {
  // ... existing fields ...
  string parent_session_id = 9;       // optional: spawned from this session
  string parent_resolution_ref = 10;  // optional: reference to parent resolution
}
```

Add to `Envelope`:
```protobuf
message Envelope {
  // ... existing fields ...
  string correlation_id = 9;  // optional: trace ID across sessions
}
```

### Phase 2: Runtime Session Graph

**File:** `src/session_graph.rs` (new)

```rust
pub struct SessionGraph {
    children: RwLock<HashMap<String, Vec<String>>>,  // parent -> children
    parent: RwLock<HashMap<String, String>>,          // child -> parent
}

impl SessionGraph {
    pub fn register_child(&self, parent_id: &str, child_id: &str);
    pub fn get_children(&self, parent_id: &str) -> Vec<String>;
    pub fn get_parent(&self, child_id: &str) -> Option<String>;
}
```

Integrate into `Runtime`:
- On `SessionStart` with `parent_session_id`: validate parent exists, register relationship
- On session resolution: optionally notify parent (via Signal or internal event)
- On parent cancellation: policy decision — cancel children or leave orphaned

### Phase 3: Workflow Composition API

**File:** `src/workflow.rs` (new)

Define composition patterns:
```rust
pub enum CompositionPattern {
    Sequential { steps: Vec<SessionTemplate> },
    Parallel { branches: Vec<SessionTemplate>, join: JoinPolicy },
    Conditional { condition: String, then: Box<SessionTemplate>, otherwise: Option<Box<SessionTemplate>> },
}

pub enum JoinPolicy {
    AllComplete,
    FirstComplete,
    Quorum(u32),
}
```

### Phase 4: Correlation and Tracing

- Propagate `correlation_id` from parent to child sessions
- Log correlation_id in all tracing spans
- Add `GET /trace/{correlation_id}` endpoint to retrieve full workflow trace

### Estimated Effort

- Phase 1: 1 week (RFC discussion + proto changes)
- Phase 2: 2 weeks (session graph, persistence, tests)
- Phase 3: 3 weeks (workflow engine, patterns, tests)
- Phase 4: 1 week (tracing integration)

### Risks

- Over-engineering: workflow engines tend to grow unbounded — keep primitives minimal
- Failure semantics: what happens when a child session fails? Compensating transactions are complex
- Persistence: session graph must be durable and replayable
