use crate::error::MacpError;
use crate::pb::macp_runtime_service_server::MacpRuntimeService;
use crate::pb::{
    session_lifecycle_event, Ack, CancelSessionRequest, CancelSessionResponse,
    CancellationCapability, Capabilities, Envelope, GetManifestRequest, GetManifestResponse,
    GetPolicyRequest, GetPolicyResponse, GetSessionRequest, GetSessionResponse, InitializeRequest,
    InitializeResponse, ListExtModesRequest, ListExtModesResponse, ListModesRequest,
    ListModesResponse, ListPoliciesRequest, ListPoliciesResponse, ListRootsRequest,
    ListRootsResponse, ListSessionsRequest, ListSessionsResponse, MacpError as PbMacpError,
    ManifestCapability, ModeRegistryCapability, ParticipantActivity, PolicyDescriptor,
    PolicyRegistryCapability, ProgressCapability, PromoteModeRequest, PromoteModeResponse,
    RegisterExtModeRequest, RegisterExtModeResponse, RegisterPolicyRequest, RegisterPolicyResponse,
    ResumeSessionRequest, ResumeSessionResponse, RootsCapability, RuntimeInfo, SendRequest,
    SendResponse, SessionLifecycleEvent, SessionMetadata, SessionState as PbSessionState,
    SessionsCapability, StreamSessionRequest, StreamSessionResponse, SuspendSessionRequest,
    SuspendSessionResponse, UnregisterExtModeRequest, UnregisterExtModeResponse,
    UnregisterPolicyRequest, UnregisterPolicyResponse, WatchModeRegistryRequest,
    WatchModeRegistryResponse, WatchPoliciesRequest, WatchPoliciesResponse, WatchRootsRequest,
    WatchRootsResponse, WatchSessionsRequest, WatchSessionsResponse, WatchSignalsRequest,
    WatchSignalsResponse, WhatIfReplayRequest, WhatIfReplayResponse,
};
use crate::runtime::{Runtime, WhatIfError};
use crate::security::{AuthIdentity, SecurityLayer};
use crate::session::SessionState;
use std::collections::HashMap;
use std::sync::Arc;
use tonic::{Request, Response, Status};

type SessionResponseStream = std::pin::Pin<
    Box<dyn futures_core::Stream<Item = Result<StreamSessionResponse, Status>> + Send>,
>;

#[derive(Clone)]
pub struct MacpServer {
    runtime: Arc<Runtime>,
    security: SecurityLayer,
    /// RFC-MACP-0012 §9 file-loaded profile: when policies are preloaded from
    /// `MACP_POLICIES_DIR`, the policy registry is read-only over the wire —
    /// `register_policy` is advertised `false` and the mutating RPCs return
    /// `FAILED_PRECONDITION`. Governance then has exactly one source of truth.
    policies_read_only: bool,
    /// Optional external ingress policy engine (E3). Consulted after
    /// authentication, before kernel acceptance; deny-on-error. See
    /// `crate::policy_engine`.
    policy_engine: Option<Arc<dyn crate::policy_engine::PolicyEngine>>,
}

impl MacpServer {
    pub fn new(runtime: Arc<Runtime>, security: SecurityLayer) -> Self {
        Self {
            runtime,
            security,
            policies_read_only: false,
            policy_engine: None,
        }
    }

    pub fn with_read_only_policies(mut self) -> Self {
        self.policies_read_only = true;
        self
    }

    /// Install an external ingress policy engine (OPA/Cedar/custom). All
    /// session starts, session-scoped sends, and session reads are then
    /// additionally gated on it (fail closed).
    pub fn with_policy_engine(
        mut self,
        engine: Arc<dyn crate::policy_engine::PolicyEngine>,
    ) -> Self {
        self.policy_engine = Some(engine);
        self
    }

    /// E3 ingress gate for the send path. No-op without an engine. Denials
    /// surface as `POLICY_DENIED` acks (fail closed, including unrecognized
    /// decisions — `PolicyDecision` is `#[non_exhaustive]`).
    async fn enforce_ingress_policy(
        &self,
        identity: &crate::security::AuthIdentity,
        env: &Envelope,
    ) -> Result<(), MacpError> {
        let Some(engine) = &self.policy_engine else {
            return Ok(());
        };
        let decision = if env.message_type == "SessionStart" {
            engine
                .evaluate_session_start(identity, &env.mode, env)
                .await
        } else if !env.session_id.is_empty() {
            // Session-scoped message: give the engine the session context. A
            // missing session falls through to the kernel's own
            // UnknownSession handling.
            match self.runtime.get_session_checked(&env.session_id).await {
                Some(session) => engine.evaluate_message(identity, &session, env).await,
                None => return Ok(()),
            }
        } else {
            return Ok(());
        };
        match decision {
            macp_core::policy::PolicyDecision::Allow { .. } => Ok(()),
            macp_core::policy::PolicyDecision::Deny { reasons } => {
                Err(MacpError::PolicyDenied { reasons })
            }
            other => {
                tracing::warn!(decision = ?other, "unrecognized ingress policy decision");
                Err(MacpError::PolicyDenied {
                    reasons: vec!["unrecognized policy decision".into()],
                })
            }
        }
    }

    fn validate_envelope_shape(&self, env: &Envelope) -> Result<(), MacpError> {
        if env.macp_version != macp_core::MACP_VERSION {
            return Err(MacpError::InvalidMacpVersion);
        }
        if env.message_type.is_empty() || env.message_id.is_empty() {
            return Err(MacpError::InvalidEnvelope);
        }
        // RFC-MACP-0001: Signals MUST have empty session_id and empty mode.
        // Progress messages MAY be ambient (empty session_id/mode) or session-scoped.
        let is_ambient_type = env.message_type == "Signal" || env.message_type == "Progress";
        if env.message_type == "Signal" {
            if !env.session_id.is_empty() {
                return Err(MacpError::InvalidEnvelope);
            }
            if !env.mode.trim().is_empty() {
                return Err(MacpError::InvalidEnvelope);
            }
        }
        if env.message_type == "Progress" {
            // Progress is valid in exactly two shapes: both session_id and mode
            // empty (ambient), or both non-empty (session-scoped). Exactly one
            // empty is a mixed shape and must be rejected.
            let session_id_empty = env.session_id.is_empty();
            let mode_empty = env.mode.trim().is_empty();
            if session_id_empty != mode_empty {
                return Err(MacpError::InvalidEnvelope);
            }
        }
        if !is_ambient_type && env.session_id.is_empty() {
            return Err(MacpError::InvalidEnvelope);
        }
        if !is_ambient_type && env.mode.trim().is_empty() {
            return Err(MacpError::InvalidEnvelope);
        }
        if env.payload.len() > self.security.max_payload_bytes {
            return Err(MacpError::PayloadTooLarge);
        }
        Ok(())
    }

    fn session_state_to_pb(state: &SessionState) -> i32 {
        match state {
            SessionState::Open => PbSessionState::Open.into(),
            SessionState::Suspended => PbSessionState::Suspended.into(),
            SessionState::Resolved => PbSessionState::Resolved.into(),
            SessionState::Expired => PbSessionState::Expired.into(),
            SessionState::Cancelled => PbSessionState::Cancelled.into(),
        }
    }

    fn session_to_metadata(session: &crate::session::Session) -> SessionMetadata {
        let participant_activity = session
            .participant_message_counts
            .iter()
            .map(|(pid, count)| ParticipantActivity {
                participant_id: pid.clone(),
                last_message_at_unix_ms: session
                    .participant_last_seen
                    .get(pid)
                    .copied()
                    .unwrap_or(0),
                message_count: *count,
            })
            .collect();
        SessionMetadata {
            session_id: session.session_id.clone(),
            mode: session.mode.clone(),
            state: Self::session_state_to_pb(&session.state),
            started_at_unix_ms: session.started_at_unix_ms,
            expires_at_unix_ms: session.ttl_expiry,
            mode_version: session.mode_version.clone(),
            configuration_version: session.configuration_version.clone(),
            policy_version: session.policy_version.clone(),
            participants: session.participants.clone(),
            participant_activity,
            initiator: session.initiator_sender.clone(),
            context_id: session.context_id.clone(),
            extension_keys: session.extensions.keys().cloned().collect(),
        }
    }

    fn make_error_ack(e: &MacpError, env: &Envelope) -> Ack {
        let details = Self::error_details_bytes(e);
        Ack {
            ok: false,
            duplicate: false,
            message_id: env.message_id.clone(),
            session_id: env.session_id.clone(),
            accepted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            session_state: PbSessionState::Unspecified.into(),
            error: Some(PbMacpError {
                code: e.error_code().into(),
                message: e.to_string(),
                session_id: env.session_id.clone(),
                message_id: env.message_id.clone(),
                details,
            }),
        }
    }

    /// Serialize structured error details as JSON bytes for the `details` field.
    /// Currently only `PolicyDenied` carries additional detail:
    ///
    /// ```json
    /// {"reasons": ["<message>", ...],
    ///  "denials": [{"code": "veto", "message": "<message>", "participants": ["agent://a"]}, ...]}
    /// ```
    ///
    /// `denials` is the structured form (RFC-MACP-0012 §6.2): a stable code per
    /// reason, with participant identifiers carried beside the message, never
    /// inside it. `reasons` keeps the pre-0.9 array-of-strings shape (now the
    /// identifier-free messages) so an existing client keeps parsing.
    fn error_details_bytes(e: &MacpError) -> Vec<u8> {
        match e {
            MacpError::PolicyDenied { reasons } => serde_json::to_vec(&serde_json::json!({
                "reasons": reasons.iter().map(|r| r.message.as_str()).collect::<Vec<_>>(),
                "denials": reasons,
            }))
            .unwrap_or_default(),
            _ => vec![],
        }
    }

    /// A rejected stream frame as an inline `MACPError` (RFC-MACP-0001): the
    /// registry code, the same structured details a unary `Ack` carries.
    fn stream_inline_error(
        err: &MacpError,
        session_id: Option<&String>,
        message_id: &str,
    ) -> StreamSessionResponse {
        StreamSessionResponse {
            response: Some(crate::pb::stream_session_response::Response::Error(
                PbMacpError {
                    code: err.error_code().into(),
                    message: err.to_string(),
                    session_id: session_id.cloned().unwrap_or_default(),
                    message_id: message_id.to_string(),
                    details: Self::error_details_bytes(err),
                },
            )),
        }
    }

    fn apply_authenticated_sender(
        identity: &AuthIdentity,
        mut env: Envelope,
    ) -> Result<Envelope, MacpError> {
        if !env.sender.is_empty() && env.sender != identity.sender {
            return Err(MacpError::Unauthenticated);
        }
        env.sender = identity.sender.clone();
        Ok(env)
    }

    async fn authenticate_send_request(
        &self,
        request: &Request<SendRequest>,
        env: Envelope,
    ) -> Result<(Envelope, Option<usize>), MacpError> {
        let identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await?;
        let env = Self::apply_authenticated_sender(&identity, env)?;
        let is_session_start = env.message_type == "SessionStart";
        self.security
            .authorize_mode(&identity, &env.mode, is_session_start)?;
        self.security
            .enforce_rate_limit(&identity.sender, is_session_start)
            .await?;
        // External ingress policy engine (E3): after authentication and the
        // built-in security checks, before kernel acceptance.
        self.enforce_ingress_policy(&identity, &env).await?;
        let max_open = if is_session_start {
            identity.max_open_sessions
        } else {
            None
        };
        Ok((env, max_open))
    }

    async fn authenticate_session_access<T>(
        &self,
        request: &Request<T>,
        session_id: &str,
    ) -> Result<AuthIdentity, Status> {
        let identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        let session = self
            .runtime
            .get_session_checked(session_id)
            .await
            .ok_or_else(|| {
                Self::coded_status(
                    MacpError::UnknownSession,
                    format!("Session '{}' not found", session_id),
                )
            })?;
        let allowed = identity.is_observer
            || session.initiator_sender == identity.sender
            || session.participants.iter().any(|p| p == &identity.sender);
        if !allowed {
            return Err(Self::coded_status(
                MacpError::Forbidden,
                "FORBIDDEN: session access denied",
            ));
        }
        // External ingress policy engine (E3): may additionally restrict
        // reads beyond the built-in membership check (fail closed).
        if let Some(engine) = &self.policy_engine {
            let decision = engine.evaluate_session_access(&identity, &session).await;
            crate::policy_engine::require_allow(decision, "session access")?;
        }
        Ok(identity)
    }

    /// Subscribe-window dedupe (see `replay_dedup` in the stream loop): drop
    /// a buffered envelope that was already delivered in the replay batch;
    /// the first miss disarms the filter (the receiver is FIFO and every
    /// in-window duplicate precedes the first post-snapshot envelope). Every
    /// path that yields broadcast envelopes MUST route through this — the
    /// drain loops bypassing it delivered subscribe-window duplicates.
    fn should_skip_replayed(
        replay_dedup: &mut Option<std::collections::HashSet<String>>,
        envelope: &Envelope,
    ) -> bool {
        if let Some(seen) = replay_dedup.as_mut() {
            if seen.remove(&envelope.message_id) {
                return true;
            }
            *replay_dedup = None;
        }
        false
    }

    fn try_next_stream_event(
        receiver: &mut Option<tokio::sync::broadcast::Receiver<Envelope>>,
    ) -> Result<Option<Envelope>, Status> {
        use tokio::sync::broadcast::error::TryRecvError;

        let rx = match receiver.as_mut() {
            Some(rx) => rx,
            None => return Ok(None),
        };

        match rx.try_recv() {
            Ok(envelope) => Ok(Some(envelope)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Closed) => {
                *receiver = None;
                Ok(None)
            }
            Err(TryRecvError::Lagged(skipped)) => {
                // Terminate the stream so the client knows it missed messages.
                // Consistent with the async recv() path which also returns ResourceExhausted.
                tracing::warn!(
                    skipped,
                    "StreamSession receiver fell behind; terminating stream"
                );
                Err(Self::coded_status(
                    MacpError::StreamLagged,
                    format!("StreamSession receiver fell behind by {skipped} envelopes"),
                ))
            }
        }
    }

    /// Process a single StreamSessionRequest frame.
    ///
    /// Returns `Ok(replay_envelopes)` — empty for normal sends, non-empty when
    /// a subscribe frame triggers history replay (RFC-MACP-0006-A1).
    async fn process_stream_request(
        &self,
        identity: &AuthIdentity,
        req: StreamSessionRequest,
        bound_session_id: &mut Option<String>,
        session_events: &mut Option<tokio::sync::broadcast::Receiver<Envelope>>,
    ) -> Result<Vec<Envelope>, StreamFailure> {
        // RFC-MACP-0006-A1: Handle subscribe-only frame.
        // When subscribe_session_id is set and envelope is absent, subscribe to
        // the session's broadcast channel and replay accepted history.
        if !req.subscribe_session_id.is_empty() {
            if req.envelope.is_some() {
                return Err(StreamFailure::Terminal(Self::coded_status(
                    MacpError::InvalidEnvelope,
                    "StreamSessionRequest must not contain both envelope and subscribe_session_id",
                )));
            }
            return self
                .process_subscribe_frame(
                    identity,
                    &req.subscribe_session_id,
                    req.after_sequence,
                    bound_session_id,
                    session_events,
                )
                .await;
        }

        let envelope = req.envelope.ok_or_else(|| {
            StreamFailure::Terminal(Self::coded_status(
                MacpError::InvalidEnvelope,
                "StreamSessionRequest must contain an envelope or subscribe_session_id",
            ))
        })?;
        let message_id = envelope.message_id.clone();
        let rejected = |err: MacpError| StreamFailure::Rejected {
            err,
            message_id: message_id.clone(),
        };

        self.validate_envelope_shape(&envelope).map_err(rejected)?;
        if envelope.session_id.trim().is_empty() {
            return Err(StreamFailure::Terminal(Self::coded_status(
                MacpError::InvalidEnvelope,
                "StreamSession requires a non-empty session_id",
            )));
        }
        if envelope.mode.trim().is_empty() {
            return Err(StreamFailure::Terminal(Self::coded_status(
                MacpError::InvalidEnvelope,
                "StreamSession requires a non-empty mode",
            )));
        }
        if let Some(bound) = bound_session_id.as_ref() {
            if bound != &envelope.session_id {
                return Err(StreamFailure::Terminal(Self::coded_status(
                    MacpError::InvalidEnvelope,
                    "StreamSession may only carry envelopes for one session_id",
                )));
            }
        }

        let envelope = Self::apply_authenticated_sender(identity, envelope).map_err(rejected)?;
        let is_session_start = envelope.message_type == "SessionStart";

        if !is_session_start {
            if let Some(session) = self.runtime.get_session_checked(&envelope.session_id).await {
                if envelope.mode != session.mode {
                    return Err(StreamFailure::Terminal(Self::coded_status(
                        MacpError::InvalidEnvelope,
                        "INVALID_ENVELOPE: envelope mode does not match the bound session mode",
                    )));
                }
                if session.state != SessionState::Open {
                    return Err(StreamFailure::Terminal(Self::coded_status(
                        MacpError::InvalidEnvelope,
                        "SESSION_NOT_OPEN",
                    )));
                }
            } else if envelope.message_type == "Signal" {
                return Err(StreamFailure::Terminal(Self::coded_status(
                    MacpError::UnknownSession,
                    format!("Session '{}' not found", envelope.session_id),
                )));
            }
        }

        self.security
            .authorize_mode(identity, &envelope.mode, is_session_start)
            .map_err(rejected)?;
        // External ingress policy engine (E3): the stream path must be gated
        // identically to unary Send — without this, a sender denied by an
        // installed engine could simply switch transports.
        self.enforce_ingress_policy(identity, &envelope)
            .await
            .map_err(rejected)?;
        self.security
            .enforce_rate_limit(&identity.sender, is_session_start)
            .await
            .map_err(rejected)?;

        if session_events.is_none() {
            *bound_session_id = Some(envelope.session_id.clone());
            *session_events = Some(self.runtime.subscribe_session_stream(&envelope.session_id));
        }

        let max_open = if is_session_start {
            identity.max_open_sessions
        } else {
            None
        };
        self.runtime
            .process(&envelope, max_open)
            .await
            .map_err(rejected)?;
        Ok(vec![])
    }

