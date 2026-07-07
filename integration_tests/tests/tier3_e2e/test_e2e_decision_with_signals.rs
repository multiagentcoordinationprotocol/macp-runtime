use crate::common;
use macp_integration_tests::helpers::*;
use macp_integration_tests::macp_tools::{self, decision::*};
use macp_runtime::pb::{Envelope, SendRequest, SignalPayload, WatchSignalsRequest};
use prost::Message;
use rig::completion::Prompt;
use rig::prelude::*;
use rig::providers::openai;
use tonic::Request;

fn with_sender<T>(sender: &str, inner: T) -> Request<T> {
    let mut request = Request::new(inner);
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {sender}")
            .parse()
            .expect("valid auth header"),
    );
    request
}

/// Send a Signal envelope through the runtime.
async fn send_signal(
    client: &mut macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient<
        tonic::transport::Channel,
    >,
    sender_id: &str,
    signal_type: &str,
    data: &str,
    confidence: f64,
    correlation_session_id: &str,
) {
    let payload = SignalPayload {
        signal_type: signal_type.into(),
        data: data.as_bytes().to_vec(),
        confidence,
        correlation_session_id: correlation_session_id.into(),
    };
    let env = Envelope {
        macp_version: "1.0".into(),
        mode: String::new(),
        message_type: "Signal".into(),
        message_id: new_message_id(),
        session_id: String::new(),
        sender: String::new(),
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        payload: payload.encode_to_vec(),
    };
    let resp = client
        .send(with_sender(
            sender_id,
            SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ack.unwrap().ok, "Signal should be accepted");
}

/// Full E2E showcasing both Coordination Plane (session) and Ambient Plane (signals)
/// running simultaneously.
///
/// Scenario: Wire transfer review with 3 specialist LLM agents.
/// Each specialist sends progress Signals BEFORE submitting their Evaluation.
/// The orchestrator watches the signal stream to observe real-time progress.
///
/// Flow:
///   Orchestrator subscribes to WatchSignals
///   Orchestrator starts session + proposes (code, no LLM)
///   Each specialist:
///     1. Sends a "progress" Signal: "starting analysis" (ambient plane)
///     2. LLM reasons about the proposal (outside session)
///     3. Rig tool sends Evaluation into session (coordination plane)
///     4. Sends a "completed" Signal (ambient plane)
///   Orchestrator collects signals + commits (code, no LLM)
#[tokio::test]
#[ignore]
async fn decision_with_signals_full_flow() {
    if std::env::var("OPENAI_API_KEY").is_err() {
        eprintln!("SKIPPED: OPENAI_API_KEY not set");
        return;
    }

    let ep = common::endpoint().await;
    let sid = new_session_id();
    let orch_id = "agent://checkout.orchestrator";
    let fraud_id = "agent://fraud";
    let growth_id = "agent://growth";
    let compliance_id = "agent://compliance";

    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║  MACP E2E: Decision Mode + Signals (Ambient + Coordination) ║");
    eprintln!("║  Session: {sid}  ║");
    eprintln!("╚══════════════════════════════════════════════════════════════╝");
    eprintln!();

    let orch_client = macp_tools::shared_client(ep).await;

    // ═══════════════════════════════════════════════════════════════════
    // Orchestrator subscribes to WatchSignals BEFORE session starts
    // ═══════════════════════════════════════════════════════════════════
    eprintln!("── Orchestrator subscribes to WatchSignals stream ───────────");
    let mut signal_watcher =
        macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient::connect(
            ep.to_string(),
        )
        .await
        .unwrap();
    // WatchSignals requires authentication since v0.5.0 (ambient payloads are
    // agent data): subscribe as the orchestrator identity.
    let mut signal_stream = signal_watcher
        .watch_signals(with_sender(orch_id, WatchSignalsRequest {}))
        .await
        .unwrap()
        .into_inner();
    eprintln!("   Stream opened. Orchestrator will see all ambient Signals.\n");

    // ═══════════════════════════════════════════════════════════════════
    // STEP 1: Orchestrator starts session + proposes (code, no LLM)
    // ═══════════════════════════════════════════════════════════════════
    eprintln!("── STEP 1: Orchestrator starts session and proposes ─────────");
    eprintln!("   (Coordination Plane — these enter session history)");
    {
        let mut client = orch_client.lock().await;
        let ack = send_as(
            &mut client,
            orch_id,
            envelope(
                MODE_DECISION,
                "SessionStart",
                &new_message_id(),
                &sid,
                orch_id,
                session_start_payload(
                    "Review suspicious $4,800 wire transfer",
                    &[orch_id, fraud_id, growth_id, compliance_id],
                    60_000,
                ),
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok);
        eprintln!("   → [Session History #1] SessionStart from orchestrator");

        let ack = send_as(
            &mut client,
            orch_id,
            envelope(
                MODE_DECISION,
                "Proposal",
                &new_message_id(),
                &sid,
                orch_id,
                proposal_payload(
                    "transfer-review",
                    "Require step-up verification for $4,800 wire transfer",
                    "Fraud detection threshold triggered",
                ),
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok);
        eprintln!("   → [Session History #2] Proposal from orchestrator");
    }
    eprintln!();

    // ═══════════════════════════════════════════════════════════════════
    // STEP 2: Specialists send signals + evaluate IN PARALLEL
    // ═══════════════════════════════════════════════════════════════════
    eprintln!("── STEP 2: Specialists analyze (signals + evaluations) ──────");
    eprintln!("   Each agent: Signal(starting) → LLM reasons → Evaluation → Signal(done)\n");

    let openai_client = openai::Client::from_env();
    let scenario =
        "A $4,800 wire transfer triggered fraud alerts. Proposal: require step-up verification.";

    // Build specialist agents
    let fraud_grpc = macp_tools::shared_client(ep).await;
    let fraud_agent = openai_client
        .agent(openai::GPT_4O_MINI)
        .preamble("You are a fraud detection specialist. Call macp_evaluate with your assessment. Use EXACT session_id and proposal_id from prompt. For recommendation: APPROVE, REVIEW, BLOCK, or REJECT. Confidence: 0.0-1.0.")
        .tool(EvaluateTool { client: fraud_grpc.clone(), agent_id: fraud_id.into() })
        .build();

    let growth_grpc = macp_tools::shared_client(ep).await;
    let growth_agent = openai_client
        .agent(openai::GPT_4O_MINI)
        .preamble("You are a growth/UX specialist. Call macp_evaluate with your assessment. Use EXACT session_id and proposal_id from prompt. For recommendation: APPROVE, REVIEW, BLOCK, or REJECT. Confidence: 0.0-1.0.")
        .tool(EvaluateTool { client: growth_grpc.clone(), agent_id: growth_id.into() })
        .build();

    let compliance_grpc = macp_tools::shared_client(ep).await;
    let compliance_agent = openai_client
        .agent(openai::GPT_4O_MINI)
        .preamble("You are a compliance specialist. Call macp_evaluate with your assessment. Use EXACT session_id and proposal_id from prompt. For recommendation: APPROVE, REVIEW, BLOCK, or REJECT. Confidence: 0.0-1.0.")
        .tool(EvaluateTool { client: compliance_grpc.clone(), agent_id: compliance_id.into() })
        .build();

    let eval_prompt = |domain: &str| {
        format!(
            "{scenario}\n\nEvaluate from your {domain} perspective. Call macp_evaluate with:\n\
             - session_id: {sid}\n\
             - proposal_id: \"transfer-review\"\n\
             - recommendation, confidence, reason: use your judgment"
        )
    };

    // Each specialist: Signal(starting) → LLM evaluate → Signal(done)
    // All 3 run in parallel
    let sid_clone = sid.clone();
    let ep_str = ep.to_string();

    let start = std::time::Instant::now();

    let (fraud_res, growth_res, compliance_res) = tokio::join!(
        // Fraud agent flow
        async {
            let mut sig_client =
                macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient::connect(
                    ep_str.clone(),
                )
                .await
                .unwrap();
            send_signal(
                &mut sig_client,
                fraud_id,
                "progress",
                "starting fraud risk analysis",
                0.0,
                &sid_clone,
            )
            .await;
            let result = fraud_agent.prompt(&eval_prompt("fraud-risk")).await;
            send_signal(
                &mut sig_client,
                fraud_id,
                "completed",
                "fraud evaluation submitted",
                1.0,
                &sid_clone,
            )
            .await;
            result
        },
        // Growth agent flow
        async {
            let mut sig_client =
                macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient::connect(
                    ep_str.clone(),
                )
                .await
                .unwrap();
            send_signal(
                &mut sig_client,
                growth_id,
                "progress",
                "starting customer impact analysis",
                0.0,
                &sid_clone,
            )
            .await;
            let result = growth_agent
                .prompt(&eval_prompt("customer-experience"))
                .await;
            send_signal(
                &mut sig_client,
                growth_id,
                "completed",
                "growth evaluation submitted",
                1.0,
                &sid_clone,
            )
            .await;
            result
        },
        // Compliance agent flow
        async {
            let mut sig_client =
                macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient::connect(
                    ep_str.clone(),
                )
                .await
                .unwrap();
            send_signal(
                &mut sig_client,
                compliance_id,
                "progress",
                "starting regulatory review",
                0.0,
                &sid_clone,
            )
            .await;
            let result = compliance_agent
                .prompt(&eval_prompt("regulatory-compliance"))
                .await;
            send_signal(
                &mut sig_client,
                compliance_id,
                "completed",
                "compliance evaluation submitted",
                1.0,
                &sid_clone,
            )
            .await;
            result
        },
    );

    let parallel_duration = start.elapsed();

    // Log LLM results
    match &fraud_res {
        Ok(text) => eprintln!("   [Fraud LLM]      {text}"),
        Err(e) => panic!("Fraud agent failed: {e}"),
    }
    match &growth_res {
        Ok(text) => eprintln!("   [Growth LLM]     {text}"),
        Err(e) => panic!("Growth agent failed: {e}"),
    }
    match &compliance_res {
        Ok(text) => eprintln!("   [Compliance LLM] {text}"),
        Err(e) => panic!("Compliance agent failed: {e}"),
    }
    eprintln!();
    eprintln!(
        "   All 3 specialists completed in {:.1}s (parallel)\n",
        parallel_duration.as_secs_f64()
    );

    // ═══════════════════════════════════════════════════════════════════
    // STEP 3: Drain the signal stream — show what orchestrator observed
    // ═══════════════════════════════════════════════════════════════════
    eprintln!("── STEP 3: Orchestrator reads signal stream ─────────────────");
    eprintln!("   (Ambient Plane — these did NOT enter session history)\n");

    let mut signal_count = 0;
    while let Ok(Ok(Some(resp))) = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        signal_stream.message(),
    )
    .await
    {
        let env = resp.envelope.unwrap();
        let payload = SignalPayload::decode(&*env.payload).unwrap_or_default();
        signal_count += 1;
        eprintln!(
            "   Signal #{signal_count}: sender={:<30} type={:<12} data=\"{}\" confidence={}",
            env.sender,
            payload.signal_type,
            String::from_utf8_lossy(&payload.data),
            payload.confidence,
        );
    }
    eprintln!();
    eprintln!("   Total Signals received: {signal_count}");
    eprintln!("   These Signals are ambient — session history is unaffected.\n");

    // ═══════════════════════════════════════════════════════════════════
    // STEP 4: Orchestrator commits (code, no LLM)
    // ═══════════════════════════════════════════════════════════════════
    eprintln!("── STEP 4: Orchestrator commits (Coordination Plane) ────────");
    {
        let mut client = orch_client.lock().await;
        let ack = send_as(
            &mut client,
            orch_id,
            envelope(
                MODE_DECISION,
                "Commitment",
                &new_message_id(),
                &sid,
                orch_id,
                commitment_payload(
                    "cmt-001",
                    "transfer.step-up-verification",
                    "checkout-payments",
                    "All specialists evaluated — proceeding with step-up verification",
                    true,
                ),
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok);
        assert_eq!(ack.session_state, 2);
        eprintln!("   → [Session History #6] Commitment from orchestrator");
        eprintln!("   → Session state: RESOLVED");
    }

    // ═══════════════════════════════════════════════════════════════════
    // STEP 5: Verify
    // ═══════════════════════════════════════════════════════════════════
    eprintln!();
    eprintln!("── STEP 5: Final verification ───────────────────────────────");
    {
        let mut client = orch_client.lock().await;
        let resp = get_session_as(&mut client, orch_id, &sid).await.unwrap();
        let meta = resp.metadata.unwrap();
        assert_eq!(meta.state, 2);
        eprintln!("   Session state: {} (RESOLVED)", meta.state);
    }

    eprintln!();
    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║  PASSED                                                      ║");
    eprintln!("║                                                              ║");
    eprintln!("║  Coordination Plane (session history — binding):             ║");
    eprintln!("║    [1] SessionStart  (orchestrator, code)                    ║");
    eprintln!("║    [2] Proposal      (orchestrator, code)                    ║");
    eprintln!("║    [3] Evaluation    (fraud, LLM)                            ║");
    eprintln!("║    [4] Evaluation    (growth, LLM)                           ║");
    eprintln!("║    [5] Evaluation    (compliance, LLM)                       ║");
    eprintln!("║    [6] Commitment    (orchestrator, code)                    ║");
    eprintln!("║                                                              ║");
    eprintln!("║  Ambient Plane (signals — non-binding):                      ║");
    eprintln!("║    {signal_count} signals observed (progress + completed from each agent)  ║");
    eprintln!("║    Signals correlate with session but do NOT enter history    ║");
    eprintln!("║                                                              ║");
    eprintln!(
        "║  Agents: {:.1}s parallel | LLM used only for evaluations     ║",
        parallel_duration.as_secs_f64()
    );
    eprintln!("╚══════════════════════════════════════════════════════════════╝");
}
