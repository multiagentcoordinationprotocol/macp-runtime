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
    WatchSignalsResponse,
};
use crate::runtime::Runtime;
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
        if env.macp_version != "1.0" {
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
        if env.message_type == "Progress" && env.session_id.is_empty() {
            // Ambient Progress: mode must also be empty
            if !env.mode.trim().is_empty() {
                return Err(MacpError::InvalidEnvelope);
            }
        }
        if !is_ambient_type && env.session_id.is_empty() {
            return Err(MacpError::InvalidEnvelope);
        }
        if !is_ambient_type && env.mode.trim().is_empty() {
            return Err(MacpError::InvalidEnvelope);
        }
        // Session-scoped Progress must have non-empty mode (enforced above for non-ambient types,
        // and ambient Progress with non-empty session_id falls through to here naturally)
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
    /// Currently only `PolicyDenied` carries additional detail (its reasons list).
    fn error_details_bytes(e: &MacpError) -> Vec<u8> {
        match e {
            MacpError::PolicyDenied { reasons } => {
                serde_json::to_vec(&serde_json::json!({ "reasons": reasons })).unwrap_or_default()
            }
            _ => vec![],
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
            .ok_or_else(|| Status::not_found(format!("Session '{}' not found", session_id)))?;
        let allowed = identity.is_observer
            || session.initiator_sender == identity.sender
            || session.participants.iter().any(|p| p == &identity.sender);
        if !allowed {
            return Err(Status::permission_denied(
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
                Err(Status::resource_exhausted(format!(
                    "StreamSession receiver fell behind by {skipped} envelopes"
                )))
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
    ) -> Result<Vec<Envelope>, Status> {
        // RFC-MACP-0006-A1: Handle subscribe-only frame.
        // When subscribe_session_id is set and envelope is absent, subscribe to
        // the session's broadcast channel and replay accepted history.
        if !req.subscribe_session_id.is_empty() {
            if req.envelope.is_some() {
                return Err(Status::invalid_argument(
                    "StreamSessionRequest must not contain both envelope and subscribe_session_id",
                ));
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
            Status::invalid_argument(
                "StreamSessionRequest must contain an envelope or subscribe_session_id",
            )
        })?;

        self.validate_envelope_shape(&envelope)
            .map_err(Self::status_from_error)?;
        if envelope.session_id.trim().is_empty() {
            return Err(Status::invalid_argument(
                "StreamSession requires a non-empty session_id",
            ));
        }
        if envelope.mode.trim().is_empty() {
            return Err(Status::invalid_argument(
                "StreamSession requires a non-empty mode",
            ));
        }
        if let Some(bound) = bound_session_id.as_ref() {
            if bound != &envelope.session_id {
                return Err(Status::invalid_argument(
                    "StreamSession may only carry envelopes for one session_id",
                ));
            }
        }

        let envelope = Self::apply_authenticated_sender(identity, envelope)
            .map_err(Self::status_from_error)?;
        let is_session_start = envelope.message_type == "SessionStart";

        if !is_session_start {
            if let Some(session) = self.runtime.get_session_checked(&envelope.session_id).await {
                if envelope.mode != session.mode {
                    return Err(Status::invalid_argument(
                        "INVALID_ENVELOPE: envelope mode does not match the bound session mode",
                    ));
                }
                if session.state != SessionState::Open {
                    return Err(Status::invalid_argument("SESSION_NOT_OPEN"));
                }
            } else if envelope.message_type == "Signal" {
                return Err(Status::not_found(format!(
                    "Session '{}' not found",
                    envelope.session_id
                )));
            }
        }

        self.security
            .authorize_mode(identity, &envelope.mode, is_session_start)
            .map_err(Self::status_from_error)?;
        // External ingress policy engine (E3): the stream path must be gated
        // identically to unary Send — without this, a sender denied by an
        // installed engine could simply switch transports.
        self.enforce_ingress_policy(identity, &envelope)
            .await
            .map_err(Self::status_from_error)?;
        self.security
            .enforce_rate_limit(&identity.sender, is_session_start)
            .await
            .map_err(Self::status_from_error)?;

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
            .map_err(Self::status_from_error)?;
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
    ) -> Result<Vec<Envelope>, Status> {
        // Validate: only one session per stream
        if let Some(bound) = bound_session_id.as_ref() {
            if bound != session_id {
                return Err(Status::invalid_argument(
                    "StreamSession may only carry envelopes for one session_id",
                ));
            }
        }

        // Validate session exists
        let session = self
            .runtime
            .get_session_checked(session_id)
            .await
            .ok_or_else(|| Status::not_found(format!("Session '{}' not found", session_id)))?;

        // Authorize: caller must be a declared participant, initiator, or observer
        let allowed = identity.is_observer
            || session.initiator_sender == identity.sender
            || session.participants.iter().any(|p| p == &identity.sender);
        if !allowed {
            return Err(Status::permission_denied(
                "FORBIDDEN: caller is not a declared participant or observer for this session",
            ));
        }
        // External ingress policy engine (E3): stream-based history replay is
        // a read and must be gated like GetSession (fail closed).
        if let Some(engine) = &self.policy_engine {
            let decision = engine.evaluate_session_access(identity, &session).await;
            crate::policy_engine::require_allow(decision, "session access")?;
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
                Status::failed_precondition(format!(
                    "session history before ordinal {base} was compacted; \
                     resume with after_sequence >= {base} or re-read state via GetSession"
                ))
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
                                Err(status) if Self::is_stream_terminal_error(&status) => {
                                    Err(status)?;
                                }
                                Err(status) => {
                                    // RFC-MACP-0001: application-level validation errors
                                    // are sent as inline MACPError; stream remains open.
                                    yield StreamSessionResponse {
                                        response: Some(
                                            crate::pb::stream_session_response::Response::Error(
                                                PbMacpError {
                                                    code: status.message().to_string(),
                                                    message: status.message().to_string(),
                                                    session_id: bound_session_id.clone().unwrap_or_default(),
                                                    message_id: String::new(),
                                                    details: vec![],
                                                },
                                            ),
                                        ),
                                    };
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
                            Err(Status::resource_exhausted(format!(
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
                                Err(status) if Self::is_stream_terminal_error(&status) => {
                                    Err(status)?;
                                }
                                Err(status) => {
                                    yield StreamSessionResponse {
                                        response: Some(
                                            crate::pb::stream_session_response::Response::Error(
                                                PbMacpError {
                                                    code: status.message().to_string(),
                                                    message: status.message().to_string(),
                                                    session_id: bound_session_id.clone().unwrap_or_default(),
                                                    message_id: String::new(),
                                                    details: vec![],
                                                },
                                            ),
                                        ),
                                    };
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

    /// Returns true if the error should terminate a StreamSession stream.
    /// Transport and binding errors terminate. Application-level validation
    /// errors (from `runtime.process()`) are sent as inline MACPError per RFC-0001.
    fn is_stream_terminal_error(status: &Status) -> bool {
        matches!(
            status.code(),
            tonic::Code::Unauthenticated
                | tonic::Code::Internal
                | tonic::Code::ResourceExhausted
                | tonic::Code::InvalidArgument
                | tonic::Code::NotFound
                | tonic::Code::AlreadyExists
        )
    }

    fn status_from_error(err: MacpError) -> Status {
        match err {
            MacpError::Unauthenticated => Status::unauthenticated(err.to_string()),
            MacpError::Forbidden => Status::permission_denied(err.to_string()),
            MacpError::PayloadTooLarge => Status::resource_exhausted(err.to_string()),
            MacpError::RateLimited => Status::resource_exhausted(err.to_string()),
            MacpError::StorageFailed => Status::internal(err.to_string()),
            MacpError::InvalidSessionId => Status::invalid_argument(err.to_string()),
            MacpError::InvalidPolicyDefinition => Status::invalid_argument(err.to_string()),
            MacpError::SessionAlreadyExists => Status::already_exists(err.to_string()),
            MacpError::PolicyDenied { ref reasons } => {
                let details = Self::error_details_bytes(&err);
                let msg = if reasons.is_empty() {
                    "PolicyDenied".to_string()
                } else {
                    format!("PolicyDenied: {}", reasons.join("; "))
                };
                let mut status = Status::failed_precondition(msg);
                if !details.is_empty() {
                    // Attach JSON details as binary metadata so clients can parse structured reasons.
                    let val = tonic::metadata::MetadataValue::from_bytes(&details);
                    status
                        .metadata_mut()
                        .insert_bin("macp-error-details-bin", val);
                }
                status
            }
            _ => Status::failed_precondition(err.to_string()),
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
            return Err(Status::invalid_argument(
                "INVALID_REQUEST: supported_protocol_versions must not be empty",
            ));
        }
        if !req.supported_protocol_versions.iter().any(|v| v == "1.0") {
            return Err(Status::failed_precondition(
                "UNSUPPORTED_PROTOCOL_VERSION: no mutually supported protocol version",
            ));
        }

        Ok(Response::new(InitializeResponse {
            selected_protocol_version: "1.0".into(),
            runtime_info: Some(RuntimeInfo {
                name: "macp-runtime".into(),
                title: "MACP Reference Runtime".into(),
                version: "0.4.0".into(),
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
                    register_policy: !self.policies_read_only,
                    list_policies: true,
                    list_changed: true,
                }),
                experimental: Some(crate::pb::ExperimentalCapabilities {
                    features: HashMap::from([
                        ("ext_mode_lifecycle".into(), "true".into()),
                    ]),
                }),
            }),
            supported_modes: self.runtime.registered_mode_names(),
            instructions: "Authenticate requests with Authorization: Bearer <token>. Use the unary Send RPC for all session messaging. For local development only, x-macp-agent-id may be enabled by configuration.".into(),
        }))
    }

    async fn send(&self, request: Request<SendRequest>) -> Result<Response<SendResponse>, Status> {
        let env = request
            .get_ref()
            .envelope
            .clone()
            .ok_or_else(|| Status::invalid_argument("SendRequest must contain an envelope"))?;

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
            .ok_or_else(|| Status::not_found(format!("Session '{}' not found", session_id)))?;

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
            .ok_or_else(|| Status::not_found(format!("Session '{}' not found", session_id)))?;
        // RFC-MACP-0001: "Only the initiator and policy-delegated roles may cancel."
        // CancelSession is a Core control-plane message — mode authorization does not apply.
        if identity.sender != session.initiator_sender
            && crate::mode::util::check_commitment_authority(&session, &identity.sender).is_err()
        {
            return Err(Status::permission_denied(
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
            .ok_or_else(|| Status::not_found(format!("Session '{}' not found", session_id)))?;
        // RFC-MACP-0001 §7.5: same authority model as CancelSession — initiator
        // or policy-delegated roles only; mode authorization does not apply.
        if identity.sender != session.initiator_sender
            && crate::mode::util::check_commitment_authority(&session, &identity.sender).is_err()
        {
            return Err(Status::permission_denied(
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
            .ok_or_else(|| Status::not_found(format!("Session '{}' not found", session_id)))?;
        if identity.sender != session.initiator_sender
            && crate::mode::util::check_commitment_authority(&session, &identity.sender).is_err()
        {
            return Err(Status::permission_denied(
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
            return Err(Status::not_found(format!(
                "Agent '{}' not found",
                req.agent_id
            )));
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
        let mut rx = self.runtime.subscribe_signals();
        let stream = async_stream::try_stream! {
            loop {
                match rx.recv().await {
                    Ok(envelope) => {
                        yield WatchSignalsResponse {
                            envelope: Some(envelope),
                        };
                    }
                    // Surface lag instead of silently ending the stream: a
                    // slow consumer must be able to distinguish "no traffic"
                    // from "events dropped" (mirrors StreamSession).
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        Err(Status::resource_exhausted(format!(
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
        let _identity = self
            .security
            .authenticate_metadata(request.metadata())
            .await
            .map_err(Self::status_from_error)?;
        let sessions = self.runtime.registry.get_all_sessions().await;
        let metadata: Vec<SessionMetadata> =
            sessions.iter().map(Self::session_to_metadata).collect();
        Ok(Response::new(ListSessionsResponse { sessions: metadata }))
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
        let mut rx = self.runtime.subscribe_session_lifecycle();
        let runtime = Arc::clone(&self.runtime);
        let stream = async_stream::try_stream! {
            // Initial sync: emit all current sessions as CREATED events. The
            // lifecycle bus was subscribed *before* this snapshot (so no event
            // is missed); any Created event buffered in that window would
            // duplicate a snapshot entry — session IDs are create-once, so we
            // dedupe buffered Created events against the synced set below.
            let sessions = runtime.registry.get_all_sessions().await;
            let mut synced: std::collections::HashSet<String> =
                std::collections::HashSet::with_capacity(sessions.len());
            for session in &sessions {
                synced.insert(session.session_id.clone());
                yield WatchSessionsResponse {
                    event: Some(SessionLifecycleEvent {
                        event_type: session_lifecycle_event::EventType::Created.into(),
                        session: Some(Self::session_to_metadata(session)),
                        observed_at_unix_ms: session.started_at_unix_ms,
                    }),
                };
            }
            // Stream lifecycle transitions
            loop {
                let event = match rx.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        Err(Status::resource_exhausted(format!(
                            "WatchSessions receiver fell behind by {skipped} events"
                        )))?;
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
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
                if event_type == session_lifecycle_event::EventType::Created
                    && !synced.insert(sid.clone())
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
            .ok_or_else(|| Status::invalid_argument("descriptor required"))?;
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
            return Err(Status::failed_precondition(
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
            .ok_or_else(|| Status::invalid_argument("descriptor required"))?;
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
            return Err(Status::failed_precondition(
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
        let policy = self
            .runtime
            .get_policy(&req.policy_id)
            .ok_or_else(|| Status::not_found(format!("Policy '{}' not found", req.policy_id)))?;
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
    use crate::pb::SessionStartPayload;
    use crate::registry::SessionRegistry;
    use chrono::Utc;
    use prost::Message;

    fn new_sid() -> String {
        uuid::Uuid::new_v4().as_hyphenated().to_string()
    }

    fn make_server() -> (MacpServer, Arc<Runtime>) {
        let storage: Arc<dyn crate::storage::StorageBackend> =
            Arc::new(crate::storage::MemoryBackend);
        let registry = Arc::new(SessionRegistry::new());
        let log_store = Arc::new(LogStore::new());
        let runtime = Arc::new(Runtime::new(storage, registry, log_store));
        let server = MacpServer::new(runtime.clone(), SecurityLayer::dev_mode());
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
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
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
        assert_eq!(err.code(), tonic::Code::NotFound);
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
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
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
        // PolicyDenied maps to FailedPrecondition on the transport (same
        // error the unary path expresses as a POLICY_DENIED ack).
        assert_eq!(err.code(), tonic::Code::FailedPrecondition, "{err:?}");
        assert!(err.message().contains("PolicyDenied"), "{err:?}");

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
}
