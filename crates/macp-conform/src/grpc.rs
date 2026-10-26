//! The gRPC target: the Server profile, against any `MACPRuntimeService`.
//!
//! Every envelope goes through `Send`, authenticated as its own sender: the
//! bearer token is the sender's name (what a server in local-development
//! mode, `MACP_ALLOW_INSECURE=1`, takes as the identity), unless a token was
//! configured for that sender with [`GrpcTarget::with_token`].
//!
//! What a server cannot show over the wire is reported, not guessed:
//!
//! - there is no clock control, so clocked fixtures are skipped
//!   (`needs-clock-control`);
//! - there is no mode-state or replay RPC, so `expected_mode_state` and
//!   `verify_replay_equivalence` are *unverified*;
//! - there is no commitment-hash RPC, so the `cmt-hash` vectors are skipped
//!   (`needs-hash-api`), unless they run in-process against the same
//!   implementation and release (`crate::runner::run_with_hash_target`; the
//!   CLI's `--hash-vectors in-process` checks the release with
//!   [`GrpcTarget::runtime_info`]).
//!
//! State comes from `GetSession`; history from a passive `StreamSession`
//! subscription (`subscribe_session_id`, `after_sequence = 0`), read until
//! the stream goes quiet; the resolution is the payload of the last
//! accepted `Commitment` when the session is `Resolved`.

use crate::target::{
    Capabilities, ConformanceTarget, HistoryEntry, Observable, Observation, Profile, Submission,
    TargetError,
};
use async_trait::async_trait;
use macp_core::PolicyDefinition;
use macp_pb::pb::{
    stream_session_response, Envelope, GetSessionRequest, InitializeRequest, PolicyDescriptor,
    RegisterPolicyRequest, SendRequest, SessionState, StreamSessionRequest,
    UnregisterPolicyRequest,
};
use std::collections::HashMap;
use std::time::Duration;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

use crate::client::MacpRuntimeServiceClient;

/// Identity used for the policy-registration RPCs.
const ADMIN_SENDER: &str = "macp-conform";

/// See the module docs.
pub struct GrpcTarget {
    url: String,
    client: MacpRuntimeServiceClient<Channel>,
    tokens: HashMap<String, String>,
    stream_idle: Duration,
}

impl GrpcTarget {
    /// Connect to `url`: `grpc://host:port` or `http://host:port` in the
    /// clear, `grpcs://` or `https://` over TLS (WebPKI roots).
    pub async fn connect(url: &str) -> Result<Self, TargetError> {
        let client = crate::client::connect(url).await.map_err(TargetError)?;
        Ok(Self {
            url: url.to_string(),
            client,
            tokens: HashMap::new(),
            stream_idle: Duration::from_millis(250),
        })
    }

    /// Authenticate as `sender` with this bearer token instead of the
    /// sender's name.
    pub fn with_token(mut self, sender: impl Into<String>, token: impl Into<String>) -> Self {
        self.tokens.insert(sender.into(), token.into());
        self
    }

    /// How long a quiet `StreamSession` is taken to have finished replaying
    /// history (default 250 ms).
    pub fn with_stream_idle(mut self, idle: Duration) -> Self {
        self.stream_idle = idle;
        self
    }

    /// The server's `Initialize` `runtime_info`: `(name, version)`, e.g.
    /// `("macp-runtime", "0.9.0")`.
    pub async fn runtime_info(&mut self) -> Result<(String, String), TargetError> {
        let req = self.request(
            ADMIN_SENDER,
            InitializeRequest {
                supported_protocol_versions: vec![macp_core::MACP_VERSION.into()],
                ..Default::default()
            },
        )?;
        let resp = self
            .client
            .initialize(req)
            .await
            .map_err(|s| TargetError(format!("Initialize failed: {}", s.message())))?
            .into_inner();
        let info = resp.runtime_info.unwrap_or_default();
        Ok((info.name, info.version))
    }

    fn request<T>(&self, sender: &str, body: T) -> Result<tonic::Request<T>, TargetError> {
        let token = self
            .tokens
            .get(sender)
            .map(String::as_str)
            .unwrap_or(sender);
        let mut req = tonic::Request::new(body);
        let value = MetadataValue::try_from(format!("Bearer {token}"))
            .map_err(|_| TargetError(format!("sender {sender:?} is not a valid bearer token")))?;
        req.metadata_mut().insert("authorization", value);
        req.set_timeout(Duration::from_secs(30));
        Ok(req)
    }

    async fn history(
        &mut self,
        session_id: &str,
        reader: &str,
    ) -> Result<Vec<Envelope>, TargetError> {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.send(StreamSessionRequest {
            envelope: None,
            subscribe_session_id: session_id.to_string(),
            after_sequence: 0,
        })
        .await
        .map_err(|_| TargetError("stream request channel closed".into()))?;
        let token = self
            .tokens
            .get(reader)
            .map(String::as_str)
            .unwrap_or(reader);
        let mut req = tonic::Request::new(tokio_stream::wrappers::ReceiverStream::new(rx));
        req.metadata_mut().insert(
            "authorization",
            MetadataValue::try_from(format!("Bearer {token}"))
                .map_err(|_| TargetError(format!("{reader:?} is not a valid bearer token")))?,
        );
        let mut stream = self
            .client
            .stream_session(req)
            .await
            .map_err(|s| TargetError(format!("StreamSession: {}", s.message())))?
            .into_inner();
        let mut out = Vec::new();
        // The subscription replays history then stays open for live traffic,
        // with no end-of-replay marker: read until it goes quiet.
        loop {
            match tokio::time::timeout(self.stream_idle, stream.message()).await {
                Err(_) => break,
                Ok(Ok(None)) => break,
                Ok(Ok(Some(resp))) => match resp.response {
                    Some(stream_session_response::Response::Envelope(env)) => out.push(env),
                    Some(stream_session_response::Response::Error(e)) => {
                        return Err(TargetError(format!(
                            "StreamSession error {}: {}",
                            e.code, e.message
                        )))
                    }
                    None => {}
                },
                Ok(Err(s)) => return Err(TargetError(format!("StreamSession: {}", s.message()))),
            }
        }
        drop(tx);
        Ok(out)
    }
}

