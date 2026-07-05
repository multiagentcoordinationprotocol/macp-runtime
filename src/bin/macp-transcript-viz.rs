//! E6: render a session transcript as a Mermaid sequence diagram.
//!
//! Input (positional arg): a conformance fixture (`tests/conformance/*.json`)
//! or a session log (`log.jsonl` from `MACP_DATA_DIR/sessions/<id>/`).
//! Output: a Mermaid `sequenceDiagram` on stdout — paste into any Mermaid
//! renderer, docs page, or GitHub markdown block.
//!
//! ```bash
//! macp-transcript-viz tests/conformance/decision_happy_path.json
//! macp-transcript-viz .macp-data/sessions/<session-id>/log.jsonl
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: macp-transcript-viz <fixture.json | log.jsonl>");
        std::process::exit(2);
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            std::process::exit(1);
        }
    };
    let diagram = if path.ends_with(".jsonl") {
        render_log(&raw)
    } else {
        render_fixture(&raw)
    };
    match diagram {
        Ok(d) => println!("{d}"),
        Err(e) => {
            eprintln!("cannot render {path}: {e}");
            std::process::exit(1);
        }
    }
}

/// Stable short alias for a participant (Mermaid participant ids must be
/// simple identifiers; senders are URIs).
fn alias_for(aliases: &mut BTreeMap<String, String>, sender: &str) -> String {
    if let Some(a) = aliases.get(sender) {
        return a.clone();
    }
    let alias = format!("P{}", aliases.len());
    aliases.insert(sender.to_string(), alias.clone());
    alias
}

fn header(aliases: &BTreeMap<String, String>) -> String {
    let mut out = String::from("sequenceDiagram\n");
    out.push_str("    participant RT as runtime\n");
    for (sender, alias) in aliases {
        let _ = writeln!(out, "    participant {alias} as {sender}");
    }
    out
}

/// A conformance fixture: scripted messages with accept/reject expectations.
fn render_fixture(raw: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let mode = v["mode"].as_str().unwrap_or("?");
    let messages = v["messages"]
        .as_array()
        .ok_or("fixture has no messages array")?;

    let mut aliases = BTreeMap::new();
    let mut body = String::new();
    let initiator = v["initiator"].as_str().unwrap_or("?");
    let init_alias = alias_for(&mut aliases, initiator);
    let _ = writeln!(body, "    {init_alias}->>RT: SessionStart [{mode}]");

    for msg in messages {
        let sender = msg["sender"].as_str().unwrap_or("?");
        let mtype = msg["message_type"].as_str().unwrap_or("?");
        let accept = msg["expect"].as_str().unwrap_or("accept") == "accept";
        let alias = alias_for(&mut aliases, sender);
        if accept {
            let _ = writeln!(body, "    {alias}->>RT: {mtype}");
        } else {
            let _ = writeln!(body, "    {alias}--xRT: {mtype} (rejected)");
        }
    }
    if let Some(state) = v["expected_final_state"].as_str() {
        let _ = writeln!(body, "    Note over RT: session {state}");
    }
    Ok(header(&aliases) + &body)
}

/// A session log: one JSON `LogEntry` per line.
fn render_log(raw: &str) -> Result<String, String> {
    let mut aliases = BTreeMap::new();
    let mut body = String::new();
    let mut rendered = 0usize;
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // corrupt lines are skipped, like the runtime does
        };
        let kind = entry["entry_kind"].as_str().unwrap_or("Incoming");
        let mtype = entry["message_type"].as_str().unwrap_or("?");
        match kind {
            "Incoming" => {
                let sender = entry["sender"].as_str().unwrap_or("?");
                let alias = alias_for(&mut aliases, sender);
                let _ = writeln!(body, "    {alias}->>RT: {mtype}");
            }
            "Internal" => {
                let _ = writeln!(body, "    Note over RT: {mtype}");
            }
            _ => {} // checkpoints carry no transcript information
        }
        rendered += 1;
    }
    if rendered == 0 {
        return Err("no renderable log entries".into());
    }
    Ok(header(&aliases) + &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fixture in tests/conformance renders to structurally valid
    /// Mermaid: starts with `sequenceDiagram`, declares every participant it
    /// references, one arrow per scripted message.
    #[test]
    fn all_conformance_fixtures_render() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("json")
                || path.file_name().and_then(|n| n.to_str()) == Some("schema.json")
            {
                continue;
            }
            let raw = std::fs::read_to_string(&path).unwrap();
            let diagram = render_fixture(&raw)
                .unwrap_or_else(|e| panic!("{} failed to render: {e}", path.display()));
            assert!(diagram.starts_with("sequenceDiagram\n"));

            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            let msg_count = v["messages"].as_array().unwrap().len();
            let arrows = diagram
                .lines()
                .filter(|l| l.contains("->>RT:") || l.contains("--xRT:"))
                .count();
            // +1 for the SessionStart line.
            assert_eq!(arrows, msg_count + 1, "{}", path.display());

            // Every referenced alias is declared.
            for line in diagram
                .lines()
                .filter(|l| l.contains("->>RT:") || l.contains("--xRT:"))
            {
                let alias = line.trim().split(['-', ' ']).next().unwrap();
                assert!(
                    diagram.contains(&format!("participant {alias} ")),
                    "{}: undeclared participant {alias}",
                    path.display()
                );
            }
            checked += 1;
        }
        assert!(checked >= 13, "all fixtures rendered, got {checked}");
    }

    #[test]
    fn log_rendering_handles_internal_entries_and_corrupt_lines() {
        let log = r#"{"message_id":"m1","received_at_ms":1,"sender":"agent://a","message_type":"SessionStart","raw_payload":[],"entry_kind":"Incoming"}
not json
{"message_id":"","received_at_ms":2,"sender":"_runtime","message_type":"TtlExpired","raw_payload":[],"entry_kind":"Internal"}"#;
        let d = render_log(log).unwrap();
        assert!(d.contains("P0->>RT: SessionStart"));
        assert!(d.contains("Note over RT: TtlExpired"));
    }
}