    /// RFC-MACP-0006-A1: Process a subscribe-only frame.
    /// Subscribes the stream to the session's broadcast channel and replays
    /// accepted envelope history from `after_sequence` onwards.
    async fn process_subscribe_frame(
        &self,
        identity: &AuthIdentity,
        session_id: &str,
        after_sequence: u64,
        bound_session_id: &mut Option<String>,
        session_events: &mut Option<tokio::sync::broadcast::Receiver<Envelope>>,
    ) -> Result<Vec<Envelope>, StreamFailure> {
        // Validate: only one session per stream
        if let Some(bound) = bound_session_id.as_ref() {
            if bound != session_id {
                return Err(StreamFailure::Terminal(Self::coded_status(
                    MacpError::InvalidEnvelope,
                    "StreamSession may only carry envelopes for one session_id",
                )));
            }
        }

        // Validate session exists
        let session = self
            .runtime
            .get_session_checked(session_id)
            .await
            .ok_or_else(|| {
                StreamFailure::Terminal(Self::coded_status(
                    MacpError::UnknownSession,
                    format!("Session '{}' not found", session_id),
                ))
            })?;

        // Authorize: caller must be a declared participant, initiator, or observer
        let allowed = identity.is_observer
            || session.initiator_sender == identity.sender
            || session.participants.iter().any(|p| p == &identity.sender);
        if !allowed {
            // Per-frame refusal, not a transport failure: inline, stream stays
            // open (the subscriber may still send envelopes it is entitled to).
            return Err(StreamFailure::Inline {
                code: MacpError::Forbidden.error_code(),
                message: "caller is not a declared participant or observer for this session".into(),
            });
        }
        // External ingress policy engine (E3): stream-based history replay is
        // a read and must be gated like GetSession (fail closed).
        if let Some(engine) = &self.policy_engine {
            let decision = engine.evaluate_session_access(identity, &session).await;
            crate::policy_engine::require_allow(decision, "session access").map_err(|status| {
                StreamFailure::Inline {
                    code: MacpError::Forbidden.error_code(),
                    message: status.message().to_string(),
                }
            })?;
        }

        // Subscribe to live broadcast (if not already subscribed)
        if session_events.is_none() {
            *bound_session_id = Some(session_id.to_string());
            *session_events = Some(self.runtime.subscribe_session_stream(session_id));
        }

        tracing::info!(
            session_id = %session_id,
            sender = %identity.sender,
            after_sequence = after_sequence,
            "passive subscribe: replaying session history"
        );

        // Replay accepted envelopes from LogStore
        let replay = self
            .runtime
            .get_session_envelopes_after(session_id, after_sequence)
            .await
            .map_err(|base| {
                // The requested `after_sequence` cannot be served; the
                // subscriber re-subscribes with a later one on the same stream.
                StreamFailure::Inline {
                    code: MacpError::InvalidEnvelope.error_code(),
                    message: format!(
                        "session history before ordinal {base} was compacted; \
                         resume with after_sequence >= {base} or re-read state via GetSession"
                    ),
                }
            })?;

        Ok(replay)
    }

    fn build_stream_session_stream<S>(
        &self,
        identity: AuthIdentity,
        inbound: S,
    ) -> SessionResponseStream
    where
        S: futures_core::Stream<Item = Result<StreamSessionRequest, Status>> + Send + 'static,
    {
        use tokio::sync::broadcast;
        use tokio_stream::StreamExt;

        // Actions collected from tokio::select! arms to process outside the
        // select scope, avoiding borrow and macro-expansion issues with `?`
        // and `yield` inside select branches within try_stream!.
        enum StreamAction {
            ProcessRequest(StreamSessionRequest),
            EmitEnvelope(Envelope),
            ClientError(Status),
            ClientDone,
            EventsClosed,
            Lagged(u64),
        }

        let server = self.clone();
        let output = async_stream::try_stream! {
            let mut inbound = Box::pin(inbound);
            let mut bound_session_id: Option<String> = None;
            let mut session_events: Option<broadcast::Receiver<Envelope>> = None;
            // Subscribe-window dedup (RFC-0006 §3.2): the receiver is
            // subscribed BEFORE the history snapshot is read, so an envelope
            // accepted in that window arrives twice — once in the replay
            // batch, once buffered on the receiver. Buffered events are FIFO
            // and all in-window events precede post-snapshot ones, so we drop
            // buffered envelopes whose message_id was replayed and disarm on
            // the first miss.
            let mut replay_dedup: Option<std::collections::HashSet<String>> = None;

            loop {
                if session_events.is_some() {
                    let action = {
                        let events = session_events.as_mut().unwrap();
                        tokio::select! {
                            maybe_req = inbound.next() => {
                                match maybe_req {
                                    Some(Ok(req)) => StreamAction::ProcessRequest(req),
                                    Some(Err(status)) => StreamAction::ClientError(status),
                                    None => StreamAction::ClientDone,
                                }
                            }
                            recv_result = events.recv() => {
                                match recv_result {
                                    Ok(envelope) => StreamAction::EmitEnvelope(envelope),
                                    Err(broadcast::error::RecvError::Closed) => StreamAction::EventsClosed,
                                    Err(broadcast::error::RecvError::Lagged(n)) => StreamAction::Lagged(n),
                                }
                            }
                        }
                    };

                    match action {
                        StreamAction::ProcessRequest(req) => {
                            match server
                                .process_stream_request(
                                    &identity,
                                    req,
                                    &mut bound_session_id,
                                    &mut session_events,
                                )
                                .await
                            {
                                Ok(replay) => {
                                    // RFC-MACP-0006-A1: yield replayed envelopes from subscribe
                                    if !replay.is_empty() {
                                        replay_dedup = Some(
                                            replay.iter().map(|e| e.message_id.clone()).collect(),
                                        );
                                    }
                                    for env in replay {
                                        yield StreamSessionResponse {
                                            response: Some(
                                                crate::pb::stream_session_response::Response::Envelope(env),
                                            ),
                                        };
                                    }
                                }
                                Err(StreamFailure::Terminal(status)) => {
                                    Err(status)?;
                                }
                                Err(StreamFailure::Rejected { err, .. }) if Self::is_stream_terminal_error(&err) => {
                                    Err(Self::status_from_error(err))?;
                                }
                                Err(StreamFailure::Inline { code, message }) => {
                                    yield StreamSessionResponse {
                                        response: Some(
                                            crate::pb::stream_session_response::Response::Error(
                                                PbMacpError {
                                                    code: code.into(),
                                                    message,
                                                    session_id: bound_session_id.clone().unwrap_or_default(),
                                                    message_id: String::new(),
                                                    details: vec![],
                                                },
                                            ),
                                        ),
                                    };
                                }
                                Err(StreamFailure::Rejected { err, message_id }) => {
                                    // RFC-MACP-0001: application-level validation errors
                                    // are sent as inline MACPError; stream remains open.
                                    yield Self::stream_inline_error(&err, bound_session_id.as_ref(), &message_id);
                                }
                            }
                            while let Some(envelope) = Self::try_next_stream_event(&mut session_events)? {
                                if Self::should_skip_replayed(&mut replay_dedup, &envelope) {
                                    continue;
                                }
                                yield StreamSessionResponse {
                                    response: Some(
                                        crate::pb::stream_session_response::Response::Envelope(envelope),
                                    ),
                                };
                            }
                        }
                        StreamAction::EmitEnvelope(envelope) => {
                            if Self::should_skip_replayed(&mut replay_dedup, &envelope) {
                                continue;
                            }
                            yield StreamSessionResponse {
                                response: Some(
                                    crate::pb::stream_session_response::Response::Envelope(envelope),
                                ),
                            };
                        }
                        StreamAction::ClientError(status) => {
                            Err(status)?;
                        }
                        StreamAction::ClientDone => {
                            while let Some(envelope) = Self::try_next_stream_event(&mut session_events)? {
                                if Self::should_skip_replayed(&mut replay_dedup, &envelope) {
                                    continue;
                                }
                                yield StreamSessionResponse {
                                    response: Some(
                                        crate::pb::stream_session_response::Response::Envelope(envelope),
                                    ),
                                };
                            }
                            break;
                        }
                        StreamAction::EventsClosed => {
                            session_events = None;
                        }
                        StreamAction::Lagged(skipped) => {
                            Err(Self::coded_status(MacpError::StreamLagged, format!(
                                "StreamSession receiver fell behind by {skipped} envelopes"
                            )))?;
                        }
                    }
                } else {
                    match inbound.next().await {
                        Some(Ok(req)) => {
                            match server
                                .process_stream_request(
                                    &identity,
                                    req,
                                    &mut bound_session_id,
                                    &mut session_events,
                                )
                                .await
                            {
                                Ok(replay) => {
                                    // RFC-MACP-0006-A1: yield replayed envelopes from subscribe
                                    if !replay.is_empty() {
                                        replay_dedup = Some(
                                            replay.iter().map(|e| e.message_id.clone()).collect(),
                                        );
                                    }
                                    for env in replay {
                                        yield StreamSessionResponse {
                                            response: Some(
                                                crate::pb::stream_session_response::Response::Envelope(env),
                                            ),
                                        };
                                    }
                                }
                                Err(StreamFailure::Terminal(status)) => {
                                    Err(status)?;
                                }
                                Err(StreamFailure::Rejected { err, .. }) if Self::is_stream_terminal_error(&err) => {
                                    Err(Self::status_from_error(err))?;
                                }
                                Err(StreamFailure::Inline { code, message }) => {
                                    yield StreamSessionResponse {
                                        response: Some(
                                            crate::pb::stream_session_response::Response::Error(
                                                PbMacpError {
                                                    code: code.into(),
                                                    message,
                                                    session_id: bound_session_id.clone().unwrap_or_default(),
                                                    message_id: String::new(),
                                                    details: vec![],
                                                },
                                            ),
                                        ),
                                    };
                                }
                                Err(StreamFailure::Rejected { err, message_id }) => {
                                    // RFC-MACP-0001: application-level validation errors
                                    // are sent as inline MACPError; stream remains open.
                                    yield Self::stream_inline_error(&err, bound_session_id.as_ref(), &message_id);
                                }
                            }
                            while let Some(envelope) = Self::try_next_stream_event(&mut session_events)? {
                                if Self::should_skip_replayed(&mut replay_dedup, &envelope) {
                                    continue;
                                }
                                yield StreamSessionResponse {
                                    response: Some(
                                        crate::pb::stream_session_response::Response::Envelope(envelope),
                                    ),
                                };
                            }
                        }
                        Some(Err(status)) => Err(status)?,
                        None => break,
                    }
                }
            }
        };
        Box::pin(output)
    }

    /// Returns true if a rejected frame's error should terminate the
    /// StreamSession stream rather than come back as an inline `MACPError`.
    ///
    /// Decided by the error, never by its gRPC status: S13 moved
    /// `INVALID_ENVELOPE` to `INVALID_ARGUMENT` and `SESSION_NOT_FOUND` to
    /// `NOT_FOUND`, and classifying by status would have turned every mode
    /// validation failure into a dropped stream. The set is exactly the errors
    /// that terminated before S13 — authentication, resource limits, storage
    /// failure, and the SessionStart-binding failures — so stream behaviour is
    /// unchanged; everything else (mode validation, policy denial, a closed
    /// session) stays inline, per RFC-MACP-0001.
    fn is_stream_terminal_error(err: &MacpError) -> bool {
        matches!(
            err,
            MacpError::Unauthenticated
                | MacpError::PayloadTooLarge
                | MacpError::RateLimited
                | MacpError::StorageFailed
                | MacpError::InvalidSessionId
                | MacpError::InvalidPolicyDefinition
                | MacpError::SessionAlreadyExists
        )
    }

    /// `MacpError` → gRPC status, per the error-codes registry's gRPC column
    /// (S13; `macp_core::error::ERROR_CODE_REGISTRY` is the single table).
    ///
    /// Every status carries the registry code in `macp-error-code` metadata, so
    /// a client can branch on the code without parsing the message. A
    /// `PolicyDenied` also carries its structured reasons, JSON-encoded, in
    /// `macp-error-details-bin` (the same bytes as `MACPError.details`).
    fn status_from_error(err: MacpError) -> Status {
        let msg = match &err {
            MacpError::PolicyDenied { reasons } if !reasons.is_empty() => format!(
                "PolicyDenied: {}",
                macp_core::policy::deny::join_messages(reasons, "; ")
            ),
            _ => err.to_string(),
        };
        Self::coded_status(err, msg)
    }

    /// [`Self::status_from_error`] with a caller-chosen human-readable
    /// message: the gRPC code and `macp-error-code` still come from the
    /// registry, so a control-plane RPC (GetSession, CancelSession, the policy
    /// registry, the watch streams) is as machine-branchable as `Send`.
    fn coded_status(err: MacpError, msg: impl Into<String>) -> Status {
        let code = tonic::Code::from_i32(err.transport_status().grpc_code());
        let mut status = Status::new(code, msg.into());
        status.metadata_mut().insert(
            "macp-error-code",
            tonic::metadata::MetadataValue::from_static(err.error_code()),
        );
        let details = Self::error_details_bytes(&err);
        if !details.is_empty() {
            let val = tonic::metadata::MetadataValue::from_bytes(&details);
            status
                .metadata_mut()
                .insert_bin("macp-error-details-bin", val);
        }
        status
    }
}

/// `WhatIfRun` → `WhatIfReplayResponse`.
///
/// The wire reports accepted-history entries only: `history_index` is the
/// 0-based accepted ordinal (`SessionStart` = 0, so the library's
/// `Event(i)` is whatever index that event took), and runtime-internal
/// events (suspend/resume/cancel/expiry), which take no ordinal, are omitted.
/// `first_divergence` is recomputed over the entries reported.
pub mod what_if {
    use crate::pb::what_if_entry_outcome::Kind;
    use crate::pb::{
        WhatIfDenial, WhatIfEntry, WhatIfEntryOutcome, WhatIfOutcome, WhatIfReplayResponse,
    };
    use crate::runtime::WhatIfRun;
    use macp_core::policy::DenyReason;
    use macp_modes::what_if::{EntryOutcome, HistoryPoint, Outcome};

    /// The `WhatIfReplayResponse` for a run, as the RPC returns it.
    pub fn response(run: WhatIfRun) -> WhatIfReplayResponse {
        let WhatIfRun {
            report,
            history_index,
        } = run;
        let index = |at: HistoryPoint| -> Option<u64> {
            match at {
                HistoryPoint::SessionStart => Some(0),
                HistoryPoint::Event(i) => history_index.get(i).copied().flatten(),
                _ => None,
            }
        };
        let mut first_divergence = None;
        let mut entries = Vec::new();
        for e in &report.entries {
            let Some(i) = index(e.at) else { continue };
            if first_divergence.is_none() && e.diverges() {
                first_divergence = Some(i);
            }
            entries.push(WhatIfEntry {
                history_index: i,
                message_id: e.message_id.clone().unwrap_or_default(),
                message_type: e.message_type.clone().unwrap_or_default(),
                recorded: Some(entry_outcome(&e.recorded)),
                candidate: Some(entry_outcome(&e.candidate)),
            });
        }
        let denials = report
            .denials
            .iter()
            .filter_map(|d| {
                Some(WhatIfDenial {
                    history_index: index(d.at)?,
                    message_id: d.message_id.clone().unwrap_or_default(),
                    details: details(&d.reasons),
                })
            })
            .collect();
        WhatIfReplayResponse {
            candidate_policy_id: report.candidate_policy_id.clone(),
            recorded_policy_id: report
                .recorded_policy_id
                .clone()
                .unwrap_or_else(|| crate::policy::defaults::DEFAULT_POLICY_ID.to_string()),
            recorded: Some(outcome(&report.recorded, &index)),
            candidate: Some(outcome(&report.candidate, &index)),
            first_divergence,
            denials,
            entries,
        }
    }

    fn entry_outcome(o: &EntryOutcome) -> WhatIfEntryOutcome {
        let mut out = WhatIfEntryOutcome::default();
        match o {
            EntryOutcome::Applied { state } => {
                out.kind = Kind::Applied.into();
                out.state = super::MacpServer::session_state_to_pb(state);
            }
            EntryOutcome::NotOpen => out.kind = Kind::NotOpen.into(),
            EntryOutcome::Refused { code, reasons } => {
                out.kind = Kind::Refused.into();
                out.error_code = code.to_string();
                out.details = details(reasons);
            }
            EntryOutcome::NoSession => out.kind = Kind::NoSession.into(),
            _ => {}
        }
        out
    }

    fn outcome(o: &Outcome, index: &impl Fn(HistoryPoint) -> Option<u64>) -> WhatIfOutcome {
        let by = o.resolved_by.as_ref();
        WhatIfOutcome {
            state: o
                .state
                .as_ref()
                .map(super::MacpServer::session_state_to_pb)
                .unwrap_or_default(),
            resolution: o.resolution.clone().unwrap_or_default(),
            resolved_by_history_index: by.and_then(|b| index(b.at)).unwrap_or_default(),
            resolved_by_message_id: by.map(|b| b.message_id.clone()).unwrap_or_default(),
            resolved_by_message_type: by.map(|b| b.message_type.clone()).unwrap_or_default(),
        }
    }

    /// The bytes `MACPError.details` carries for `POLICY_DENIED` (see
    /// `MacpServer::error_details_bytes`); empty when there are no reasons.
    fn details(reasons: &[DenyReason]) -> Vec<u8> {
        if reasons.is_empty() {
            return Vec::new();
        }
        super::MacpServer::error_details_bytes(&crate::error::MacpError::PolicyDenied {
            reasons: reasons.to_vec(),
        })
    }
}

/// Why a StreamSession frame failed: a transport/binding problem that ends the
/// stream, or a protocol rejection of one envelope (inline unless
/// [`MacpServer::is_stream_terminal_error`]).
#[derive(Debug)]
enum StreamFailure {
    Terminal(Status),
    Rejected {
        err: MacpError,
        message_id: String,
    },
    /// A per-frame refusal with no `MacpError` (a subscribe frame's read
    /// gate, an unservable `after_sequence`): inline, stream stays open.
    Inline {
        code: &'static str,
        message: String,
    },
}