fn state_name(state: i32) -> String {
    match SessionState::try_from(state) {
        Ok(SessionState::Open) => "Open".into(),
        Ok(SessionState::Resolved) => "Resolved".into(),
        Ok(SessionState::Expired) => "Expired".into(),
        Ok(SessionState::Suspended) => "Suspended".into(),
        Ok(SessionState::Cancelled) => "Cancelled".into(),
        _ => format!("unknown({state})"),
    }
}

#[async_trait]
impl ConformanceTarget for GrpcTarget {
    fn profile(&self) -> Profile {
        Profile::Server
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn describe(&self) -> String {
        self.url.clone()
    }

    async fn begin_fixture(&mut self, _clock_start: Option<i64>) -> Result<(), TargetError> {
        Ok(())
    }

    async fn register_policy(&mut self, def: PolicyDefinition) -> Result<(), TargetError> {
        let descriptor = PolicyDescriptor {
            policy_id: def.policy_id.clone(),
            mode: def.mode.clone(),
            description: def.description.clone(),
            rules: serde_json::to_string(&def.rules)
                .map_err(|e| TargetError(format!("policy rules: {e}")))?,
            schema_version: def.schema_version,
            registered_at_unix_ms: 0,
        };
        let register = |this: &Self| {
            this.request(
                ADMIN_SENDER,
                RegisterPolicyRequest {
                    policy_descriptor: Some(descriptor.clone()),
                },
            )
        };
        let first = self
            .client
            .register_policy(register(self)?)
            .await
            .map_err(|s| TargetError(format!("RegisterPolicy: {}", s.message())))?
            .into_inner();
        if first.ok {
            return Ok(());
        }
        // Most likely already registered by an earlier run: replace it.
        let req = self.request(
            ADMIN_SENDER,
            UnregisterPolicyRequest {
                policy_id: def.policy_id.clone(),
            },
        )?;
        let _ = self.client.unregister_policy(req).await;
        let second = self
            .client
            .register_policy(register(self)?)
            .await
            .map_err(|s| TargetError(format!("RegisterPolicy: {}", s.message())))?
            .into_inner();
        if second.ok {
            Ok(())
        } else {
            Err(TargetError(format!(
                "RegisterPolicy {} refused: {} (first attempt: {})",
                def.policy_id, second.error, first.error
            )))
        }
    }

    async fn submit(&mut self, envelope: Envelope) -> Result<Submission, TargetError> {
        let sender = envelope.sender.clone();
        let req = self.request(
            &sender,
            SendRequest {
                envelope: Some(envelope),
            },
        )?;
        match self.client.send(req).await {
            Ok(resp) => {
                let ack = resp
                    .into_inner()
                    .ack
                    .ok_or_else(|| TargetError("SendResponse has no ack".into()))?;
                if ack.ok {
                    Ok(Submission::Accepted {
                        duplicate: ack.duplicate,
                    })
                } else {
                    Ok(Submission::Rejected {
                        code: ack.error.map(|e| e.code).unwrap_or_default(),
                    })
                }
            }
            // A coded status is a protocol rejection; anything else is the
            // transport failing.
            Err(status) => match status
                .metadata()
                .get("macp-error-code")
                .and_then(|v| v.to_str().ok())
            {
                Some(code) => Ok(Submission::Rejected { code: code.into() }),
                None => Err(TargetError(format!(
                    "Send: {:?} {}",
                    status.code(),
                    status.message()
                ))),
            },
        }
    }

    async fn observe(
        &mut self,
        session_id: &str,
        initiator: &str,
    ) -> Result<Observation, TargetError> {
        let req = self.request(
            initiator,
            GetSessionRequest {
                session_id: session_id.to_string(),
            },
        )?;
        let meta = self
            .client
            .get_session(req)
            .await
            .map_err(|s| TargetError(format!("GetSession: {}", s.message())))?
            .into_inner()
            .metadata
            .ok_or_else(|| TargetError("GetSessionResponse has no metadata".into()))?;
        let state = state_name(meta.state);
        let history = self.history(session_id, initiator).await?;
        let resolution = if state == "Resolved" {
            match history
                .iter()
                .rev()
                .find(|e| e.message_type == "Commitment")
            {
                Some(c) => Observable::Observed(Some(c.payload.clone())),
                None => Observable::Unobservable,
            }
        } else {
            Observable::Observed(None)
        };
        let mut obs = Observation::new(state).with_history(
            history
                .iter()
                .filter(|e| e.message_type != "SessionStart")
                .map(HistoryEntry::from_envelope)
                .collect(),
        );
        obs.resolution = resolution;
        Ok(obs)
    }
}
