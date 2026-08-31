use crate::common;
use macp_integration_tests::helpers::*;
use macp_integration_tests::macp_tools::{self, decision::*};
use rig::prelude::*;
use rig::providers::openai;

/// Realistic multi-agent decision coordination following the MACP spec.
///
/// Architecture (per RFC-MACP-0001 and RFC-MACP-0007):
///   - Orchestrator: plain code (deterministic session lifecycle)
///   - Specialists: LLM-powered (reasoning happens OUTSIDE the session,
///     only the resulting Evaluation Envelope enters the session)
///   - Agents run in PARALLEL (runtime serializes by acceptance order)
///
/// Scenario: Suspicious $4,800 wire transfer. Orchestrator proposes step-up
/// verification. Three specialists (fraud, growth, compliance) evaluate
/// concurrently. Orchestrator commits.
#[tokio::test]
#[ignore]
async fn real_llm_agents_coordinate_decision() {
    if std::env::var("OPENAI_API_KEY").is_err() {
        eprintln!("SKIPPED: OPENAI_API_KEY not set");
        return;
    }

    let ep = common::endpoint().await;
    let sid = new_session_id();
    let orchestrator_id = "agent://checkout.orchestrator";
    let fraud_id = "agent://fraud";
    let growth_id = "agent://growth";
    let compliance_id = "agent://compliance";

    eprintln!("═══════════════════════════════════════════════════════════════");
    eprintln!("  MACP Decision Mode E2E Test — Wire Transfer Review");
    eprintln!("  Session: {sid}");
    eprintln!("  Orchestrator: plain code (no LLM)");
    eprintln!("  Specialists:  3x GPT-4o-mini (LLM reasoning OUTSIDE session)");
    eprintln!("  Agents run:   IN PARALLEL (runtime serializes on arrival)");
    eprintln!("═══════════════════════════════════════════════════════════════");

    // ── Orchestrator gRPC client (no LLM needed) ────────────────────────
    let orch_client = macp_tools::shared_client(ep).await;

    // ═══════════════════════════════════════════════════════════════════
    // STEP 1: Orchestrator starts session + proposes (deterministic)
    // ═══════════════════════════════════════════════════════════════════
    eprintln!("\n── Step 1: Orchestrator starts session and proposes ──────────");
    {
        let mut client = orch_client.lock().await;

        let ack = send_as(
            &mut client,
            orchestrator_id,
            envelope(
                MODE_DECISION,
                "SessionStart",
                &new_message_id(),
                &sid,
                orchestrator_id,
                session_start_payload(
                    "Review suspicious wire transfer requiring step-up verification",
                    &[orchestrator_id, fraud_id, growth_id, compliance_id],
                    60_000,
                ),
            ),
        )
        .await
        .expect("SessionStart should succeed");
        assert!(ack.ok);
        eprintln!("   SessionStart accepted. State: OPEN");

        let ack = send_as(
            &mut client,
            orchestrator_id,
            envelope(
                MODE_DECISION,
                "Proposal",
                &new_message_id(),
                &sid,
                orchestrator_id,
                proposal_payload(
                    "transfer-review",
                    "Require step-up verification before processing the $4,800 wire transfer",
                    "Transaction amount and pattern triggered fraud detection threshold",
                ),
            ),
        )
        .await
        .expect("Proposal should succeed");
        assert!(ack.ok);
        eprintln!("   Proposal accepted. Phase → Evaluation");
    }

    // ═══════════════════════════════════════════════════════════════════
    // STEP 2: Specialist agents evaluate IN PARALLEL
    //
    // Per RFC-MACP-0001 §8.1: agents operate independently.
    // LLM reasoning happens OUTSIDE the session (ambient plane).
    // Only the resulting Evaluation Envelope enters the session.
    // Runtime serializes by acceptance order.
    // ═══════════════════════════════════════════════════════════════════
    eprintln!("\n── Step 2: 3 specialists evaluate IN PARALLEL ──────────────");
    eprintln!("   LLM reasoning happens outside the session (ambient plane)");
    eprintln!("   Runtime serializes Evaluations by acceptance order\n");

    let openai_client = openai::Client::from_env();

    let scenario = "A customer is attempting a $4,800 wire transfer that triggered fraud alerts. \
                    The orchestrator proposes requiring step-up verification before processing.";

    // Build all 3 agents
    let fraud_client = macp_tools::shared_client(ep).await;
    let fraud_agent = openai_client
        .agent(openai::GPT_4O_MINI)
        .preamble(
            "You are a fraud detection specialist at a bank. Assess transaction proposals \
             from a fraud-risk perspective. Call macp_evaluate with your assessment.\n\
             For recommendation use: APPROVE, REVIEW, BLOCK, or REJECT.\n\
             For confidence use 0.0 to 1.0. Use EXACT session_id and proposal_id from prompt.",
        )
        .tool(EvaluateTool {
            client: fraud_client,
            agent_id: fraud_id.into(),
        })
        .build();

    let growth_client = macp_tools::shared_client(ep).await;
    let growth_agent = openai_client
        .agent(openai::GPT_4O_MINI)
        .preamble(
            "You are a customer experience and growth specialist at a bank. Assess transaction \
             proposals from a UX and business growth perspective. Call macp_evaluate.\n\
             For recommendation use: APPROVE, REVIEW, BLOCK, or REJECT.\n\
             For confidence use 0.0 to 1.0. Use EXACT session_id and proposal_id from prompt.",
        )
        .tool(EvaluateTool {
            client: growth_client,
            agent_id: growth_id.into(),
        })
        .build();

    let compliance_client = macp_tools::shared_client(ep).await;
    let compliance_agent = openai_client
        .agent(openai::GPT_4O_MINI)
        .preamble(
            "You are a regulatory compliance specialist at a bank. Assess transaction proposals \
             from a regulatory and legal perspective. Call macp_evaluate.\n\
             For recommendation use: APPROVE, REVIEW, BLOCK, or REJECT.\n\
             For confidence use 0.0 to 1.0. Use EXACT session_id and proposal_id from prompt.",
        )
        .tool(EvaluateTool {
            client: compliance_client,
            agent_id: compliance_id.into(),
        })
        .build();

    let eval_prompt = |domain: &str| {
        format!(
            "{scenario}\n\nEvaluate from your {domain} perspective. Call macp_evaluate with:\n\
             - session_id: {sid}\n\
             - proposal_id: \"transfer-review\"\n\
             - recommendation, confidence, reason: use your professional judgment"
        )
    };

    // Launch all 3 evaluations concurrently with tokio::join!
    //
    // The per-agent budget guards against an indefinite hang; it is not a
    // latency SLO for OpenAI. At the original 30s it was itself a flake source
    // — an unloaded run of this test measured 28.6s for the parallel batch, and
    // a later run failed with "Compliance agent timed out". The `e2e` CI job's
    // own 10-minute cap is the real backstop.
    const AGENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
    const AGENT_ATTEMPTS: u32 = 3;
    let start = std::time::Instant::now();
    eprintln!("   Launching 3 LLM agents concurrently...");

    // Bound outside `join!` so the prompt strings outlive the borrowed futures.
    let fraud_prompt = eval_prompt("fraud-risk");
    let growth_prompt = eval_prompt("customer-experience and growth");
    let compliance_prompt = eval_prompt("regulatory compliance");

    let (fraud_result, growth_result, compliance_result) = tokio::join!(
        super::prompt_with_retry(
            "Fraud",
            &fraud_agent,
            &fraud_prompt,
            AGENT_TIMEOUT,
            AGENT_ATTEMPTS,
        ),
        super::prompt_with_retry(
            "Growth",
            &growth_agent,
            &growth_prompt,
            AGENT_TIMEOUT,
            AGENT_ATTEMPTS,
        ),
        super::prompt_with_retry(
            "Compliance",
            &compliance_agent,
            &compliance_prompt,
            AGENT_TIMEOUT,
            AGENT_ATTEMPTS,
        ),
    );

    let parallel_duration = start.elapsed();
    eprintln!(
        "   All 3 agents completed in {:.1}s (parallel)\n",
        parallel_duration.as_secs_f64()
    );

    // `prompt_with_retry` panics if an agent cannot be reached at all, so a
    // returned value here is a real reply.
    eprintln!("   [Fraud]      {fraud_result}\n");
    eprintln!("   [Growth]     {growth_result}\n");
    eprintln!("   [Compliance] {compliance_result}\n");

    // ═══════════════════════════════════════════════════════════════════
    // STEP 3: Orchestrator commits (deterministic)
    // ═══════════════════════════════════════════════════════════════════
    eprintln!("── Step 3: Orchestrator commits (deterministic) ─────────────");
    {
        let mut client = orch_client.lock().await;
        let ack = send_as(
            &mut client,
            orchestrator_id,
            envelope(
                MODE_DECISION,
                "Commitment",
                &new_message_id(),
                &sid,
                orchestrator_id,
                commitment_payload(
                    "cmt-001",
                    "transfer.step-up-verification",
                    "checkout-payments",
                    "Specialist agents evaluated — proceeding with step-up verification",
                    true,
                ),
            ),
        )
        .await
        .expect("Commitment should succeed");
        assert!(ack.ok);
        assert_eq!(ack.session_state, 2);
        eprintln!("   Committed. Session → RESOLVED");
    }

    // ═══════════════════════════════════════════════════════════════════
    // STEP 4: Verify
    // ═══════════════════════════════════════════════════════════════════
    eprintln!("\n── Step 4: Verify ──────────────────────────────────────────");
    {
        let mut client = orch_client.lock().await;
        let resp = get_session_as(&mut client, orchestrator_id, &sid)
            .await
            .expect("GetSession should succeed");
        let meta = resp.metadata.expect("metadata present");
        assert_eq!(meta.state, 2);
        // RESOLVED alone does not prove the specialists were heard: decision
        // mode's `commitment_ready` requires only a Proposal, so this exact
        // sequence resolves with zero Evaluations. Assert the LLM-produced
        // Evaluations actually entered accepted history.
        super::assert_participants_contributed(&meta, &[fraud_id, growth_id, compliance_id], 1);
        eprintln!("   Session state: {} (RESOLVED)", meta.state);
        eprintln!("   Verified: all 3 specialist Evaluations are in accepted history");
    }

    eprintln!("\n═══════════════════════════════════════════════════════════════");
    eprintln!("  PASSED");
    eprintln!("  Orchestrator (code) proposed");
    eprintln!(
        "  → 3 specialists (LLM) evaluated IN PARALLEL ({:.1}s)",
        parallel_duration.as_secs_f64()
    );
    eprintln!("  → Orchestrator (code) committed");
    eprintln!("  LLM reasoning happened OUTSIDE session (ambient plane)");
    eprintln!("  Runtime serialized Evaluations by acceptance order");
    eprintln!("═══════════════════════════════════════════════════════════════");
}