#[cfg(test)]
impl StreamFailure {
    /// The gRPC code this failure carries (a rejection: its registry status).
    fn code(&self) -> tonic::Code {
        match self {
            StreamFailure::Terminal(status) => status.code(),
            StreamFailure::Rejected { err, .. } => {
                tonic::Code::from_i32(err.transport_status().grpc_code())
            }
            StreamFailure::Inline { code, .. } => tonic::Code::from_i32(
                macp_core::error::registry_entry(code)
                    .map(|e| e.grpc.grpc_code())
                    .unwrap_or(13),
            ),
        }
    }
}

#[tonic::async_trait]
impl MacpRuntimeService for MacpServer {
    async fn initialize(
        &self,
        request: Request<InitializeRequest>,
    ) -> Result<Response<InitializeResponse>, Status> {
        let req = request.into_inner();
        if req.supported_protocol_versions.is_empty() {
            return Err(Self::coded_status(
                MacpError::InvalidRequest,
                "INVALID_REQUEST: supported_protocol_versions must not be empty",
            ));
        }
        if !req
            .supported_protocol_versions
            .iter()
            .any(|v| v == macp_core::MACP_VERSION)
        {
            // Registry: UNSUPPORTED_PROTOCOL_VERSION → INVALID_ARGUMENT (S13).
            return Err(Self::coded_status(
                MacpError::InvalidMacpVersion,
                "UNSUPPORTED_PROTOCOL_VERSION: no mutually supported protocol version",
            ));
        }

        Ok(Response::new(InitializeResponse {
            selected_protocol_version: macp_core::MACP_VERSION.into(),
            runtime_info: Some(RuntimeInfo {
                name: "macp-runtime".into(),
                title: "MACP Reference Runtime".into(),
                // Tracks the workspace version; a literal here drifted once
                // (still advertising 0.4.0 after the 0.5.0 release).
                version: env!("CARGO_PKG_VERSION").into(),
                description: "Reference implementation of the Multi-Agent Coordination Protocol"
                    .into(),
                website_url: String::new(),
            }),
            capabilities: Some(Capabilities {
                sessions: Some(SessionsCapability { stream: true, list_sessions: true, watch_sessions: true }),
                cancellation: Some(CancellationCapability {
                    cancel_session: true,
                }),
                progress: Some(ProgressCapability { progress: true }),
                manifest: Some(ManifestCapability { get_manifest: true }),
                mode_registry: Some(ModeRegistryCapability {
                    list_modes: true,
                    list_changed: true,
                }),
                roots: Some(RootsCapability {
                    // ListRoots is answerable (the root set is empty — a valid
                    // state), but this runtime has no roots provider, so the
                    // set never changes: do not advertise change notifications
                    // (RFC-MACP-0006 §3.3 gates WatchRoots on list_changed).
                    // Revisit when a roots provider lands (plans E2).
                    list_roots: true,
                    list_changed: false,
                }),
                policy_registry: Some(PolicyRegistryCapability {
                    what_if_replay: true,
                    register_policy: !self.policies_read_only,
                    list_policies: true,
                    list_changed: true,
                    // Advisory (policy.proto): admission stays authoritative.
                    rule_extensions: self.runtime.policy_registry().rule_extension_names(),
                }),
                experimental: Some(crate::pb::ExperimentalCapabilities {
                    features: HashMap::from([
                        ("ext_mode_lifecycle".into(), "true".into()),
                    ]),
                }),
            }),
            supported_modes: self.runtime.registered_mode_names(),
            instructions: "Authenticate requests with Authorization: Bearer <token>. Use the unary Send RPC for all session messaging. For local development only (MACP_ALLOW_INSECURE=1 with no auth configured), the bearer token's value is used directly as the sender identity.".into(),
        }))
    }

    async fn send(&self, request: Request<SendRequest>) -> Result<Response<SendResponse>, Status> {
        let env = request.get_ref().envelope.clone().ok_or_else(|| {
            Self::coded_status(
                MacpError::InvalidEnvelope,
                "SendRequest must contain an envelope",
            )
        })?;

        let result = async {
            self.validate_envelope_shape(&env)?;
            let (env, max_open) = self.authenticate_send_request(&request, env).await?;
            self.runtime
                .process(&env, max_open)
                .await
                .map(|process_result| (env, process_result))
        }
        .await;

        let ack = match result {
            Ok((env, process_result)) => Ack {
                ok: true,
                duplicate: process_result.duplicate,
                message_id: env.message_id.clone(),
                session_id: env.session_id.clone(),
                accepted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                session_state: Self::session_state_to_pb(&process_result.session_state),
                error: None,
            },
            Err(err) => {
                let env = request.get_ref().envelope.clone().unwrap_or_default();
                // Rejection counters were collected but never recorded before
                // (permanently zero). Session-scoped rejections are counted
                // per mode; commitments additionally under their own counter.
                if !env.session_id.is_empty() {
                    self.runtime.metrics().record_message_rejected(&env.mode);
                    if env.message_type == "Commitment" {
                        self.runtime.metrics().record_commitment_rejected(&env.mode);
                    }
                }
                Self::make_error_ack(&err, &env)
            }
        };

        Ok(Response::new(SendResponse { ack: Some(ack) }))
    }

    async fn get_session(
        &self,
        request: Request<GetSessionRequest>,
    ) -> Result<Response<GetSessionResponse>, Status> {
        let session_id = request.get_ref().session_id.clone();
        let _identity = self
            .authenticate_session_access(&request, &session_id)
            .await?;
        let session = self
            .runtime
            .get_session_checked(&session_id)
            .await
            .ok_or_else(|| {
                Self::coded_status(
                    MacpError::UnknownSession,
                    format!("Session '{}' not found", session_id),
                )
            })?;

        Ok(Response::new(GetSessionResponse {
            metadata: Some(Self::session_to_metadata(&session)),
        }))
    }

    async fn cancel_session(
        &self,
        request: Request<CancelSessionRequest>,
    ) -> Result<Response<CancelSessionResponse>, Status> {
        let session_id = request.get_ref().session_id.clone();
        let identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        let session = self
            .runtime
            .get_session_checked(&session_id)
            .await
            .ok_or_else(|| {
                Self::coded_status(
                    MacpError::UnknownSession,
                    format!("Session '{}' not found", session_id),
                )
            })?;
        // RFC-MACP-0001: "Only the initiator and policy-delegated roles may cancel."
        // CancelSession is a Core control-plane message — mode authorization does not apply.
        if identity.sender != session.initiator_sender
            && crate::mode::util::check_commitment_authority(&session, &identity.sender).is_err()
        {
            return Err(Self::coded_status(
                MacpError::Forbidden,
                "FORBIDDEN: only the session initiator or policy-delegated roles can cancel",
            ));
        }
        let sender = identity.sender.clone();
        let req = request.into_inner();
        match self
            .runtime
            .cancel_session(&req.session_id, &req.reason, &sender)
            .await
        {
            Ok(result) => Ok(Response::new(CancelSessionResponse {
                ack: Some(Ack {
                    ok: true,
                    duplicate: false,
                    message_id: String::new(),
                    session_id: req.session_id,
                    accepted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                    session_state: Self::session_state_to_pb(&result.session_state),
                    error: None,
                }),
            })),
            Err(err) => Ok(Response::new(CancelSessionResponse {
                ack: Some(Ack {
                    ok: false,
                    duplicate: false,
                    message_id: String::new(),
                    session_id: req.session_id.clone(),
                    accepted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                    session_state: PbSessionState::Unspecified.into(),
                    error: Some(PbMacpError {
                        code: err.error_code().into(),
                        message: err.to_string(),
                        session_id: req.session_id,
                        message_id: String::new(),
                        details: vec![],
                    }),
                }),
            })),
        }
    }

    async fn suspend_session(
        &self,
        request: Request<SuspendSessionRequest>,
    ) -> Result<Response<SuspendSessionResponse>, Status> {
        let session_id = request.get_ref().session_id.clone();
        let identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        let session = self
            .runtime
            .get_session_checked(&session_id)
            .await
            .ok_or_else(|| {
                Self::coded_status(
                    MacpError::UnknownSession,
                    format!("Session '{}' not found", session_id),
                )
            })?;
        // RFC-MACP-0001 §7.5: same authority model as CancelSession — initiator
        // or policy-delegated roles only; mode authorization does not apply.
        if identity.sender != session.initiator_sender
            && crate::mode::util::check_commitment_authority(&session, &identity.sender).is_err()
        {
            return Err(Self::coded_status(
                MacpError::Forbidden,
                "FORBIDDEN: only the session initiator or policy-delegated roles can suspend",
            ));
        }
        let sender = identity.sender.clone();
        let req = request.into_inner();
        match self
            .runtime
            .suspend_session(&req.session_id, &req.reason, &sender)
            .await
        {
            Ok(result) => Ok(Response::new(SuspendSessionResponse {
                ack: Some(Ack {
                    ok: true,
                    duplicate: false,
                    message_id: String::new(),
                    session_id: req.session_id,
                    accepted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                    session_state: Self::session_state_to_pb(&result.session_state),
                    error: None,
                }),
            })),
            Err(err) => Ok(Response::new(SuspendSessionResponse {
                ack: Some(Ack {
                    ok: false,
                    duplicate: false,
                    message_id: String::new(),
                    session_id: req.session_id.clone(),
                    accepted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                    session_state: PbSessionState::Unspecified.into(),
                    error: Some(PbMacpError {
                        code: err.error_code().into(),
                        message: err.to_string(),
                        session_id: req.session_id,
                        message_id: String::new(),
                        details: vec![],
                    }),
                }),
            })),
        }
    }

    async fn resume_session(
        &self,
        request: Request<ResumeSessionRequest>,
    ) -> Result<Response<ResumeSessionResponse>, Status> {
        let session_id = request.get_ref().session_id.clone();
        let identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        let session = self
            .runtime
            .get_session_checked(&session_id)
            .await
            .ok_or_else(|| {
                Self::coded_status(
                    MacpError::UnknownSession,
                    format!("Session '{}' not found", session_id),
                )
            })?;
        if identity.sender != session.initiator_sender
            && crate::mode::util::check_commitment_authority(&session, &identity.sender).is_err()
        {
            return Err(Self::coded_status(
                MacpError::Forbidden,
                "FORBIDDEN: only the session initiator or policy-delegated roles can resume",
            ));
        }
        let sender = identity.sender.clone();
        let req = request.into_inner();
        match self
            .runtime
            .resume_session(&req.session_id, &req.reason, &sender)
            .await
        {
            Ok(result) => Ok(Response::new(ResumeSessionResponse {
                ack: Some(Ack {
                    ok: true,
                    duplicate: false,
                    message_id: String::new(),
                    session_id: req.session_id,
                    accepted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                    session_state: Self::session_state_to_pb(&result.session_state),
                    error: None,
                }),
            })),
            Err(err) => Ok(Response::new(ResumeSessionResponse {
                ack: Some(Ack {
                    ok: false,
                    duplicate: false,
                    message_id: String::new(),
                    session_id: req.session_id.clone(),
                    accepted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                    session_state: PbSessionState::Unspecified.into(),
                    error: Some(PbMacpError {
                        code: err.error_code().into(),
                        message: err.to_string(),
                        session_id: req.session_id,
                        message_id: String::new(),
                        details: vec![],
                    }),
                }),
            })),
        }
    }

    async fn get_manifest(
        &self,
        request: Request<GetManifestRequest>,
    ) -> Result<Response<GetManifestResponse>, Status> {
        let req = request.into_inner();
        if !req.agent_id.is_empty() && req.agent_id != "macp-runtime" {
            return Err(Self::coded_status(
                MacpError::AgentNotFound,
                format!("Agent '{}' not found", req.agent_id),
            ));
        }

        Ok(Response::new(GetManifestResponse {
            manifest: Some(crate::pb::AgentManifest {
                agent_id: "macp-runtime".into(),
                title: "MACP Reference Runtime".into(),
                description: "Reference implementation of MACP".into(),
                supported_modes: self.runtime.registered_mode_names(),
                input_content_types: vec!["application/macp-envelope+proto".into()],
                output_content_types: vec!["application/macp-envelope+proto".into()],
                metadata: HashMap::new(),
                // Empty: unary-first profile has no dedicated transport endpoints.
                transport_endpoints: vec![],
            }),
        }))
    }

    async fn list_modes(
        &self,
        _request: Request<ListModesRequest>,
    ) -> Result<Response<ListModesResponse>, Status> {
        Ok(Response::new(ListModesResponse {
            modes: self.runtime.standard_mode_descriptors(),
        }))
    }

    async fn list_roots(
        &self,
        _request: Request<ListRootsRequest>,
    ) -> Result<Response<ListRootsResponse>, Status> {
        Ok(Response::new(ListRootsResponse { roots: vec![] }))
    }

    type StreamSessionStream = SessionResponseStream;

    async fn stream_session(
        &self,
        request: Request<tonic::Streaming<StreamSessionRequest>>,
    ) -> Result<Response<Self::StreamSessionStream>, Status> {
        let identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        let inbound = request.into_inner();
        Ok(Response::new(
            self.build_stream_session_stream(identity, inbound),
        ))
    }

    type WatchModeRegistryStream = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<WatchModeRegistryResponse, Status>> + Send>,
    >;

    async fn watch_mode_registry(
        &self,
        _request: Request<WatchModeRegistryRequest>,
    ) -> Result<Response<Self::WatchModeRegistryStream>, Status> {
        let mut rx = self.runtime.subscribe_mode_changes();
        let stream = async_stream::try_stream! {
            // Send initial state
            yield WatchModeRegistryResponse {
                change: Some(crate::pb::RegistryChanged {
                    registry: "modes".into(),
                    observed_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                }),
            };
            // Wait for changes from register/unregister/promote
            while rx.recv().await.is_ok() {
                yield WatchModeRegistryResponse {
                    change: Some(crate::pb::RegistryChanged {
                        registry: "modes".into(),
                        observed_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                    }),
                };
            }
        };
        Ok(Response::new(Box::pin(stream)))
    }

    type WatchRootsStream = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<WatchRootsResponse, Status>> + Send>,
    >;

    async fn watch_roots(
        &self,
        _request: Request<WatchRootsRequest>,
    ) -> Result<Response<Self::WatchRootsStream>, Status> {
        let initial = WatchRootsResponse {
            change: Some(crate::pb::RootsChanged {
                observed_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            }),
        };
        let stream = async_stream::try_stream! {
            yield initial;
            // Roots are static — keep the stream open but idle.
            std::future::pending::<()>().await;
        };
        Ok(Response::new(Box::pin(stream)))
    }

    type WatchSignalsStream = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<WatchSignalsResponse, Status>> + Send>,
    >;

    type WatchSessionsStream = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<WatchSessionsResponse, Status>> + Send>,
    >;

