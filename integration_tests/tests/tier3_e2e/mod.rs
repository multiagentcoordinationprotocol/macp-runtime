mod test_e2e_decision;
mod test_e2e_decision_with_signals;
mod test_e2e_task;

use macp_runtime::pb::SessionMetadata;
use std::future::IntoFuture;
use std::time::Duration;

/// Prompt an agent, retrying transient provider failures, and return its reply.
///
/// Tier 3 exists to exercise the MACP runtime, but it reaches a third-party API
/// over the network to do it. Both observed failure modes here were external:
/// an HTTP 503 from the OpenAI edge ("upstream connect error ... Connection
/// refused"), and a slow response against what used to be a 30s budget. Neither
/// is a runtime defect, and failing the suite on either makes Tier 3 worthless
/// as a release signal — so both are retried with linear backoff.
///
/// The timeout is a hang guard, not a latency SLO; the `e2e` CI job's own
/// 10-minute cap is the outer backstop. A genuine agent failure still fails the
/// test, after `max_attempts`, with every attempt's error reported.
pub async fn prompt_with_retry<A>(
    label: &str,
    agent: &A,
    prompt: &str,
    timeout: Duration,
    max_attempts: u32,
) -> String
where
    A: rig::completion::Prompt,
{
    let mut last = String::new();
    for attempt in 1..=max_attempts {
        match tokio::time::timeout(timeout, agent.prompt(prompt).into_future()).await {
            Ok(Ok(text)) => return text,
            Ok(Err(e)) => last = format!("provider error: {e}"),
            Err(_) => last = format!("timed out after {}s", timeout.as_secs()),
        }
        eprintln!("   [{label}] attempt {attempt}/{max_attempts} failed: {last}");
        if attempt < max_attempts {
            tokio::time::sleep(Duration::from_secs(2 * u64::from(attempt))).await;
        }
    }
    panic!("[{label}] failed after {max_attempts} attempts; last error: {last}");
}

/// Assert that each named participant landed at least `min` accepted messages
/// in the session's history.
///
/// Tier 3 drives these participants with a real LLM, so "the agent replied that
/// it evaluated" is not evidence. The Rig tools surface a runtime *rejection* as
/// a **successful** tool call carrying `ok: false` (see `EvaluateTool::call`),
/// and the model reports success either way — so the agent's prose says nothing
/// about whether the message entered accepted history. Only history does.
///
/// `message_count` comes from `Session::record_participant_activity`, which runs
/// only in `macp_modes::step::commit`. `SessionStart` creates the session
/// outside that path, so it is not counted — a participant's count is its
/// accepted messages *after* session creation.
pub fn assert_participants_contributed(meta: &SessionMetadata, participants: &[&str], min: u32) {
    let observed = |id: &str| -> u32 {
        meta.participant_activity
            .iter()
            .find(|p| p.participant_id == id)
            .map_or(0, |p| p.message_count)
    };

    let short: Vec<(&str, u32)> = participants
        .iter()
        .copied()
        .map(|id| (id, observed(id)))
        .filter(|(_, count)| *count < min)
        .collect();

    assert!(
        short.is_empty(),
        "these participants landed fewer than {min} accepted message(s): {short:?}\n\
         full participant_activity: {:?}",
        meta.participant_activity
            .iter()
            .map(|p| (p.participant_id.as_str(), p.message_count))
            .collect::<Vec<_>>()
    );
}
