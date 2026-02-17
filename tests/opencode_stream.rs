//! Integration test: connect to a real OpenCode server and verify the
//! SSE stream works end-to-end with bytes_stream() parsing.
//!
//! Requires an OpenCode server running. Skips if none found.

use futures::StreamExt;
use reqwest::Client;
use spacebot::opencode::types::*;

/// Find a running OpenCode server by checking common ports.
async fn find_server() -> Option<(String, String)> {
    let client = Client::builder().build().ok()?;

    for port in [15728u16, 37182] {
        let url = format!("http://127.0.0.1:{port}");
        let health = client
            .get(format!("{url}/global/health"))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await;
        if let Ok(resp) = health {
            if resp.status().is_success() {
                // Find the directory from the session list or use a known one
                // Find the directory -- try known ones
                let dirs = [
                    "/Users/jamespine/Projects/opencode-memes-3",
                    "/Users/jamespine/Projects/opencode-memes-2",
                ];
                for dir in dirs {
                    if std::path::Path::new(dir).exists() {
                        return Some((url, dir.into()));
                    }
                }
                return Some((url, "/tmp".into()));
            }
        }
    }
    None
}

#[tokio::test]
async fn stream_events_from_live_server() {
    let Some((base_url, directory)) = find_server().await else {
        eprintln!("no OpenCode server found, skipping");
        return;
    };

    let client = Client::builder().build().unwrap();

    // 1. Subscribe to SSE events
    let event_response = client
        .get(format!("{base_url}/event"))
        .query(&[("directory", directory.as_str())])
        .header("Accept", "text/event-stream")
        .timeout(std::time::Duration::from_secs(86400))
        .send()
        .await
        .expect("failed to subscribe to events");

    assert!(
        event_response.status().is_success(),
        "event subscription failed"
    );

    // 2. Create a session
    let session: Session = client
        .post(format!("{base_url}/session"))
        .query(&[("directory", directory.as_str())])
        .json(&serde_json::json!({"title": "stream-integration-test"}))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .expect("failed to create session")
        .json()
        .await
        .expect("failed to parse session");

    let session_id = session.id.clone();
    eprintln!("session created: {session_id}");

    // 3. Send a simple prompt async
    let prompt_status = client
        .post(format!("{base_url}/session/{session_id}/prompt_async"))
        .query(&[("directory", directory.as_str())])
        .json(&serde_json::json!({
            "parts": [{"type": "text", "text": "say the word 'pineapple' and nothing else"}]
        }))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .expect("failed to send prompt");

    assert!(
        prompt_status.status().is_success(),
        "prompt failed: {}",
        prompt_status.status()
    );

    // 4. Read events from bytes_stream, parse them, wait for session.idle
    let mut stream = event_response.bytes_stream();
    let mut buffer = String::new();
    let mut events: Vec<SseEvent> = Vec::new();
    let mut saw_idle = false;
    let mut saw_text = false;
    let mut saw_assistant = false;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);

    loop {
        let chunk = tokio::select! {
            chunk = stream.next() => chunk,
            _ = tokio::time::sleep_until(deadline) => {
                panic!(
                    "timed out after 60s waiting for session.idle. received {} events. saw_text={saw_text} saw_assistant={saw_assistant}",
                    events.len()
                );
            }
        };

        let Some(chunk) = chunk else {
            panic!(
                "stream ended before session.idle. received {} events",
                events.len()
            );
        };

        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => {
                panic!("bytes_stream error after {} events: {e}", events.len());
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&bytes));

        // Parse SSE events from buffer
        while let Some(pos) = buffer.find("\n\n") {
            let block = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            let mut data_parts = Vec::new();
            for line in block.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    data_parts.push(data);
                } else if let Some(data) = line.strip_prefix("data:") {
                    data_parts.push(data);
                }
            }

            if data_parts.is_empty() {
                continue;
            }

            let json_str = data_parts.join("\n");
            if json_str.is_empty() {
                continue;
            }

            let envelope: SseEventEnvelope = match serde_json::from_str(&json_str) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!(
                        "parse error: {err} on: {}",
                        &json_str[..json_str.len().min(200)]
                    );
                    continue;
                }
            };

            let event = SseEvent::from_envelope(envelope);
            eprintln!("  event: {}", format_event(&event));

            match &event {
                SseEvent::MessageUpdated { info } => {
                    if let Some(msg) = info {
                        if msg.role == "assistant" {
                            if msg.session_id.as_deref() == Some(session_id.as_str()) {
                                saw_assistant = true;
                            }
                        }
                    }
                }
                SseEvent::MessagePartUpdated { part, .. } => {
                    if let Part::Text {
                        session_id: Some(sid),
                        ..
                    } = part
                    {
                        if sid == &session_id {
                            saw_text = true;
                        }
                    }
                }
                SseEvent::SessionIdle { session_id: sid } => {
                    if sid == &session_id && saw_assistant {
                        saw_idle = true;
                        events.push(event);
                        break;
                    }
                }
                _ => {}
            }

            events.push(event);
        }

        if saw_idle {
            break;
        }
    }

    eprintln!("total events: {}", events.len());
    assert!(saw_text, "never received a text part for our session");
    assert!(saw_idle, "never received session.idle for our session");
}

fn format_event(event: &SseEvent) -> String {
    match event {
        SseEvent::MessageUpdated { info } => {
            let role = info.as_ref().map(|i| i.role.as_str()).unwrap_or("?");
            format!("message.updated (role={role})")
        }
        SseEvent::MessagePartUpdated { part, .. } => {
            let part_type = match part {
                Part::Text { .. } => "text",
                Part::Tool { tool, state, .. } => {
                    let tool_name = tool.as_deref().unwrap_or("?");
                    let status = state.as_ref().map(|s| s.status_str()).unwrap_or("?");
                    return format!("message.part.updated (tool={tool_name} status={status})");
                }
                Part::StepStart { .. } => "step-start",
                Part::StepFinish { .. } => "step-finish",
                Part::Other => "other",
            };
            format!("message.part.updated ({part_type})")
        }
        SseEvent::SessionIdle { .. } => "session.idle".into(),
        SseEvent::SessionError { .. } => "session.error".into(),
        SseEvent::SessionStatus { status, .. } => {
            let s = match status {
                SessionStatusPayload::Idle => "idle",
                SessionStatusPayload::Busy => "busy",
                SessionStatusPayload::Retry { .. } => "retry",
            };
            format!("session.status ({s})")
        }
        SseEvent::PermissionAsked(_) => "permission.asked".into(),
        SseEvent::QuestionAsked(_) => "question.asked".into(),
        SseEvent::Unknown(t) => format!("unknown ({t})"),
        _ => "other".into(),
    }
}