    async fn watch_signals(
        &self,
        request: Request<WatchSignalsRequest>,
    ) -> Result<Response<Self::WatchSignalsStream>, Status> {
        // Ambient signals carry agent-generated payload data; subscribing is
        // gated on authentication like the session-observation surfaces.
        // (RFC-0004 §4.1 constrains unauthenticated *producers*; requiring
        // authenticated subscribers is this runtime's hardening posture.)
        let _identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        // RFC-MACP-0004 §4.1 / RFC-MACP-0006 §3.4: `true` only when the
        // sender was authenticated per RFC-MACP-0004 §3. Every Signal on the
        // bus entered through this server's security layer, which derived its
        // `sender` from the credential, so the layer decides for all of them;
        // dev mode takes the bearer value as the sender (self-assertion) and
        // reports `false`.
        let sender_authenticated = self.security.authenticates_senders();
        let mut rx = self.runtime.subscribe_signals();
        let stream = async_stream::try_stream! {
            loop {
                match rx.recv().await {
                    Ok(envelope) => {
                        yield WatchSignalsResponse {
                            envelope: Some(envelope),
                            sender_authenticated,
                        };
                    }
                    // Surface lag instead of silently ending the stream: a
                    // slow consumer must be able to distinguish "no traffic"
                    // from "events dropped" (mirrors StreamSession).
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        Err(Self::coded_status(MacpError::StreamLagged, format!(
                            "WatchSignals receiver fell behind by {skipped} signals"
                        )))?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        };
        Ok(Response::new(Box::pin(stream)))
    }

    // Session lifecycle observation RPCs

    async fn list_sessions(
        &self,
        request: Request<ListSessionsRequest>,
    ) -> Result<Response<ListSessionsResponse>, Status> {
        // Authentication stays the FIRST statement: an unauthenticated caller
        // must see UNAUTHENTICATED, never a request-shape error, which would
        // confirm the endpoint is live and leak validation order pre-auth.
        let _identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        let req = request.into_inner();

        // Paged: a keyset scan over session IDs in ascending byte order, with
        // `next_page_token` carrying the last emitted ID as the cursor. Per
        // `macp-proto` core.proto:411-426, `page_size = 0` means the
        // server-chosen default, the server MAY cap the effective size, and a
        // response is complete only when `next_page_token` is empty — a page
        // may be short while more results remain.
        if req.page_size < 0 {
            return Err(Self::coded_status(
                MacpError::InvalidRequest,
                "INVALID_ARGUMENT: page_size must not be negative",
            ));
        }
        // The cast is safe only below the negative guard above: `page_size` is
        // an int32, and `-1 as usize` is astronomically large on 64-bit.
        let effective = if req.page_size == 0 {
            self.security.list_sessions_default_page_size
        } else {
            (req.page_size as usize).min(self.security.list_sessions_max_page_size)
        };
        // Floor, not redundant: at `effective == 0`, `page_ids` is `&ids[..0]`,
        // so `page_ids.last()` is `None` and the `next_page_token` match below
        // falls to its `_` arm — an empty page paired with an empty token,
        // which looks complete. The traversal terminates immediately and
        // `ListSessions` silently returns nothing, forever. The configured
        // values are `>= 1` four ways today, but both fields are deliberately
        // `pub`, so a library consumer or a test can set 0.
        let effective = effective.max(1);

        let cursor = if req.page_token.is_empty() {
            None
        } else {
            // Every decode failure collapses to one opaque message: the token
            // must not be an oracle for which check rejected it. The error
            // discriminant is dropped here and never logged (see
            // `crate::pagination` — the token is attacker-chosen bytes).
            Some(
                crate::pagination::decode_page_token(&req.page_token).map_err(|_| {
                    Self::coded_status(
                        MacpError::InvalidRequest,
                        "INVALID_ARGUMENT: page_token is not a valid continuation token",
                    )
                })?,
            )
        };

        // Fetch one extra ID than the page holds: its presence is an exact
        // "more results exist" signal, so the terminal page carries an empty
        // token with no extra round trip. `saturating_add` because `effective`
        // is operator-controlled.
        let ids = self
            .runtime
            .registry
            .session_ids_after(cursor.as_deref(), effective.saturating_add(1))
            .await;
        let has_more = ids.len() > effective;
        let page_ids = &ids[..effective.min(ids.len())];

        // The cursor comes from the ID list, NOT from the materialized
        // sessions below. A session can be removed between the ID scan and the
        // fetch; deriving the cursor from what survived would stall the cursor
        // (re-emitting the same page) or, if the whole page vanished, drop
        // every remaining session by terminating the traversal early.
        let next_page_token = match (has_more, page_ids.last()) {
            (true, Some(last)) => crate::pagination::encode_page_token(last),
            _ => String::new(),
        };

        let mut metadata: Vec<SessionMetadata> = Vec::with_capacity(page_ids.len());
        for id in page_ids {
            // A `None` here means the session was removed between the scan and
            // this fetch; skipping it is correct. The resulting short page with
            // a non-empty token is explicitly permitted by core.proto:411-414.
            if let Some(session) = self.runtime.registry.get_session(id).await {
                debug_assert_eq!(
                    session.session_id, *id,
                    "registry map key must equal Session::session_id — paging orders \
                     by the key but emits the field"
                );
                metadata.push(Self::session_to_metadata(&session));
            }
        }

        Ok(Response::new(ListSessionsResponse {
            sessions: metadata,
            next_page_token,
        }))
    }

    async fn watch_sessions(
        &self,
        request: Request<WatchSessionsRequest>,
    ) -> Result<Response<Self::WatchSessionsStream>, Status> {
        let _identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        // Subscribed HERE, during the unary call — strictly before the generator
        // below is first polled, which does not happen until the client reads.
        // An event published in that gap must be buffered by an existing
        // subscription, not missed; moving this inside the generator
        // reintroduces exactly that race.
        let mut rx = self.runtime.subscribe_session_lifecycle();
        let runtime = Arc::clone(&self.runtime);
        let stream = async_stream::try_stream! {
            // Initial sync: emit all current sessions as CREATED events.
            //
            // The traversal snapshots the registry's shared session handles
            // ONCE and then locks and clones one session at a time (see
            // `watch_sync`), so peak resident `Session` clones is one rather
            // than the registry size: this generator is paced by the client's
            // reads and there can be `MACP_MAX_CONCURRENT_STREAMS` of them at
            // once.
            let mut sync = crate::watch_sync::InitialSync::begin(&runtime.registry).await;
            // The IDs this sync emits. A Created event buffered in the
            // subscribe→sync window would duplicate one of them — the session
            // was already registered when the snapshot was taken, but
            // `process_session_start` inserts into the registry BEFORE
            // publishing Created, so that event can still arrive afterwards.
            // Session IDs are create-once and `runtime.rs` holds the only
            // `Created` publisher, so one `send` per session start means a
            // *live* Created can never repeat for an ID already in this set —
            // membership is read, never extended, past the sync. That keeps the
            // set bounded by the registry size at subscribe time instead of
            // growing with every session the stream ever observes.
            let mut synced: std::collections::HashSet<String> =
                std::collections::HashSet::with_capacity(sync.remaining());
            // Lifecycle events that arrive while the sync is still emitting.
            // The sync loop cannot `recv().await` (it has its own output to
            // produce) but must not ignore the bus either: the bus holds 64
            // events, so a slow sync would otherwise make the first post-sync
            // `recv()` return `Lagged` and kill the stream. Bounded by
            // `PENDING_EVENT_LIMIT`; on overflow the client gets the same
            // `RESOURCE_EXHAUSTED` it gets for bus lag.
            let mut pending: std::collections::VecDeque<crate::runtime::SessionLifecycleEvent> =
                std::collections::VecDeque::new();
            loop {
                // Drained BEFORE the next session is fetched, so the bus is
                // relieved once per emitted session rather than once for the
                // whole sync.
                if let Err(drain_err) = crate::watch_sync::drain_lifecycle_events(
                    &mut rx,
                    &mut pending,
                    crate::watch_sync::PENDING_EVENT_LIMIT,
                ) {
                    Err(Self::coded_status(MacpError::StreamLagged, drain_err.message()))?;
                    break;
                }
                // One session, emitted and dropped before the next is asked
                // for, which is what keeps residency bounded.
                let Some(session) = sync.next_session().await else { break };
                synced.insert(session.session_id.clone());
                yield WatchSessionsResponse {
                    event: Some(SessionLifecycleEvent {
                        event_type: session_lifecycle_event::EventType::Created.into(),
                        session: Some(Self::session_to_metadata(&session)),
                        observed_at_unix_ms: session.started_at_unix_ms,
                    }),
                };
            }
            // Stream lifecycle transitions: first the ones buffered during the
            // sync (in bus order), then live ones.
            loop {
                let event = match pending.pop_front() {
                    Some(event) => event,
                    None => match rx.recv().await {
                        Ok(event) => event,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            Err(Self::coded_status(MacpError::StreamLagged, format!(
                                "WatchSessions receiver fell behind by {skipped} events"
                            )))?;
                            break;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                };
                let (event_type, sid) = match &event {
                    crate::runtime::SessionLifecycleEvent::Created { session_id } =>
                        (session_lifecycle_event::EventType::Created, session_id.clone()),
                    crate::runtime::SessionLifecycleEvent::Resolved { session_id } =>
                        (session_lifecycle_event::EventType::Resolved, session_id.clone()),
                    crate::runtime::SessionLifecycleEvent::Expired { session_id } =>
                        (session_lifecycle_event::EventType::Expired, session_id.clone()),
                    crate::runtime::SessionLifecycleEvent::Suspended { session_id } =>
                        (session_lifecycle_event::EventType::Suspended, session_id.clone()),
                    crate::runtime::SessionLifecycleEvent::Resumed { session_id } =>
                        (session_lifecycle_event::EventType::Resumed, session_id.clone()),
                    crate::runtime::SessionLifecycleEvent::Cancelled { session_id } =>
                        (session_lifecycle_event::EventType::Cancelled, session_id.clone()),
                };
                // Skip the buffered duplicate of an initial-sync entry;
                // non-Created events for synced sessions are new information
                // and pass through.
                //
                // `contains`, not `insert`: a live Created for a session the
                // sync never saw is emitted as-is and NOT recorded. Recording
                // it would be the only thing making this set grow with the
                // stream's lifetime, and it would buy nothing — `runtime.rs`
                // is the single `Created` publisher and sends once per session
                // start, so no live Created can repeat.
                if event_type == session_lifecycle_event::EventType::Created
                    && synced.contains(&sid)
                {
                    continue;
                }
                let session_meta = runtime.registry.get_session(&sid).await
                    .map(|s| Self::session_to_metadata(&s));
                yield WatchSessionsResponse {
                    event: Some(SessionLifecycleEvent {
                        event_type: event_type.into(),
                        session: session_meta,
                        observed_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                    }),
                };
            }
        };
        Ok(Response::new(Box::pin(stream)))
    }

    // Extension mode lifecycle RPCs

    async fn list_ext_modes(
        &self,
        _request: Request<ListExtModesRequest>,
    ) -> Result<Response<ListExtModesResponse>, Status> {
        Ok(Response::new(ListExtModesResponse {
            modes: self.runtime.extension_mode_descriptors(),
        }))
    }

    async fn register_ext_mode(
        &self,
        request: Request<RegisterExtModeRequest>,
    ) -> Result<Response<RegisterExtModeResponse>, Status> {
        let identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        self.security
            .authorize_mode_registry(&identity)
            .map_err(Self::status_from_error)?;
        let req = request.into_inner();
        let descriptor = req
            .mode_descriptor
            .ok_or_else(|| Self::coded_status(MacpError::InvalidRequest, "descriptor required"))?;
        match self.runtime.register_extension(descriptor) {
            Ok(()) => Ok(Response::new(RegisterExtModeResponse {
                ok: true,
                error: String::new(),
            })),
            Err(e) => Ok(Response::new(RegisterExtModeResponse {
                ok: false,
                error: e,
            })),
        }
    }

    async fn unregister_ext_mode(
        &self,
        request: Request<UnregisterExtModeRequest>,
    ) -> Result<Response<UnregisterExtModeResponse>, Status> {
        let identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        self.security
            .authorize_mode_registry(&identity)
            .map_err(Self::status_from_error)?;
        let req = request.into_inner();
        match self.runtime.unregister_extension(&req.mode) {
            Ok(()) => Ok(Response::new(UnregisterExtModeResponse {
                ok: true,
                error: String::new(),
            })),
            Err(e) => Ok(Response::new(UnregisterExtModeResponse {
                ok: false,
                error: e,
            })),
        }
    }

    async fn promote_mode(
        &self,
        request: Request<PromoteModeRequest>,
    ) -> Result<Response<PromoteModeResponse>, Status> {
        let identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        self.security
            .authorize_mode_registry(&identity)
            .map_err(Self::status_from_error)?;
        let req = request.into_inner();
        let new_name = if req.promoted_mode_name.is_empty() {
            None
        } else {
            Some(req.promoted_mode_name.as_str())
        };
        match self.runtime.promote_mode(&req.mode, new_name) {
            Ok(final_name) => Ok(Response::new(PromoteModeResponse {
                ok: true,
                error: String::new(),
                mode: final_name,
            })),
            Err(e) => Ok(Response::new(PromoteModeResponse {
                ok: false,
                error: e,
                mode: String::new(),
            })),
        }
    }

    // ── Governance policy lifecycle RPCs (RFC-MACP-0012) ────────────

    async fn register_policy(
        &self,
        request: Request<RegisterPolicyRequest>,
    ) -> Result<Response<RegisterPolicyResponse>, Status> {
        if self.policies_read_only {
            return Err(Self::coded_status(
                MacpError::RegistryReadOnly,
                "policy registry is read-only: policies are file-loaded via MACP_POLICIES_DIR",
            ));
        }
        let identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        self.security
            .authorize_mode_registry(&identity)
            .map_err(Self::status_from_error)?;
        let req = request.into_inner();
        let descriptor = req
            .policy_descriptor
            .ok_or_else(|| Self::coded_status(MacpError::InvalidRequest, "descriptor required"))?;
        let definition = Self::policy_descriptor_to_definition(&descriptor);
        match self.runtime.register_policy(definition) {
            Ok(()) => Ok(Response::new(RegisterPolicyResponse {
                ok: true,
                error: String::new(),
            })),
            Err(e) => Ok(Response::new(RegisterPolicyResponse {
                ok: false,
                error: e,
            })),
        }
    }

    async fn unregister_policy(
        &self,
        request: Request<UnregisterPolicyRequest>,
    ) -> Result<Response<UnregisterPolicyResponse>, Status> {
        if self.policies_read_only {
            return Err(Self::coded_status(
                MacpError::RegistryReadOnly,
                "policy registry is read-only: policies are file-loaded via MACP_POLICIES_DIR",
            ));
        }
        let identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        self.security
            .authorize_mode_registry(&identity)
            .map_err(Self::status_from_error)?;
        let req = request.into_inner();
        match self.runtime.unregister_policy(&req.policy_id) {
            Ok(()) => Ok(Response::new(UnregisterPolicyResponse {
                ok: true,
                error: String::new(),
            })),
            Err(e) => Ok(Response::new(UnregisterPolicyResponse {
                ok: false,
                error: e,
            })),
        }
    }

    /// RFC-MACP-0012 §8.1: replay a session's accepted history under a
    /// candidate policy, read-only. Authorized as `GetSession`; rate-limited
    /// as an ordinary message.
    async fn what_if_replay(
        &self,
        request: Request<WhatIfReplayRequest>,
    ) -> Result<Response<WhatIfReplayResponse>, Status> {
        let session_id = request.get_ref().session_id.clone();
        let identity = self
            .authenticate_session_access(&request, &session_id)
            .await?;
        self.security
            .enforce_rate_limit(&identity.sender, false)
            .await
            .map_err(Self::status_from_error)?;
        let descriptor = request
            .into_inner()
            .candidate
            .ok_or_else(|| Self::coded_status(MacpError::InvalidRequest, "candidate required"))?;
        let candidate = Self::policy_descriptor_to_definition(&descriptor);
        match self.runtime.what_if_replay(&session_id, &candidate).await {
            Ok(run) => Ok(Response::new(what_if::response(run))),
            Err(WhatIfError::UnknownSession) => Err(Self::coded_status(
                MacpError::UnknownSession,
                format!("Session '{}' not found", session_id),
            )),
            Err(WhatIfError::InvalidCandidate(msg)) => {
                Err(Self::coded_status(MacpError::InvalidPolicyDefinition, msg))
            }
            // RFC-MACP-0012 §8.1 item 8.
            Err(WhatIfError::Compacted) => Err(Self::coded_status(
                MacpError::HistoryUnavailable,
                format!(
                    "session '{session_id}' history was compacted when it ended and cannot be \
                     replayed; run the runtime with MACP_COMPACT_TERMINAL_LOGS=0 to keep it"
                ),
            )),
            Err(WhatIfError::Integrity(msg)) => {
                tracing::error!(session_id, error = %msg, "what-if recorded replay diverged");
                Err(Self::coded_status(
                    MacpError::StorageFailed,
                    format!("INTERNAL_ERROR: {msg}"),
                ))
            }
        }
    }

    async fn get_policy(
        &self,
        request: Request<GetPolicyRequest>,
    ) -> Result<Response<GetPolicyResponse>, Status> {
        let _identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        let req = request.into_inner();
        let policy = self.runtime.get_policy(&req.policy_id).ok_or_else(|| {
            Self::coded_status(
                MacpError::UnknownPolicyVersion,
                format!("Policy '{}' not found", req.policy_id),
            )
        })?;
        Ok(Response::new(GetPolicyResponse {
            policy_descriptor: Some(Self::policy_definition_to_descriptor(&policy)),
        }))
    }

    async fn list_policies(
        &self,
        request: Request<ListPoliciesRequest>,
    ) -> Result<Response<ListPoliciesResponse>, Status> {
        let _identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        let req = request.into_inner();
        let mode_filter = if req.mode.is_empty() {
            None
        } else {
            Some(req.mode.as_str())
        };
        let policies = self.runtime.list_policies(mode_filter);
        let descriptors = policies
            .iter()
            .map(Self::policy_definition_to_descriptor)
            .collect();
        Ok(Response::new(ListPoliciesResponse { descriptors }))
    }

    type WatchPoliciesStream = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<WatchPoliciesResponse, Status>> + Send>,
    >;

    async fn watch_policies(
        &self,
        _request: Request<WatchPoliciesRequest>,
    ) -> Result<Response<Self::WatchPoliciesStream>, Status> {
        let mut rx = self.runtime.subscribe_policy_changes();
        let runtime = Arc::clone(&self.runtime);
        let stream = async_stream::try_stream! {
            // Send initial state
            let policies = runtime.list_policies(None);
            let descriptors: Vec<PolicyDescriptor> = policies
                .iter()
                .map(MacpServer::policy_definition_to_descriptor)
                .collect();
            yield WatchPoliciesResponse {
                descriptors,
                observed_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            };
            // Wait for changes
            while rx.recv().await.is_ok() {
                let policies = runtime.list_policies(None);
                let descriptors: Vec<PolicyDescriptor> = policies
                    .iter()
                    .map(MacpServer::policy_definition_to_descriptor)
                    .collect();
                yield WatchPoliciesResponse {
                    descriptors,
                    observed_at_unix_ms: chrono::Utc::now().timestamp_millis(),
                };
            }
        };
        Ok(Response::new(Box::pin(stream)))
    }
}

// ── Policy type conversion helpers ──────────────────────────────────

impl MacpServer {
    fn policy_descriptor_to_definition(
        descriptor: &PolicyDescriptor,
    ) -> crate::policy::PolicyDefinition {
        let rules: serde_json::Value = if descriptor.rules.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&descriptor.rules).unwrap_or_else(|_| serde_json::json!({}))
        };
        crate::policy::PolicyDefinition {
            policy_id: descriptor.policy_id.clone(),
            mode: descriptor.mode.clone(),
            description: descriptor.description.clone(),
            rules,
            schema_version: descriptor.schema_version,
        }
    }

    fn policy_definition_to_descriptor(
        definition: &crate::policy::PolicyDefinition,
    ) -> PolicyDescriptor {
        PolicyDescriptor {
            policy_id: definition.policy_id.clone(),
            mode: definition.mode.clone(),
            description: definition.description.clone(),
            rules: serde_json::to_string(&definition.rules).unwrap_or_default(),
            schema_version: definition.schema_version,
            registered_at_unix_ms: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_store::LogStore;
    use crate::pb::what_if_entry_outcome::Kind;
    use crate::pb::SessionStartPayload;
    use crate::registry::SessionRegistry;
    use chrono::Utc;
    use prost::Message;

    /// S13: the server's status for every error is the registry's gRPC
    /// column, and every status names its registry code in metadata.
    #[test]
    fn status_from_error_follows_the_registry() {
        let cases = [
            (
                MacpError::PolicyDenied { reasons: vec![] },
                tonic::Code::PermissionDenied,
            ),
            (MacpError::Forbidden, tonic::Code::PermissionDenied),
            (MacpError::UnknownSession, tonic::Code::NotFound),
            (MacpError::UnknownPolicyVersion, tonic::Code::NotFound),
            (MacpError::SessionNotOpen, tonic::Code::FailedPrecondition),
            (MacpError::TtlExpired, tonic::Code::FailedPrecondition),
            (MacpError::DuplicateMessage, tonic::Code::AlreadyExists),
            (MacpError::InvalidEnvelope, tonic::Code::InvalidArgument),
            (MacpError::InvalidPayload, tonic::Code::InvalidArgument),
            (MacpError::UnknownMode, tonic::Code::InvalidArgument),
            (MacpError::InvalidMacpVersion, tonic::Code::InvalidArgument),
            (MacpError::RateLimited, tonic::Code::ResourceExhausted),
            (MacpError::StorageFailed, tonic::Code::Internal),
            (MacpError::Unauthenticated, tonic::Code::Unauthenticated),
            (MacpError::RegistryReadOnly, tonic::Code::FailedPrecondition),
            (MacpError::InvalidRequest, tonic::Code::InvalidArgument),
            (MacpError::StreamLagged, tonic::Code::ResourceExhausted),
            (MacpError::AgentNotFound, tonic::Code::NotFound),
            (
                MacpError::HistoryUnavailable,
                tonic::Code::FailedPrecondition,
            ),
        ];
        for (err, code) in cases {
            let name = err.error_code();
            let status = MacpServer::status_from_error(err);
            assert_eq!(status.code(), code, "{name}");
            assert_eq!(status.metadata().get("macp-error-code").unwrap(), name);
        }
    }

    /// RFC-MACP-0012 §6.2: POLICY_DENIED details carry structured denials
    /// (code + participants beside the message) and keep the legacy
    /// `reasons` string array.
    #[test]
    fn policy_denied_details_are_structured() {
        use macp_core::policy::{DenyCode, DenyReason};
        let err = MacpError::PolicyDenied {
            reasons: vec![
                DenyReason::new(DenyCode::VETO, "blocked by 1 blocking objection(s)")
                    .with_participants(["agent://b", "agent://a"]),
            ],
        };
        let details: serde_json::Value =
            serde_json::from_slice(&MacpServer::error_details_bytes(&err)).unwrap();
        assert_eq!(
            details,
            serde_json::json!({
                "reasons": ["blocked by 1 blocking objection(s)"],
                "denials": [{
                    "code": "veto",
                    "message": "blocked by 1 blocking objection(s)",
                    "participants": ["agent://a", "agent://b"],
                }],
            })
        );
        let status = MacpServer::status_from_error(err);
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
        let bin = status.metadata().get_bin("macp-error-details-bin").unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bin.to_bytes().unwrap()).unwrap(),
            details
        );
    }

    /// Stream classification is by error, not gRPC status: S13 moved
    /// INVALID_ENVELOPE/SESSION_NOT_FOUND onto codes that used to mean
    /// "terminate", and a mode validation failure must still stay inline.
    #[test]
    fn stream_rejections_stay_inline_under_the_registry_mapping() {
        for err in [
            MacpError::InvalidEnvelope,
            MacpError::InvalidPayload,
            MacpError::UnknownSession,
            MacpError::DuplicateMessage,
            MacpError::Forbidden,
            MacpError::SessionNotOpen,
            MacpError::PolicyDenied { reasons: vec![] },
        ] {
            assert!(!MacpServer::is_stream_terminal_error(&err), "{err:?}");
        }
        for err in [
            MacpError::Unauthenticated,
            MacpError::RateLimited,
            MacpError::PayloadTooLarge,
            MacpError::StorageFailed,
        ] {
            assert!(MacpServer::is_stream_terminal_error(&err), "{err:?}");
        }
        let inline = MacpServer::stream_inline_error(
            &MacpError::PolicyDenied {
                reasons: vec!["no votes cast".into()],
            },
            Some(&"sid".to_string()),
            "m1",
        );
        let Some(crate::pb::stream_session_response::Response::Error(e)) = inline.response else {
            panic!("expected an inline error");
        };
        // The registry code, not the status message (pre-0.9 bug).
        assert_eq!(e.code, "POLICY_DENIED");
        assert_eq!(e.message_id, "m1");
        assert!(!e.details.is_empty());
    }

    fn new_sid() -> String {
        uuid::Uuid::new_v4().as_hyphenated().to_string()
    }

    fn make_server() -> (MacpServer, Arc<Runtime>) {
        make_server_with_security(SecurityLayer::dev_mode())
    }

    /// Same harness, with the `SecurityLayer` supplied by the caller so a test
    /// can pin `list_sessions_{default,max}_page_size` without touching process
    /// env (which is not deterministic under `cargo test`'s thread pool).
    fn make_server_with_security(security: SecurityLayer) -> (MacpServer, Arc<Runtime>) {
        let storage: Arc<dyn crate::storage::StorageBackend> =
            Arc::new(crate::storage::MemoryBackend);
        let registry = Arc::new(SessionRegistry::new());
        let log_store = Arc::new(LogStore::new());
        let runtime = Arc::new(Runtime::new(storage, registry, log_store));
        let server = MacpServer::new(runtime.clone(), security);
        (server, runtime)
    }

    fn send_req(sender: &str, env: Envelope) -> Request<SendRequest> {
        let mut req = Request::new(SendRequest {
            envelope: Some(env),
        });
        req.metadata_mut()
            .insert("authorization", format!("Bearer {sender}").parse().unwrap());
        req
    }

    async fn do_send(server: &MacpServer, sender: &str, env: Envelope) -> Ack {
        let resp = server.send(send_req(sender, env)).await.unwrap();
        resp.into_inner().ack.unwrap()
    }

    fn start_payload() -> Vec<u8> {
        SessionStartPayload {
            intent: "intent".into(),
            participants: vec!["agent://fraud".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 1000,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec()
    }

    #[tokio::test]
    async fn sender_is_derived_from_authenticated_metadata() {
        let (server, runtime) = make_server();
        let sid = new_sid();
        let ack = do_send(
            &server,
            "agent://orchestrator",
            Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "SessionStart".into(),
                message_id: "m1".into(),
                session_id: sid.clone(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: start_payload(),
            },
        )
        .await;
        assert!(ack.ok);
        let session = runtime.get_session_checked(&sid).await.unwrap();
        assert_eq!(session.initiator_sender, "agent://orchestrator");
    }

    #[tokio::test]
    async fn spoofed_sender_is_rejected() {
        let (server, _) = make_server();
        let sid = new_sid();
        let ack = do_send(
            &server,
            "agent://orchestrator",
            Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "SessionStart".into(),
                message_id: "m1".into(),
                session_id: sid,
                sender: "agent://spoof".into(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: start_payload(),
            },
        )
        .await;
        assert!(!ack.ok);
        assert_eq!(ack.error.as_ref().unwrap().code, "UNAUTHENTICATED");
    }

    #[tokio::test]
    async fn get_session_requires_session_membership() {
        let (server, _) = make_server();
        let sid = new_sid();
        let ack = do_send(
            &server,
            "agent://orchestrator",
            Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "SessionStart".into(),
                message_id: "m1".into(),
                session_id: sid.clone(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: start_payload(),
            },
        )
        .await;
        assert!(ack.ok);

        let mut req = Request::new(GetSessionRequest { session_id: sid });
        req.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", "agent://outsider").parse().unwrap(),
        );
        let err = server.get_session(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn register_ext_mode_requires_authenticated_registry_permission() {
        let storage: Arc<dyn crate::storage::StorageBackend> =
            Arc::new(crate::storage::MemoryBackend);
        let registry = Arc::new(SessionRegistry::new());
        let log_store = Arc::new(LogStore::new());
        let runtime = Arc::new(Runtime::new(storage, registry, log_store));
        let security = SecurityLayer::from_env().unwrap_or_else(|_| SecurityLayer::dev_mode());
        let server = MacpServer::new(runtime, security);

        let req = Request::new(RegisterExtModeRequest {
            mode_descriptor: Some(crate::pb::ModeDescriptor {
                mode: "ext.custom.v1".into(),
                mode_version: "1.0.0".into(),
                message_types: vec!["SessionStart".into(), "Commitment".into()],
                ..Default::default()
            }),
        });
        let err = server.register_ext_mode(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    fn stream_identity(sender: &str) -> AuthIdentity {
        AuthIdentity {
            sender: sender.into(),
            allowed_modes: None,
            can_start_sessions: true,
            max_open_sessions: None,
            can_manage_mode_registry: false,
            is_observer: false,
        }
    }

    #[tokio::test]
    async fn stream_session_emits_accepted_envelopes_only() {
        use tokio_stream::{iter, StreamExt};

        let (server, _) = make_server();
        let sid = new_sid();
        let requests = iter(vec![Ok(StreamSessionRequest {
            subscribe_session_id: String::new(),
            after_sequence: 0,
            envelope: Some(Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "SessionStart".into(),
                message_id: "m1".into(),
                session_id: sid.clone(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: start_payload(),
            }),
        })]);

        let mut stream =
            server.build_stream_session_stream(stream_identity("agent://orchestrator"), requests);

        let response = stream.next().await.unwrap().unwrap();
        let envelope = match response.response.unwrap() {
            crate::pb::stream_session_response::Response::Envelope(e) => e,
            _ => panic!("expected envelope"),
        };
        assert_eq!(envelope.message_type, "SessionStart");
        assert_eq!(envelope.message_id, "m1");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn stream_session_rejects_mixed_session_ids() {
        use tokio_stream::{iter, StreamExt};

        let (server, _) = make_server();
        let sid1 = new_sid();
        let sid2 = new_sid();
        let requests = iter(vec![
            Ok(StreamSessionRequest {
                subscribe_session_id: String::new(),
                after_sequence: 0,
                envelope: Some(Envelope {
                    macp_version: "1.0".into(),
                    mode: "macp.mode.decision.v1".into(),
                    message_type: "SessionStart".into(),
                    message_id: "m1".into(),
                    session_id: sid1.clone(),
                    sender: String::new(),
                    timestamp_unix_ms: Utc::now().timestamp_millis(),
                    payload: start_payload(),
                }),
            }),
            Ok(StreamSessionRequest {
                subscribe_session_id: String::new(),
                after_sequence: 0,
                envelope: Some(Envelope {
                    macp_version: "1.0".into(),
                    mode: "macp.mode.decision.v1".into(),
                    message_type: "SessionStart".into(),
                    message_id: "m2".into(),
                    session_id: sid2,
                    sender: String::new(),
                    timestamp_unix_ms: Utc::now().timestamp_millis(),
                    payload: start_payload(),
                }),
            }),
        ]);

        let mut stream =
            server.build_stream_session_stream(stream_identity("agent://orchestrator"), requests);

        let first = stream.next().await.unwrap().unwrap();
        let first_env = match first.response.unwrap() {
            crate::pb::stream_session_response::Response::Envelope(e) => e,
            _ => panic!("expected envelope"),
        };
        assert_eq!(first_env.session_id, sid1);
        let err = stream.next().await.unwrap().unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn list_modes_returns_standard_modes() {
        let (server, _) = make_server();
        let resp = server
            .list_modes(Request::new(ListModesRequest {}))
            .await
            .unwrap();
        let names: Vec<String> = resp
            .into_inner()
            .modes
            .iter()
            .map(|m| m.mode.clone())
            .collect();
        assert_eq!(names.len(), 5);
        assert!(names.contains(&"macp.mode.decision.v1".to_string()));
        assert!(names.contains(&"macp.mode.proposal.v1".to_string()));
        assert!(names.contains(&"macp.mode.task.v1".to_string()));
        assert!(names.contains(&"macp.mode.handoff.v1".to_string()));
        assert!(names.contains(&"macp.mode.quorum.v1".to_string()));
        // multi_round is now an extension, not in ListModes
        assert!(!names.contains(&"ext.multi_round.v1".to_string()));
    }

    #[tokio::test]
    async fn list_ext_modes_returns_extensions() {
        let (server, _) = make_server();
        let resp = server
            .list_ext_modes(Request::new(ListExtModesRequest {}))
            .await
            .unwrap();
        let names: Vec<String> = resp
            .into_inner()
            .modes
            .iter()
            .map(|m| m.mode.clone())
            .collect();
        assert_eq!(names.len(), 1);
        assert!(names.contains(&"ext.multi_round.v1".to_string()));
    }

    #[tokio::test]
    async fn get_manifest_includes_all_modes() {
        let (server, _) = make_server();
        let resp = server
            .get_manifest(Request::new(crate::pb::GetManifestRequest {
                agent_id: String::new(),
            }))
            .await
            .unwrap();
        let manifest = resp.into_inner().manifest.unwrap();
        assert_eq!(manifest.supported_modes.len(), 6);
        assert!(manifest
            .supported_modes
            .contains(&"ext.multi_round.v1".to_string()));
    }

    #[tokio::test]
    async fn get_session_returns_metadata() {
        let (server, _) = make_server();
        let sid = new_sid();
        let ack = do_send(
            &server,
            "agent://orchestrator",
            Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "SessionStart".into(),
                message_id: "m1".into(),
                session_id: sid.clone(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: start_payload(),
            },
        )
        .await;
        assert!(ack.ok);

        let mut req = Request::new(GetSessionRequest {
            session_id: sid.clone(),
        });
        req.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", "agent://orchestrator")
                .parse()
                .unwrap(),
        );
        let resp = server.get_session(req).await.unwrap();
        let meta = resp.into_inner().metadata.unwrap();
        assert_eq!(meta.session_id, sid);
        assert_eq!(meta.mode, "macp.mode.decision.v1");
        assert_eq!(meta.mode_version, "1.0.0");
        assert_eq!(meta.configuration_version, "cfg-1");
    }

    #[tokio::test]
    async fn cancel_session_transitions_to_cancelled() {
        let (server, _) = make_server();
        let sid = new_sid();
        let ack = do_send(
            &server,
            "agent://orchestrator",
            Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "SessionStart".into(),
                message_id: "m1".into(),
                session_id: sid.clone(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: start_payload(),
            },
        )
        .await;
        assert!(ack.ok);

        let mut req = Request::new(CancelSessionRequest {
            session_id: sid,
            reason: "no longer needed".into(),
        });
        req.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", "agent://orchestrator")
                .parse()
                .unwrap(),
        );
        let resp = server.cancel_session(req).await.unwrap();
        let ack = resp.into_inner().ack.unwrap();
        assert!(ack.ok);
        // RFC-MACP-0001 §7.3: cancellation now yields the distinct CANCELLED state.
        assert_eq!(ack.session_state, PbSessionState::Cancelled as i32);
    }

    #[tokio::test]
    async fn participant_cannot_cancel_session() {
        let (server, _) = make_server();
        let sid = new_sid();
        let ack = do_send(
            &server,
            "agent://orchestrator",
            Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "SessionStart".into(),
                message_id: "m1".into(),
                session_id: sid.clone(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: start_payload(),
            },
        )
        .await;
        assert!(ack.ok);

        let mut req = Request::new(CancelSessionRequest {
            session_id: sid,
            reason: "I want to cancel".into(),
        });
        req.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", "agent://fraud").parse().unwrap(),
        );
        let err = server.cancel_session(req).await.unwrap_err();
        assert_coded(&err, "FORBIDDEN", tonic::Code::PermissionDenied);
    }

    /// Every control-plane rejection names its registry code in
    /// `macp-error-code`, exactly like `Send` (SDK report: these used to be
    /// bare statuses a client could only branch on by message text).
    fn assert_coded(status: &Status, code: &str, grpc: tonic::Code) {
        assert_eq!(status.code(), grpc, "{code}: {status:?}");
        assert_eq!(
            status
                .metadata()
                .get("macp-error-code")
                .unwrap_or_else(|| panic!("{code}: no macp-error-code on {status:?}")),
            code
        );
        let row = macp_core::error::registry_entry(code).expect("registry row");
        assert_eq!(status.code() as i32, row.grpc.grpc_code(), "{code}");
    }

    fn authed<T>(msg: T) -> Request<T> {
        let mut req = Request::new(msg);
        req.metadata_mut().insert(
            "authorization",
            "Bearer agent://orchestrator".parse().unwrap(),
        );
        req
    }

    /// RFC-MACP-0012 §4 / `policy.proto` field 4: Initialize advertises the
    /// vendor-extension keys the bound policy registry admits, sorted, and
    /// none when the registry has none.
    #[tokio::test]
    async fn initialize_advertises_the_registrys_rule_extensions() {
        struct AnyValue;
        impl macp_core::policy::RuleExtension for AnyValue {
            fn validate(&self, _: &serde_json::Value) -> Result<(), String> {
                Ok(())
            }
        }
        let advertised = |server: MacpServer| async move {
            server
                .initialize(Request::new(InitializeRequest {
                    supported_protocol_versions: vec!["1.0".into()],
                    ..Default::default()
                }))
                .await
                .unwrap()
                .into_inner()
                .capabilities
                .unwrap()
                .policy_registry
                .unwrap()
                .rule_extensions
        };

        let (server, _) = make_server();
        assert!(advertised(server).await.is_empty());

        let ext = || Arc::new(AnyValue) as Arc<dyn macp_core::policy::RuleExtension>;
        let policies = crate::policy::registry::PolicyRegistry::new()
            .with_rule_extensions([
                ("x-org.zeta".to_string(), ext()),
                ("x-com.alpha".to_string(), ext()),
            ])
            .unwrap();
        let runtime = Arc::new(Runtime::with_registries(
            Arc::new(crate::storage::MemoryBackend),
            Arc::new(SessionRegistry::new()),
            Arc::new(LogStore::new()),
            Arc::new(crate::mode_registry::ModeRegistry::build_default(Arc::new(
                macp_policy::DefaultPolicyEvaluator,
            ))),
            Arc::new(policies),
        ));
        let server = MacpServer::new(runtime, SecurityLayer::dev_mode());
        assert_eq!(advertised(server).await, ["x-com.alpha", "x-org.zeta"]);
    }

    /// RFC-MACP-0004 §4.1 / RFC-MACP-0006 §3.4: dev-mode auth takes the
    /// bearer value as the sender, which is self-assertion, not
    /// authentication — so every Signal it forwards is marked
    /// `sender_authenticated = false`. (The `true` side needs configured
    /// auth; `tier1_jwt` pins it through the real binary.)
    #[tokio::test]
    async fn dev_mode_signals_are_marked_unauthenticated() {
        let (server, _) = make_server();
        let mut stream = server
            .watch_signals(authed(WatchSignalsRequest {}))
            .await
            .unwrap()
            .into_inner();
        let signal = Envelope {
            macp_version: "1.0".into(),
            message_type: "Signal".into(),
            message_id: "sig-1".into(),
            payload: crate::pb::SignalPayload {
                signal_type: "progress".into(),
                ..Default::default()
            }
            .encode_to_vec(),
            ..Default::default()
        };
        assert!(do_send(&server, "agent://worker", signal).await.ok);
        let delivered = tokio_stream::StreamExt::next(&mut stream)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered.envelope.unwrap().message_id, "sig-1");
        assert!(!delivered.sender_authenticated);
    }

    #[tokio::test]
    async fn control_plane_rejections_carry_registry_codes() {
        let (server, _) = make_server();

        let err = server
            .get_session(authed(GetSessionRequest {
                session_id: "nonexistent".into(),
            }))
            .await
            .unwrap_err();
        assert_coded(&err, "SESSION_NOT_FOUND", tonic::Code::NotFound);

        let err = server
            .list_sessions(authed(crate::pb::ListSessionsRequest {
                page_size: -1,
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_coded(&err, "INVALID_REQUEST", tonic::Code::InvalidArgument);

        let err = server
            .list_sessions(authed(crate::pb::ListSessionsRequest {
                page_token: "!!not-a-token!!".into(),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_coded(&err, "INVALID_REQUEST", tonic::Code::InvalidArgument);

        let err = server
            .get_manifest(authed(crate::pb::GetManifestRequest {
                agent_id: "someone-else".into(),
            }))
            .await
            .unwrap_err();
        assert_coded(&err, "AGENT_NOT_FOUND", tonic::Code::NotFound);

        let err = server
            .get_policy(authed(crate::pb::GetPolicyRequest {
                policy_id: "policy.nope".into(),
            }))
            .await
            .unwrap_err();
        assert_coded(&err, "UNKNOWN_POLICY_VERSION", tonic::Code::NotFound);

        let err = server
            .register_policy(authed(crate::pb::RegisterPolicyRequest {
                policy_descriptor: None,
            }))
            .await
            .unwrap_err();
        assert_coded(&err, "INVALID_REQUEST", tonic::Code::InvalidArgument);

        let err = server
            .initialize(Request::new(InitializeRequest::default()))
            .await
            .unwrap_err();
        assert_coded(&err, "INVALID_REQUEST", tonic::Code::InvalidArgument);

        let err = server
            .initialize(Request::new(InitializeRequest {
                supported_protocol_versions: vec!["9.9".into()],
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_coded(
            &err,
            "UNSUPPORTED_PROTOCOL_VERSION",
            tonic::Code::InvalidArgument,
        );

        let (server, _) = make_server();
        let server = server.with_read_only_policies();
        let err = server
            .register_policy(authed(crate::pb::RegisterPolicyRequest {
                policy_descriptor: None,
            }))
            .await
            .unwrap_err();
        assert_coded(&err, "REGISTRY_READ_ONLY", tonic::Code::FailedPrecondition);
        let err = server
            .unregister_policy(authed(crate::pb::UnregisterPolicyRequest {
                policy_id: "policy.default".into(),
            }))
            .await
            .unwrap_err();
        assert_coded(&err, "REGISTRY_READ_ONLY", tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn cancel_session_unknown_session_returns_error() {
        let (server, _) = make_server();
        let mut req = Request::new(CancelSessionRequest {
            session_id: "nonexistent".into(),
            reason: "test".into(),
        });
        req.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", "agent://orchestrator")
                .parse()
                .unwrap(),
        );
        let err = server.cancel_session(req).await.unwrap_err();
        assert_coded(&err, "SESSION_NOT_FOUND", tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn ambient_signal_accepted() {
        let (server, _) = make_server();
        let ack = do_send(
            &server,
            "agent://orchestrator",
            Envelope {
                macp_version: "1.0".into(),
                mode: String::new(),
                message_type: "Signal".into(),
                message_id: "sig-1".into(),
                session_id: String::new(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: vec![],
            },
        )
        .await;
        assert!(ack.ok);
    }

    #[tokio::test]
    async fn signal_with_session_id_rejected() {
        let (server, _) = make_server();
        let ack = do_send(
            &server,
            "agent://orchestrator",
            Envelope {
                macp_version: "1.0".into(),
                mode: String::new(),
                message_type: "Signal".into(),
                message_id: "sig-2".into(),
                session_id: "some-session".into(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: vec![],
            },
        )
        .await;
        assert!(!ack.ok);
        assert_eq!(ack.error.as_ref().unwrap().code, "INVALID_ENVELOPE");
    }

    #[tokio::test]
    async fn signal_with_mode_rejected() {
        let (server, _) = make_server();
        let ack = do_send(
            &server,
            "agent://orchestrator",
            Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "Signal".into(),
                message_id: "sig-3".into(),
                session_id: String::new(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: vec![],
            },
        )
        .await;
        assert!(!ack.ok);
        assert_eq!(ack.error.as_ref().unwrap().code, "INVALID_ENVELOPE");
    }

    #[tokio::test]
    async fn ambient_progress_accepted() {
        let (server, _) = make_server();
        let ack = do_send(
            &server,
            "agent://orchestrator",
            Envelope {
                macp_version: "1.0".into(),
                mode: String::new(),
                message_type: "Progress".into(),
                message_id: "prog-1".into(),
                session_id: String::new(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: vec![],
            },
        )
        .await;
        assert!(ack.ok);
    }

    #[tokio::test]
    async fn ambient_progress_with_mode_rejected() {
        let (server, _) = make_server();
        let ack = do_send(
            &server,
            "agent://orchestrator",
            Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "Progress".into(),
                message_id: "prog-2".into(),
                session_id: String::new(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: vec![],
            },
        )
        .await;
        assert!(!ack.ok);
        assert_eq!(ack.error.as_ref().unwrap().code, "INVALID_ENVELOPE");
    }

    #[tokio::test]
    async fn manifest_advertises_stream_enabled() {
        let (server, _) = make_server();
        let resp = server
            .initialize(Request::new(InitializeRequest {
                supported_protocol_versions: vec!["1.0".into()],
                client_info: None,
                capabilities: None,
            }))
            .await
            .unwrap();
        let caps = resp.into_inner().capabilities.unwrap();
        assert!(caps.sessions.unwrap().stream);
    }

    #[tokio::test]
    async fn initialize_empty_versions_rejected() {
        let (server, _) = make_server();
        let err = server
            .initialize(Request::new(InitializeRequest {
                supported_protocol_versions: vec![],
                client_info: None,
                capabilities: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn initialize_unsupported_version_rejected() {
        let (server, _) = make_server();
        let err = server
            .initialize(Request::new(InitializeRequest {
                supported_protocol_versions: vec!["2.0".into()],
                client_info: None,
                capabilities: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            err.metadata().get("macp-error-code").unwrap(),
            "UNSUPPORTED_PROTOCOL_VERSION"
        );
    }

    // Regression test for issue #198: the served `instructions` string once
    // advertised `x-macp-agent-id`, a header no non-test code path reads.
    // Pinned against both directions so a future edit can't silently swap
    // one wrong claim for another.
    #[tokio::test]
    async fn initialize_instructions_do_not_advertise_removed_header() {
        let (server, _) = make_server();
        let resp = server
            .initialize(Request::new(InitializeRequest {
                supported_protocol_versions: vec!["1.0".into()],
                client_info: None,
                capabilities: None,
            }))
            .await
            .unwrap();
        let instructions = resp.into_inner().instructions;
        assert!(
            !instructions.contains("x-macp-agent-id"),
            "instructions must not advertise the removed x-macp-agent-id header: {instructions:?}"
        );
        assert!(
            instructions.contains("MACP_ALLOW_INSECURE"),
            "instructions must describe the real dev-mode auth path: {instructions:?}"
        );
    }

    // ── RFC-MACP-0006-A1: passive subscribe tests ──────────────────────

    fn observer_identity(sender: &str) -> AuthIdentity {
        AuthIdentity {
            sender: sender.into(),
            allowed_modes: None,
            can_start_sessions: false,
            max_open_sessions: None,
            can_manage_mode_registry: false,
            is_observer: true,
        }
    }

    fn subscribe_frame(session_id: &str, after: u64) -> StreamSessionRequest {
        StreamSessionRequest {
            subscribe_session_id: session_id.into(),
            after_sequence: after,
            envelope: None,
        }
    }

    fn start_multi_participant(participants: Vec<String>) -> Vec<u8> {
        SessionStartPayload {
            intent: "intent".into(),
            participants,
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 60_000,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec()
    }

    async fn start_session(
        server: &MacpServer,
        initiator: &str,
        sid: &str,
        participants: Vec<String>,
    ) {
        let ack = do_send(
            server,
            initiator,
            Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "SessionStart".into(),
                message_id: "start".into(),
                session_id: sid.into(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: start_multi_participant(participants),
            },
        )
        .await;
        assert!(ack.ok, "SessionStart failed: {:?}", ack.error);
    }

    async fn send_proposal(
        server: &MacpServer,
        sender: &str,
        sid: &str,
        message_id: &str,
        proposal_id: &str,
    ) {
        let payload = crate::decision_pb::ProposalPayload {
            proposal_id: proposal_id.into(),
            option: "opt".into(),
            rationale: "r".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();
        let ack = do_send(
            server,
            sender,
            Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "Proposal".into(),
                message_id: message_id.into(),
                session_id: sid.into(),
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload,
            },
        )
        .await;
        assert!(ack.ok, "Proposal failed: {:?}", ack.error);
    }

    #[tokio::test]
    async fn subscribe_replays_session_history_from_zero() {
        let (server, _) = make_server();
        let sid = new_sid();
        let initiator = "agent://orchestrator";
        let peer = "agent://fraud";
        start_session(
            &server,
            initiator,
            &sid,
            vec![initiator.into(), peer.into()],
        )
        .await;
        send_proposal(&server, peer, &sid, "m2", "p1").await;

        let mut bound = None;
        let mut events = None;
        let replay = server
            .process_stream_request(
                &stream_identity(peer),
                subscribe_frame(&sid, 0),
                &mut bound,
                &mut events,
            )
            .await
            .unwrap();

        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].message_type, "SessionStart");
        assert_eq!(replay[0].message_id, "start");
        assert_eq!(replay[1].message_type, "Proposal");
        assert_eq!(replay[1].message_id, "m2");
        assert_eq!(bound.as_deref(), Some(sid.as_str()));
        assert!(events.is_some());
    }

    #[tokio::test]
    async fn subscribe_after_sequence_filters_history() {
        let (server, _) = make_server();
        let sid = new_sid();
        let initiator = "agent://orchestrator";
        let peer = "agent://fraud";
        start_session(
            &server,
            initiator,
            &sid,
            vec![initiator.into(), peer.into()],
        )
        .await;
        send_proposal(&server, peer, &sid, "m2", "p1").await;
        send_proposal(&server, peer, &sid, "m3", "p2").await;

        let mut bound = None;
        let mut events = None;
        let replay = server
            .process_stream_request(
                &stream_identity(peer),
                subscribe_frame(&sid, 2),
                &mut bound,
                &mut events,
            )
            .await
            .unwrap();

        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].message_id, "m3");
    }

    #[tokio::test]
    async fn subscribe_unknown_session_returns_not_found() {
        let (server, _) = make_server();
        let mut bound = None;
        let mut events = None;
        let status = server
            .process_stream_request(
                &stream_identity("agent://orchestrator"),
                subscribe_frame("missing-session", 0),
                &mut bound,
                &mut events,
            )
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::NotFound);
        assert!(bound.is_none());
        assert!(events.is_none());
    }

    #[tokio::test]
    async fn subscribe_non_participant_is_forbidden() {
        let (server, _) = make_server();
        let sid = new_sid();
        start_session(
            &server,
            "agent://orchestrator",
            &sid,
            vec!["agent://orchestrator".into(), "agent://fraud".into()],
        )
        .await;

        let mut bound = None;
        let mut events = None;
        let status = server
            .process_stream_request(
                &stream_identity("agent://outsider"),
                subscribe_frame(&sid, 0),
                &mut bound,
                &mut events,
            )
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn subscribe_observer_identity_allowed() {
        let (server, _) = make_server();
        let sid = new_sid();
        start_session(
            &server,
            "agent://orchestrator",
            &sid,
            vec!["agent://orchestrator".into(), "agent://fraud".into()],
        )
        .await;

        let mut bound = None;
        let mut events = None;
        let replay = server
            .process_stream_request(
                &observer_identity("agent://auditor"),
                subscribe_frame(&sid, 0),
                &mut bound,
                &mut events,
            )
            .await
            .unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].message_type, "SessionStart");
    }

    #[tokio::test]
    async fn subscribe_initiator_allowed_even_when_not_listed() {
        // Per RFC-MACP-0007, the initiator is always authorized for session
        // access, even if not present in the participants list.
        let (server, _) = make_server();
        let sid = new_sid();
        start_session(
            &server,
            "agent://orchestrator",
            &sid,
            vec!["agent://fraud".into()],
        )
        .await;

        let mut bound = None;
        let mut events = None;
        let replay = server
            .process_stream_request(
                &stream_identity("agent://orchestrator"),
                subscribe_frame(&sid, 0),
                &mut bound,
                &mut events,
            )
            .await
            .unwrap();
        assert_eq!(replay.len(), 1);
    }

    #[tokio::test]
    async fn stream_request_with_envelope_and_subscribe_is_rejected() {
        let (server, _) = make_server();
        let sid = new_sid();
        let req = StreamSessionRequest {
            subscribe_session_id: sid.clone(),
            after_sequence: 0,
            envelope: Some(Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "SessionStart".into(),
                message_id: "m1".into(),
                session_id: sid,
                sender: String::new(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: start_payload(),
            }),
        };

        let mut bound = None;
        let mut events = None;
        let status = server
            .process_stream_request(
                &stream_identity("agent://orchestrator"),
                req,
                &mut bound,
                &mut events,
            )
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn subscribe_to_different_session_on_bound_stream_is_rejected() {
        let (server, _) = make_server();
        let sid1 = new_sid();
        let sid2 = new_sid();
        start_session(
            &server,
            "agent://orchestrator",
            &sid1,
            vec!["agent://orchestrator".into(), "agent://fraud".into()],
        )
        .await;
        start_session(
            &server,
            "agent://orchestrator",
            &sid2,
            vec!["agent://orchestrator".into(), "agent://fraud".into()],
        )
        .await;

        // First subscribe binds the stream to sid1
        let identity = stream_identity("agent://fraud");
        let mut bound = None;
        let mut events = None;
        server
            .process_stream_request(
                &identity,
                subscribe_frame(&sid1, 0),
                &mut bound,
                &mut events,
            )
            .await
            .unwrap();
        assert_eq!(bound.as_deref(), Some(sid1.as_str()));

        // Second subscribe to sid2 on the same stream must be rejected
        let status = server
            .process_stream_request(
                &identity,
                subscribe_frame(&sid2, 0),
                &mut bound,
                &mut events,
            )
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    /// E3: an injected ingress engine gates session start, messages, and
    /// session reads — deny-one-sender double proves all three hooks fire and
    /// that denial surfaces as POLICY_DENIED / PermissionDenied (fail closed).
    struct DenySenderEngine {
        denied: String,
    }

    #[async_trait::async_trait]
    impl crate::policy_engine::PolicyEngine for DenySenderEngine {
        async fn evaluate_session_start(
            &self,
            identity: &crate::security::AuthIdentity,
            _mode: &str,
            _env: &Envelope,
        ) -> macp_core::policy::PolicyDecision {
            if identity.sender == self.denied {
                macp_core::policy::PolicyDecision::Deny {
                    reasons: vec!["sender embargoed".into()],
                }
            } else {
                macp_core::policy::PolicyDecision::Allow { reasons: vec![] }
            }
        }

        async fn evaluate_message(
            &self,
            identity: &crate::security::AuthIdentity,
            _session: &macp_core::session::Session,
            _env: &Envelope,
        ) -> macp_core::policy::PolicyDecision {
            if identity.sender == self.denied {
                macp_core::policy::PolicyDecision::Deny {
                    reasons: vec!["sender embargoed".into()],
                }
            } else {
                macp_core::policy::PolicyDecision::Allow { reasons: vec![] }
            }
        }

        async fn evaluate_session_access(
            &self,
            identity: &crate::security::AuthIdentity,
            _session: &macp_core::session::Session,
        ) -> macp_core::policy::PolicyDecision {
            if identity.sender == self.denied {
                macp_core::policy::PolicyDecision::Deny {
                    reasons: vec!["sender embargoed".into()],
                }
            } else {
                macp_core::policy::PolicyDecision::Allow { reasons: vec![] }
            }
        }
    }

    #[tokio::test]
    async fn policy_engine_gates_all_three_ingress_points() {
        let (server, _runtime) = make_server();
        let server = server.with_policy_engine(Arc::new(DenySenderEngine {
            denied: "agent://embargoed".into(),
        }));

        let sid = new_sid();
        let start_payload = SessionStartPayload {
            intent: "e3".into(),
            participants: vec!["agent://ok".into(), "agent://embargoed".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 60_000,
            context_id: String::new(),
            extensions: Default::default(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec();
        let start_env = |sender: &str, sid: &str| Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.decision.v1".into(),
            message_type: "SessionStart".into(),
            message_id: new_sid(),
            session_id: sid.into(),
            sender: sender.into(),
            timestamp_unix_ms: Utc::now().timestamp_millis(),
            payload: start_payload.clone(),
        };

        // 1. Embargoed sender cannot start a session.
        let ack = server
            .send(send_req(
                "agent://embargoed",
                start_env("agent://embargoed", &sid),
            ))
            .await
            .unwrap()
            .into_inner()
            .ack
            .unwrap();
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, "POLICY_DENIED");

        // Allowed sender starts it.
        let ack = server
            .send(send_req("agent://ok", start_env("agent://ok", &sid)))
            .await
            .unwrap()
            .into_inner()
            .ack
            .unwrap();
        assert!(ack.ok, "allowed sender must start: {:?}", ack.error);

        // 2. Embargoed sender cannot send into the session.
        let proposal = crate::decision_pb::ProposalPayload {
            proposal_id: "p1".into(),
            option: "x".into(),
            rationale: "r".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();
        let msg_env = Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.decision.v1".into(),
            message_type: "Proposal".into(),
            message_id: new_sid(),
            session_id: sid.clone(),
            sender: "agent://embargoed".into(),
            timestamp_unix_ms: Utc::now().timestamp_millis(),
            payload: proposal,
        };
        let ack = server
            .send(send_req("agent://embargoed", msg_env))
            .await
            .unwrap()
            .into_inner()
            .ack
            .unwrap();
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, "POLICY_DENIED");

        // 3. Embargoed sender cannot read the session.
        let mut req = Request::new(crate::pb::GetSessionRequest {
            session_id: sid.clone(),
        });
        req.metadata_mut()
            .insert("authorization", "Bearer agent://embargoed".parse().unwrap());
        let err = server
            .get_session(req)
            .await
            .expect_err("embargoed read must be denied");
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    /// E3 transport-parity: the ingress engine gates the STREAM path too — a
    /// denied sender must not be able to bypass the engine by switching from
    /// unary Send to StreamSession (envelope frames or subscribe frames).
    #[tokio::test]
    async fn policy_engine_gates_stream_path() {
        let (server, runtime) = make_server();
        let server = server.with_policy_engine(Arc::new(DenySenderEngine {
            denied: "agent://embargoed".into(),
        }));

        // Session started by an allowed sender (participants include the
        // embargoed agent so built-in membership checks pass — only the
        // engine denies it).
        let sid = new_sid();
        let payload = SessionStartPayload {
            intent: "e3-stream".into(),
            participants: vec!["agent://ok".into(), "agent://embargoed".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 60_000,
            context_id: String::new(),
            extensions: Default::default(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec();
        runtime
            .process(
                &Envelope {
                    macp_version: "1.0".into(),
                    mode: "macp.mode.decision.v1".into(),
                    message_type: "SessionStart".into(),
                    message_id: new_sid(),
                    session_id: sid.clone(),
                    sender: "agent://ok".into(),
                    timestamp_unix_ms: Utc::now().timestamp_millis(),
                    payload,
                },
                None,
            )
            .await
            .unwrap();

        let embargoed = crate::security::AuthIdentity {
            sender: "agent://embargoed".into(),
            allowed_modes: None,
            can_start_sessions: true,
            max_open_sessions: None,
            can_manage_mode_registry: false,
            is_observer: false,
        };
        let mut bound = None;
        let mut events = None;

        // 1. Stream envelope frame from the embargoed sender: denied.
        let proposal = crate::decision_pb::ProposalPayload {
            proposal_id: "p1".into(),
            option: "x".into(),
            rationale: "r".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();
        let req = StreamSessionRequest {
            envelope: Some(Envelope {
                macp_version: "1.0".into(),
                mode: "macp.mode.decision.v1".into(),
                message_type: "Proposal".into(),
                message_id: new_sid(),
                session_id: sid.clone(),
                sender: "agent://embargoed".into(),
                timestamp_unix_ms: Utc::now().timestamp_millis(),
                payload: proposal,
            }),
            subscribe_session_id: String::new(),
            after_sequence: 0,
        };
        let err = server
            .process_stream_request(&embargoed, req, &mut bound, &mut events)
            .await
            .expect_err("stream envelope from embargoed sender must be denied");
        // A per-envelope rejection (inline MACPError on the stream, same
        // POLICY_DENIED the unary path acks) — not a stream-ending status.
        // Its registry status is PERMISSION_DENIED (S13).
        assert!(
            matches!(
                &err,
                StreamFailure::Rejected {
                    err: MacpError::PolicyDenied { .. },
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");

        // 2. Passive-subscribe frame (history read) from the embargoed
        //    sender: denied even though membership would allow it.
        let req = StreamSessionRequest {
            envelope: None,
            subscribe_session_id: sid.clone(),
            after_sequence: 0,
        };
        let err = server
            .process_stream_request(&embargoed, req, &mut bound, &mut events)
            .await
            .expect_err("stream subscribe from embargoed sender must be denied");
        assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");
    }
    // ── ListSessions pagination (core.proto:411-426) ───────────────────

    fn paged_session(id: &str) -> crate::session::Session {
        crate::session::Session::builder(id, "macp.mode.decision.v1", "agent://initiator")
            .participants(vec!["agent://a".into()])
            .mode_version("1.0.0")
            .configuration_version("cfg-1")
            .started_at_unix_ms(1)
            .build()
    }

    /// Insert sessions straight into the registry, with each `Session`'s
    /// `session_id` equal to its map key (the Phase 1 `debug_assert_eq!`
    /// enforces the pair).
    async fn seed_sessions(runtime: &Arc<Runtime>, ids: &[String]) {
        for id in ids {
            runtime
                .registry
                .insert_recovered_session(id.clone(), paged_session(id))
                .await;
        }
    }

    fn list_sessions_req(page_size: i32, page_token: &str) -> Request<ListSessionsRequest> {
        let mut req = Request::new(ListSessionsRequest {
            page_size,
            page_token: page_token.to_string(),
        });
        req.metadata_mut()
            .insert("authorization", "Bearer agent://observer".parse().unwrap());
        req
    }

    fn page_size_security(default: usize, max: usize) -> SecurityLayer {
        let mut security = SecurityLayer::dev_mode();
        security.list_sessions_default_page_size = default;
        security.list_sessions_max_page_size = max;
        security
    }

    fn seed_ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("session-{i:03}")).collect()
    }

    #[tokio::test]
    async fn list_sessions_applies_default_page_size_when_zero() {
        let (server, runtime) = make_server_with_security(page_size_security(3, 1000));
        seed_sessions(&runtime, &seed_ids(10)).await;

        let resp = server
            .list_sessions(list_sessions_req(0, ""))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.sessions.len(), 3);
        assert!(!resp.next_page_token.is_empty());
    }

    #[tokio::test]
    async fn list_sessions_honors_explicit_page_size() {
        let (server, runtime) = make_server_with_security(page_size_security(100, 1000));
        seed_sessions(&runtime, &seed_ids(10)).await;

        let resp = server
            .list_sessions(list_sessions_req(4, ""))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.sessions.len(), 4);
        assert!(!resp.next_page_token.is_empty());
    }

    #[tokio::test]
    async fn list_sessions_clamps_page_size_above_max() {
        let (server, runtime) = make_server_with_security(page_size_security(100, 3));
        seed_sessions(&runtime, &seed_ids(10)).await;

        let resp = server
            .list_sessions(list_sessions_req(1000, ""))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.sessions.len(), 3);
        assert!(!resp.next_page_token.is_empty());
    }

    #[tokio::test]
    async fn list_sessions_rejects_negative_page_size() {
        let (server, runtime) = make_server();
        seed_sessions(&runtime, &seed_ids(3)).await;

        let err = server
            .list_sessions(list_sessions_req(-1, ""))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument, "{err:?}");
        assert!(err.message().contains("page_size"), "{err:?}");
    }

    #[tokio::test]
    async fn list_sessions_rejects_garbage_page_token() {
        use base64::Engine;
        let (server, runtime) = make_server();
        seed_sessions(&runtime, &seed_ids(3)).await;

        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let valid = engine.encode("v1:session-000");
        let tokens = vec![
            // not base64url
            "not-a-token!".to_string(),
            // wrong version prefix
            engine.encode("v2:session-000"),
            // prefix present, cursor empty
            engine.encode("v1:"),
            // truncated *through* the version prefix. Note that lopping bytes
            // off the end of an encoded token instead yields a valid, shorter
            // cursor — harmless, since a cursor is a position, not a handle —
            // so the truncation that must be rejected is the one that damages
            // the prefix.
            engine.encode("v1"),
            // front-truncated: the leading base64 character is gone, so the
            // decoded bytes are no longer valid UTF-8 (and could not carry the
            // prefix regardless).
            valid[1..].to_string(),
            // oversized: rejected by the length branch, before any decode
            "A".repeat(2 * 1024 * 1024),
        ];
        for token in tokens {
            let err = server
                .list_sessions(list_sessions_req(0, &token))
                .await
                .unwrap_err();
            assert_eq!(err.code(), tonic::Code::InvalidArgument);
            // One opaque message for every rejection reason.
            assert_eq!(
                err.message(),
                "INVALID_ARGUMENT: page_token is not a valid continuation token"
            );
        }
    }

    #[tokio::test]
    async fn list_sessions_full_traversal_visits_every_session_exactly_once() {
        let (server, runtime) = make_server_with_security(page_size_security(100, 1000));
        let ids = seed_ids(25);
        seed_sessions(&runtime, &ids).await;

        let mut collected: Vec<String> = Vec::new();
        let mut token = String::new();
        for _ in 0..100 {
            let resp = server
                .list_sessions(list_sessions_req(4, &token))
                .await
                .unwrap()
                .into_inner();
            collected.extend(resp.sessions.iter().map(|s| s.session_id.clone()));
            token = resp.next_page_token;
            if token.is_empty() {
                break;
            }
        }
        assert!(token.is_empty(), "traversal did not terminate");
        let unique: std::collections::HashSet<&String> = collected.iter().collect();
        // Both assertions: the set alone would hide duplicates, the total
        // alone would hide a duplicate paired with a drop.
        assert_eq!(unique.len(), 25, "sessions were dropped or duplicated");
        assert_eq!(collected.len(), 25, "sessions were duplicated");
    }

    #[tokio::test]
    async fn list_sessions_terminal_page_has_empty_next_page_token() {
        let (server, runtime) = make_server_with_security(page_size_security(100, 1000));
        seed_sessions(&runtime, &seed_ids(10)).await;

        let mut tokens: Vec<String> = Vec::new();
        let mut token = String::new();
        for _ in 0..20 {
            let resp = server
                .list_sessions(list_sessions_req(5, &token))
                .await
                .unwrap()
                .into_inner();
            token = resp.next_page_token;
            tokens.push(token.clone());
            if token.is_empty() {
                break;
            }
        }
        // 10 sessions at 5/page: exactly two pages, and only the last one
        // carries the empty token.
        assert_eq!(tokens.len(), 2, "{tokens:?}");
        assert!(!tokens[0].is_empty());
        assert!(tokens[1].is_empty());
    }

    #[tokio::test]
    async fn list_sessions_orders_by_session_id_ascending() {
        let (server, runtime) = make_server_with_security(page_size_security(100, 1000));
        // Insertion order deliberately unrelated to sort order.
        let ids: Vec<String> = ["delta", "alpha", "echo", "charlie", "bravo"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        seed_sessions(&runtime, &ids).await;

        let mut collected: Vec<String> = Vec::new();
        let mut token = String::new();
        loop {
            let resp = server
                .list_sessions(list_sessions_req(2, &token))
                .await
                .unwrap()
                .into_inner();
            collected.extend(resp.sessions.iter().map(|s| s.session_id.clone()));
            token = resp.next_page_token;
            if token.is_empty() {
                break;
            }
        }
        // Ascending across the whole traversal, not merely within a page.
        assert_eq!(
            collected,
            vec!["alpha", "bravo", "charlie", "delta", "echo"]
        );
    }

    #[tokio::test]
    async fn list_sessions_still_requires_authentication() {
        let (server, runtime) = make_server();
        seed_sessions(&runtime, &seed_ids(3)).await;

        // No authorization metadata, and a request body that would otherwise
        // be INVALID_ARGUMENT: authentication must still be what answers.
        let req = Request::new(ListSessionsRequest {
            page_size: -1,
            page_token: String::new(),
        });
        let err = server.list_sessions(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated, "{err:?}");
    }

    #[tokio::test]
    async fn list_sessions_tolerates_cursor_for_removed_session() {
        let (server, runtime) = make_server_with_security(page_size_security(100, 1000));
        let ids = seed_ids(4);
        seed_sessions(&runtime, &ids).await;

        let first = server
            .list_sessions(list_sessions_req(1, ""))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(first.sessions[0].session_id, "session-000");
        assert!(!first.next_page_token.is_empty());

        // Delete the very session the cursor names. A keyset cursor is a
        // position, not a handle, so paging must continue undisturbed.
        runtime
            .registry
            .sessions
            .write()
            .await
            .remove("session-000");

        let second = server
            .list_sessions(list_sessions_req(1, &first.next_page_token))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(second.sessions[0].session_id, "session-001");
    }

    #[tokio::test]
    async fn list_sessions_cursor_comes_from_the_id_list_not_the_returned_sessions() {
        // The cursor must be the last *candidate ID*, not the last *returned
        // session*. The two differ only when the registry is mutated between
        // the ID scan and the per-ID fetch, so this test manufactures exactly
        // that window: the handler parks on the first page entry's session
        // mutex, and while it is parked the last page entry is removed.
        //
        // Deriving the cursor from the returned sessions instead would move it
        // backwards, re-scanning IDs the page already accounted for — and, when
        // an entire page vanishes, would emit an empty token and silently
        // terminate the traversal, dropping every remaining session.
        let (server, runtime) = make_server_with_security(page_size_security(100, 1000));
        seed_sessions(&runtime, &seed_ids(6)).await;

        // Hold the first page entry's session mutex: the handler's fetch loop
        // parks there, which is the only deterministic yield point between the
        // ID scan and the rest of the fetches.
        let first = runtime.registry.get_shared("session-000").await.unwrap();
        let guard = first.lock().await;

        let handler = server.list_sessions(list_sessions_req(3, ""));
        let mutator = async {
            // The handler clones the Arc in `get_shared` before parking on the
            // mutex, so a strong count of 3 (map + this test + handler) means
            // it is parked. Bounded so a missed interleave fails loudly rather
            // than hanging.
            let mut spins = 0;
            while Arc::strong_count(&first) < 3 {
                assert!(spins < 10_000, "handler never parked on the session mutex");
                spins += 1;
                tokio::task::yield_now().await;
            }
            runtime
                .registry
                .sessions
                .write()
                .await
                .remove("session-002");
            drop(guard);
        };
        let (resp, ()) = tokio::join!(handler, mutator);
        let resp = resp.unwrap().into_inner();

        // The interleave actually happened: the last candidate was skipped.
        assert_eq!(
            resp.sessions.len(),
            2,
            "expected session-002 to vanish between the scan and the fetch"
        );
        assert_eq!(resp.sessions[1].session_id, "session-001");
        // ...yet the cursor is the last candidate, not the last survivor.
        assert_eq!(
            crate::pagination::decode_page_token(&resp.next_page_token),
            Ok("session-002".to_string()),
            "cursor was derived from the returned sessions, not the ID list"
        );

        // Observable through the API too: put session-002 back and page on.
        runtime
            .registry
            .insert_recovered_session("session-002".to_string(), paged_session("session-002"))
            .await;
        let second = server
            .list_sessions(list_sessions_req(3, &resp.next_page_token))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            second.sessions[0].session_id, "session-003",
            "the cursor moved backwards past an ID the page had already accounted for"
        );
    }

    #[tokio::test]
    async fn list_sessions_replaying_a_token_returns_the_identical_page() {
        let (server, runtime) = make_server_with_security(page_size_security(100, 1000));
        seed_sessions(&runtime, &seed_ids(10)).await;

        let first = server
            .list_sessions(list_sessions_req(3, ""))
            .await
            .unwrap()
            .into_inner();
        let token = first.next_page_token;
        assert!(!token.is_empty());

        let page_a = server
            .list_sessions(list_sessions_req(3, &token))
            .await
            .unwrap()
            .into_inner();
        let page_b = server
            .list_sessions(list_sessions_req(3, &token))
            .await
            .unwrap()
            .into_inner();

        let ids_a: Vec<&str> = page_a.sessions.iter().map(|s| &*s.session_id).collect();
        let ids_b: Vec<&str> = page_b.sessions.iter().map(|s| &*s.session_id).collect();
        assert_eq!(ids_a, ids_b);
        assert_eq!(page_a.next_page_token, page_b.next_page_token);
    }

    #[tokio::test]
    async fn list_sessions_survives_zero_effective_page_size() {
        // Both fields are `pub`, so a consumer can reach 0. The floor in the
        // handler must keep the response well-formed: never an empty page
        // paired with a non-empty token (which would never advance).
        let (server, runtime) = make_server_with_security(page_size_security(0, 0));
        seed_sessions(&runtime, &seed_ids(3)).await;

        let resp = server
            .list_sessions(list_sessions_req(0, ""))
            .await
            .unwrap()
            .into_inner();
        assert!(
            !resp.sessions.is_empty(),
            "empty page with token {:?} — the traversal terminates and ListSessions returns nothing",
            resp.next_page_token
        );
        assert_eq!(resp.sessions.len(), 1);
        assert!(!resp.next_page_token.is_empty());

        // And it actually advances.
        let next = server
            .list_sessions(list_sessions_req(0, &resp.next_page_token))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(next.sessions.len(), 1);
        assert_ne!(next.sessions[0].session_id, resp.sessions[0].session_id);
    }

    fn watch_sessions_req(sender: &str) -> Request<WatchSessionsRequest> {
        let mut req = Request::new(WatchSessionsRequest {});
        req.metadata_mut()
            .insert("authorization", format!("Bearer {sender}").parse().unwrap());
        req
    }

    /// Read the next event, failing (rather than hanging) if none arrives.
    async fn next_lifecycle_event(
        stream: &mut <MacpServer as MacpRuntimeService>::WatchSessionsStream,
    ) -> crate::pb::SessionLifecycleEvent {
        use tokio_stream::StreamExt;
        let resp = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("WatchSessions produced no event within 5s")
            .expect("stream ended")
            .expect("stream errored");
        resp.event.expect("event present")
    }

    /// Criterion 1 through the handler: N sessions in the registry produce
    /// exactly N `Created` events, one per session, now that the sync
    /// materializes them one at a time instead of deep-cloning the registry.
    #[tokio::test]
    async fn watch_sessions_initial_sync_emits_each_session_exactly_once() {
        let (server, runtime) = make_server();
        let ids = seed_ids(24);
        seed_sessions(&runtime, &ids).await;

        let mut stream = server
            .watch_sessions(watch_sessions_req("agent://observer"))
            .await
            .unwrap()
            .into_inner();

        let mut counts: HashMap<String, usize> = HashMap::new();
        for _ in 0..ids.len() {
            let event = next_lifecycle_event(&mut stream).await;
            assert_eq!(
                event.event_type,
                session_lifecycle_event::EventType::Created as i32
            );
            let session = event.session.expect("initial sync always carries metadata");
            *counts.entry(session.session_id).or_default() += 1;
        }
        assert_eq!(counts.len(), ids.len(), "sync emitted the wrong set");
        for id in &ids {
            assert_eq!(
                counts.get(id).copied(),
                Some(1),
                "{id} was not emitted exactly once"
            );
        }
    }

    /// Criterion 4: the lifecycle subscription is taken during the unary call,
    /// not lazily inside the generator.
    ///
    /// The generator is not polled until the client reads, so this drives a
    /// session terminal *after* the response is returned but *before* the first
    /// read. The `Cancelled` event is published while nothing is polling the
    /// stream — it can only be delivered because the subscription already
    /// existed. Move `subscribe_session_lifecycle()` inside the `try_stream!`
    /// and the second read here finds nothing and times out.
    #[tokio::test]
    async fn watch_sessions_subscribes_before_the_generator_is_polled() {
        let (server, _runtime) = make_server();
        let initiator = "agent://orchestrator";
        let sid = new_sid();
        start_session(&server, initiator, &sid, vec![initiator.into()]).await;

        let mut stream = server
            .watch_sessions(watch_sessions_req("agent://observer"))
            .await
            .unwrap()
            .into_inner();

        // Nothing has polled the stream yet; this event has only the
        // subscription taken above to land in.
        let mut cancel = Request::new(CancelSessionRequest {
            session_id: sid.clone(),
            reason: "test".into(),
        });
        cancel.metadata_mut().insert(
            "authorization",
            format!("Bearer {initiator}").parse().unwrap(),
        );
        let ack = server
            .cancel_session(cancel)
            .await
            .unwrap()
            .into_inner()
            .ack
            .unwrap();
        assert!(ack.ok);

        // The initial sync comes first...
        let first = next_lifecycle_event(&mut stream).await;
        assert_eq!(
            first.event_type,
            session_lifecycle_event::EventType::Created as i32
        );
        assert_eq!(first.session.unwrap().session_id, sid);

        // ...then the event buffered while the generator was still unpolled.
        let second = next_lifecycle_event(&mut stream).await;
        assert_eq!(
            second.event_type,
            session_lifecycle_event::EventType::Cancelled as i32,
            "the event published before the first poll was lost — the \
             subscription must be taken in the unary call"
        );
        assert_eq!(second.session.unwrap().session_id, sid);
    }

    /// Both arms of the `Created` dedup, with the race that makes it necessary
    /// driven deterministically.
    ///
    /// `process_session_start` registers the session BEFORE it publishes
    /// `Created`, so a session started after the subscribe but before the first
    /// poll is in the sync snapshot *and* has a `Created` sitting on the bus.
    /// The sync must emit it once and the buffered copy must be dropped. A
    /// session started after the sync is not in the snapshot, and its live
    /// `Created` must pass through — the handler tests membership of the sync
    /// set with `contains` rather than `insert`, so that pass-through does not
    /// extend the set; the set stays bounded by the registry size at subscribe
    /// time. That bound is structural and has no observable signal, so what is
    /// asserted here is the exactly-once contract it must not break.
    #[tokio::test]
    async fn watch_sessions_emits_created_once_for_synced_and_live_sessions() {
        let (server, _runtime) = make_server();
        let initiator = "agent://orchestrator";
        let synced_sid = new_sid();

        let mut stream = server
            .watch_sessions(watch_sessions_req("agent://observer"))
            .await
            .unwrap()
            .into_inner();

        // Registered and published while nothing is polling: this lands in the
        // subscription AND in the snapshot the first poll takes.
        start_session(&server, initiator, &synced_sid, vec![initiator.into()]).await;

        let from_sync = next_lifecycle_event(&mut stream).await;
        assert_eq!(
            from_sync.event_type,
            session_lifecycle_event::EventType::Created as i32
        );
        assert_eq!(from_sync.session.unwrap().session_id, synced_sid);

        // Started after the sync, so it is absent from the snapshot.
        let live_sid = new_sid();
        start_session(&server, initiator, &live_sid, vec![initiator.into()]).await;

        // The next event must be the live session's Created. If the buffered
        // duplicate leaked through, this is `synced_sid` a second time.
        let live = next_lifecycle_event(&mut stream).await;
        assert_eq!(
            live.event_type,
            session_lifecycle_event::EventType::Created as i32
        );
        assert_eq!(
            live.session.unwrap().session_id,
            live_sid,
            "the sync entry's buffered Created must be suppressed, and the \
             live session's must not be"
        );

        // And nothing further: neither Created repeats.
        use tokio_stream::StreamExt;
        let extra =
            tokio::time::timeout(std::time::Duration::from_millis(300), stream.next()).await;
        assert!(
            extra.is_err(),
            "unexpected extra lifecycle event: {extra:?}"
        );
    }

    /// Criterion 3 end to end through the real handler, which the unit tests of
    /// `drain_lifecycle_events` cannot reach: a client that reads slowly while
    /// lifecycle events arrive throughout a long sync must not be killed with
    /// `RESOURCE_EXHAUSTED`, and must still see every `Created` exactly once.
    ///
    /// The shape matters. The lifecycle bus holds 64 events, and far more than
    /// that arrive here — but they arrive *interleaved* with the reads, which is
    /// what a slow consumer actually looks like. The handler drains the bus
    /// before fetching each session, so the receiver never falls 64 behind.
    /// Delete that drain (or hoist it out of the loop) and the bus overruns
    /// mid-sync, the first post-sync `recv()` returns `Lagged`, and this test
    /// fails on the stream error.
    #[tokio::test]
    async fn watch_sessions_survives_a_slow_consumer_during_a_long_sync() {
        use tokio_stream::StreamExt;

        let (server, runtime) = make_server();
        let seeded = seed_ids(200);
        seed_sessions(&runtime, &seeded).await;

        let mut stream = server
            .watch_sessions(watch_sessions_req("agent://observer"))
            .await
            .unwrap()
            .into_inner();

        let mut counts: HashMap<String, usize> = HashMap::new();
        // Reads the next event, failing loudly if the stream errored — that is
        // the RESOURCE_EXHAUSTED this test exists to rule out.
        async fn read(
            stream: &mut <MacpServer as MacpRuntimeService>::WatchSessionsStream,
            counts: &mut HashMap<String, usize>,
        ) {
            let resp = tokio::time::timeout(std::time::Duration::from_secs(10), stream.next())
                .await
                .expect("WatchSessions stalled")
                .expect("stream ended early")
                .expect("stream must not be terminated (RESOURCE_EXHAUSTED)");
            let event = resp.event.expect("event present");
            if event.event_type == session_lifecycle_event::EventType::Created as i32 {
                let session = event.session.expect("Created always carries metadata");
                *counts.entry(session.session_id).or_default() += 1;
            }
        }

        // One read starts the sync and suspends the generator at its first
        // yield, with 199 sessions still to emit.
        read(&mut stream, &mut counts).await;

        // 70 live starts — more than the bus capacity — spread across the sync,
        // two reads per start so the consumer stays behind the producer without
        // ever stopping.
        let initiator = "agent://orchestrator";
        let mut live = Vec::new();
        for _ in 0..70 {
            let sid = new_sid();
            start_session(&server, initiator, &sid, vec![initiator.into()]).await;
            live.push(sid);
            read(&mut stream, &mut counts).await;
            read(&mut stream, &mut counts).await;
        }

        // Drain the rest of the sync plus the buffered live events.
        let expected = seeded.len() + live.len();
        while counts.len() < expected {
            read(&mut stream, &mut counts).await;
        }

        for id in seeded.iter().chain(live.iter()) {
            assert_eq!(
                counts.get(id).copied(),
                Some(1),
                "{id} was not emitted exactly once"
            );
        }
        assert_eq!(counts.len(), expected, "unexpected extra sessions emitted");
    }

    // ── WhatIfReplay (RFC-MACP-0012 §8.1) ───────────────────────────────

    const BUYER: &str = "agent://buyer";
    const SELLER: &str = "agent://seller";
    const PROPOSAL: &str = "macp.mode.proposal.v1";

    fn penv(message_type: &str, message_id: &str, sid: &str, payload: Vec<u8>) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: PROPOSAL.into(),
            message_type: message_type.into(),
            message_id: message_id.into(),
            session_id: sid.into(),
            sender: String::new(),
            timestamp_unix_ms: Utc::now().timestamp_millis(),
            payload,
        }
    }

    /// SessionStart(0) → Proposal(1) → Accept(2) → Accept(3) → Commitment(4),
    /// resolved under `policy.default`; the buyer initiates and commits.
    async fn resolved_proposal_session(server: &MacpServer) -> String {
        resolved_proposal_session_paused(server, false).await
    }

    /// As [`resolved_proposal_session`]; `pause` suspends and resumes the
    /// session before the Commitment, leaving two internal log entries.
    async fn resolved_proposal_session_paused(server: &MacpServer, pause: bool) -> String {
        let sid = new_sid();
        let start = SessionStartPayload {
            intent: "buy".into(),
            participants: vec![BUYER.into(), SELLER.into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 60_000,
            ..Default::default()
        }
        .encode_to_vec();
        let proposal = crate::proposal_pb::ProposalPayload {
            proposal_id: "p1".into(),
            title: "offer".into(),
            ..Default::default()
        }
        .encode_to_vec();
        let accept = crate::proposal_pb::AcceptPayload {
            proposal_id: "p1".into(),
            reason: String::new(),
        }
        .encode_to_vec();
        let commitment = crate::pb::CommitmentPayload {
            commitment_id: "c1".into(),
            action: "proposal.accepted".into(),
            authority_scope: "commercial".into(),
            reason: "bound".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy.default".into(),
            configuration_version: "cfg-1".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();
        for (sender, env) in [
            (BUYER, penv("SessionStart", "m0", &sid, start)),
            (SELLER, penv("Proposal", "m1", &sid, proposal)),
            (SELLER, penv("Accept", "m2", &sid, accept.clone())),
            (BUYER, penv("Accept", "m3", &sid, accept)),
            (BUYER, penv("Commitment", "m4", &sid, commitment)),
        ] {
            if pause && env.message_type == "Commitment" {
                server
                    .runtime
                    .suspend_session(&sid, "hold", BUYER)
                    .await
                    .unwrap();
                server
                    .runtime
                    .resume_session(&sid, "go", BUYER)
                    .await
                    .unwrap();
            }
            let ack = do_send(server, sender, env).await;
            assert!(ack.ok, "{:?}", ack.error);
        }
        sid
    }

    #[tokio::test]
    async fn what_if_skips_internal_entries_in_history_index() {
        let (server, runtime) = make_server();
        let sid = resolved_proposal_session_paused(&server, true).await;
        let internal = runtime
            .log_store
            .get_log(&sid)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.entry_kind == crate::log_store::EntryKind::Internal)
            .count();
        assert_eq!(internal, 2, "the suspend and the resume");
        let resp = server
            .what_if_replay(what_if_req(SELLER, &sid, Some(seller_only())))
            .await
            .unwrap()
            .into_inner();
        let idx: Vec<_> = resp.entries.iter().map(|e| e.history_index).collect();
        assert_eq!(idx, vec![0, 1, 2, 3, 4]);
        assert_eq!(resp.first_divergence, Some(4));
        assert_eq!(resp.recorded.unwrap().resolved_by_history_index, 4);
    }

    fn what_if_req(
        sender: &str,
        sid: &str,
        candidate: Option<PolicyDescriptor>,
    ) -> Request<WhatIfReplayRequest> {
        let mut req = Request::new(WhatIfReplayRequest {
            session_id: sid.into(),
            candidate,
        });
        req.metadata_mut()
            .insert("authorization", format!("Bearer {sender}").parse().unwrap());
        req
    }

    fn candidate(mode: &str, rules: serde_json::Value, schema_version: u32) -> PolicyDescriptor {
        PolicyDescriptor {
            policy_id: "what-if.candidate".into(),
            mode: mode.into(),
            description: "candidate".into(),
            rules: rules.to_string(),
            schema_version,
            ..Default::default()
        }
    }

    fn seller_only() -> PolicyDescriptor {
        candidate(
            PROPOSAL,
            serde_json::json!({
                "commitment": { "authority": "designated_role", "designated_roles": [SELLER] }
            }),
            1,
        )
    }

    #[tokio::test]
    async fn what_if_reports_where_a_candidate_diverges() {
        let (server, runtime) = make_server();
        let sid = resolved_proposal_session(&server).await;
        let log_before = runtime.log_store.get_log(&sid).await.unwrap();
        let session_before = runtime.get_session_checked(&sid).await.unwrap();

        let resp = server
            .what_if_replay(what_if_req(SELLER, &sid, Some(seller_only())))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.candidate_policy_id, "what-if.candidate");
        assert_eq!(resp.recorded_policy_id, "policy.default");
        let recorded = resp.recorded.unwrap();
        assert_eq!(recorded.state, PbSessionState::Resolved as i32);
        assert_eq!(recorded.resolved_by_history_index, 4);
        assert_eq!(recorded.resolved_by_message_id, "m4");
        assert!(!recorded.resolution.is_empty());
        let cand = resp.candidate.unwrap();
        assert_eq!(cand.state, PbSessionState::Open as i32);
        assert!(cand.resolution.is_empty());

        // history_index is the 0-based accepted ordinal, SessionStart = 0.
        let idx: Vec<_> = resp.entries.iter().map(|e| e.history_index).collect();
        assert_eq!(idx, vec![0, 1, 2, 3, 4]);
        assert_eq!(resp.first_divergence, Some(4));
        let last = resp.entries.last().unwrap();
        assert_eq!(last.message_type, "Commitment");
        assert_eq!(last.recorded.as_ref().unwrap().kind, Kind::Applied as i32);
        let refused = last.candidate.as_ref().unwrap();
        assert_eq!(refused.kind, Kind::Refused as i32);
        assert!(!refused.error_code.is_empty());

        // Read-only: no envelope, session and log untouched, never registered.
        assert_eq!(
            runtime.log_store.get_log(&sid).await.unwrap().len(),
            log_before.len()
        );
        let session_after = runtime.get_session_checked(&sid).await.unwrap();
        assert_eq!(session_after.state, session_before.state);
        assert_eq!(
            session_after.seen_message_ids.len(),
            session_before.seen_message_ids.len()
        );
        assert!(runtime.get_policy("what-if.candidate").is_none());
    }

    #[tokio::test]
    async fn what_if_under_an_equivalent_candidate_does_not_diverge() {
        let (server, _) = make_server();
        let sid = resolved_proposal_session(&server).await;
        // `"*"` governs every mode; an id the registry would refuse to
        // register is fine for a candidate.
        let mut equivalent = candidate("*", serde_json::json!({}), 1);
        equivalent.policy_id = "policy.default".into();
        let resp = server
            .what_if_replay(what_if_req(BUYER, &sid, Some(equivalent)))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.first_divergence, None);
        assert!(resp.denials.is_empty());
        assert_eq!(resp.recorded, resp.candidate);
    }

    #[tokio::test]
    async fn what_if_refuses_what_register_policy_would() {
        let (server, _) = make_server();
        let sid = resolved_proposal_session(&server).await;
        for bad in [
            candidate("macp.mode.decision.v1", serde_json::json!({}), 1),
            candidate(PROPOSAL, serde_json::json!({}), 99),
            candidate(PROPOSAL, serde_json::json!({ "no_such_rule": true }), 1),
        ] {
            let err = server
                .what_if_replay(what_if_req(BUYER, &sid, Some(bad)))
                .await
                .unwrap_err();
            assert_eq!(
                err.code(),
                tonic::Code::InvalidArgument,
                "{}",
                err.message()
            );
            assert_eq!(
                err.metadata().get("macp-error-code").unwrap(),
                "INVALID_POLICY_DEFINITION"
            );
        }
        let err = server
            .what_if_replay(what_if_req(BUYER, &sid, None))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn what_if_is_authorized_as_get_session() {
        let (server, _) = make_server();
        let sid = resolved_proposal_session(&server).await;
        let err = server
            .what_if_replay(what_if_req("agent://outsider", &sid, Some(seller_only())))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        let err = server
            .what_if_replay(what_if_req(BUYER, &new_sid(), Some(seller_only())))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn what_if_refuses_a_compacted_history() {
        let (server, runtime) = make_server();
        let sid = resolved_proposal_session(&server).await;
        let mut checkpoint = runtime.log_store.get_log(&sid).await.unwrap()[0].clone();
        checkpoint.entry_kind = crate::log_store::EntryKind::Checkpoint;
        checkpoint.message_type = "Checkpoint".into();
        checkpoint.compacted_incoming_ordinals = 5;
        runtime
            .log_store
            .replace_session_log(&sid, vec![checkpoint])
            .await;
        let err = server
            .what_if_replay(what_if_req(BUYER, &sid, Some(seller_only())))
            .await
            .unwrap_err();
        assert_coded(&err, "HISTORY_UNAVAILABLE", tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn what_if_fails_when_the_recorded_half_does_not_reproduce() {
        let (server, runtime) = make_server();
        let sid = resolved_proposal_session(&server).await;
        // Drop the Commitment: the recorded replay now ends Open, but the
        // stored session is Resolved.
        let mut log = runtime.log_store.get_log(&sid).await.unwrap();
        log.retain(|e| e.message_type != "Commitment");
        runtime.log_store.replace_session_log(&sid, log).await;
        let err = server
            .what_if_replay(what_if_req(BUYER, &sid, Some(seller_only())))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
        assert_eq!(
            err.metadata().get("macp-error-code").unwrap(),
            "INTERNAL_ERROR"
        );
    }

    #[tokio::test]
    async fn initialize_advertises_what_if_replay() {
        let (server, _) = make_server();
        let resp = server
            .initialize(Request::new(InitializeRequest {
                supported_protocol_versions: vec!["1.0".into()],
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(
            resp.capabilities
                .unwrap()
                .policy_registry
                .unwrap()
                .what_if_replay
        );
    }
}
