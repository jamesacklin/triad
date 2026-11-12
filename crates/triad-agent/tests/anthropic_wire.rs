//! The rig-core brain against a local stand-in for Anthropic's Messages API:
//! no key, no network. Checks the wiring a real model would exercise: the
//! system prompt, tool definitions under their wire names, a tool call
//! through the gate, the tool result sent back, the final text and the
//! token counts.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use triad_agent::brain::{Brain, ModelBrain};
use triad_agent::context::{Kind, TurnInput};
use triad_agent::policy::{Limits, ToolSet};
use triad_agent::tools::{FakeTools, NoAudit, Tools};
use triad_agent::{Agent, run_turn};

/// Claude through Anthropic's Messages API, at the stand-in.
fn anthropic(url: &str) -> ModelBrain {
    ModelBrain::connect(
        "anthropic/claude-opus-5-5",
        Some(url),
        Some("test-key"),
        reqwest::Client::new(),
    )
    .unwrap()
}

/// Serves `replies` in order, one per request, recording request bodies.
fn stand_in(replies: Vec<Value>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        let mut replies = replies.into_iter();
        for stream in listener.incoming() {
            let stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            loop {
                let mut len = 0usize;
                let mut line = String::new();
                let mut got_any = false;
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    got_any = true;
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                if !got_any {
                    break;
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                seen2
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap());
                let reply = replies
                    .next()
                    .expect("more requests than replies")
                    .to_string();
                write!(
                    writer,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
                writer.flush().unwrap();
            }
        }
    });
    (url, seen)
}

#[tokio::test]
async fn rig_brain_runs_a_tool_turn() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("fake.json");
    std::fs::write(
        &fake,
        json!({"log": dir.path().join("calls.log"), "result": "event 42",
               "servers": {"calendar": ["create_event"]}})
        .to_string(),
    )
    .unwrap();
    let tools = FakeTools::load(&fake).unwrap();
    let (url, seen) = stand_in(vec![
        json!({"id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
               "content": [{"type": "tool_use", "id": "toolu_1", "name": "calendar__create_event",
                            "input": {"title": "hedge trimming"}}],
               "stop_reason": "tool_use", "stop_sequence": null,
               "usage": {"input_tokens": 100, "output_tokens": 20}}),
        json!({"id": "msg_2", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
               "content": [{"type": "text", "text": "Added \"hedge trimming\"."}],
               "stop_reason": "end_turn", "stop_sequence": null,
               "usage": {"input_tokens": 150, "output_tokens": 10}}),
    ]);
    triad_agent::http::install_crypto_provider();
    let brain = anthropic(&url);
    assert_eq!(brain.provider(), "anthropic/claude-opus-5-5");
    let all_tools = tools.list();
    let agent = Agent {
        brain: Arc::new(brain),
        tools: Arc::new(tools),
        persona: None,
        limits: Limits::default(),
        utc_offset_minutes: 0,
        persona_path: None,
        all_tools: all_tools.clone(),
        warnings: Vec::new(),
    };
    let input = TurnInput {
        kind: Kind::Channel,
        channel: "garden".into(),
        system: "SYSTEM-PROMPT-MARKER".into(),
        transcript: Vec::new(),
        asker: "3f9a12c4".into(),
        asker_tier: "admin".into(),
        ask: "@bot add hedge trimming".into(),
        tools: all_tools,
        unavailable: Vec::new(),
        host: "7c1d09ee".into(),
    };
    let result = run_turn(&agent, ToolSet::All, &NoAudit, &input)
        .await
        .unwrap();
    assert_eq!(result.text, "Added \"hedge trimming\".");
    assert!(result.denied.is_empty());
    assert_eq!(result.output.requests, 2);
    assert_eq!(result.output.input_tokens, Some(250));
    assert_eq!(result.output.output_tokens, Some(30));
    let calls = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
    assert!(calls.contains("calendar.create_event") && calls.contains("hedge trimming"));

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    let first = seen[0].to_string();
    assert!(first.contains("SYSTEM-PROMPT-MARKER"), "{first}");
    assert!(first.contains("calendar__create_event"), "{first}");
    assert!(first.contains("@bot add hedge trimming"), "{first}");
    let second = seen[1].to_string();
    assert!(
        second.contains("tool_result") && second.contains("event 42"),
        "{second}"
    );

    // The same turn for an asker with no tools: none are sent to the model.
    let (url, seen) = stand_in(vec![
        json!({"id": "msg_3", "type": "message", "role": "assistant",
        "model": "claude-opus-5-5", "content": [{"type": "text", "text": "Ask 7c1d09ee."}],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {"input_tokens": 50, "output_tokens": 5}}),
    ]);
    let brain = anthropic(&url);
    let agent = Agent {
        brain: Arc::new(brain),
        ..agent
    };
    let input = TurnInput {
        tools: Vec::new(),
        unavailable: vec!["calendar".into()],
        ..input
    };
    let result = run_turn(&agent, ToolSet::none(), &NoAudit, &input)
        .await
        .unwrap();
    assert_eq!(result.text, "Ask 7c1d09ee.");
    let body = seen.lock().unwrap()[0].clone();
    assert!(
        body.get("tools")
            .is_none_or(|t| t.as_array().is_some_and(|a| a.is_empty())),
        "{body}"
    );
}

/// The same turn through the OpenRouter backend, against a stand-in for its
/// OpenAI-format `POST /chat/completions`.
#[tokio::test]
async fn openrouter_brain_runs_a_tool_turn() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("fake.json");
    std::fs::write(
        &fake,
        json!({"log": dir.path().join("calls.log"), "result": "event 42",
               "servers": {"calendar": ["create_event"]}})
        .to_string(),
    )
    .unwrap();
    let tools = FakeTools::load(&fake).unwrap();
    let (url, seen) = stand_in(vec![
        json!({"id": "gen-1", "object": "chat.completion", "created": 1, "model": "anthropic/claude-opus-5.5",
               "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {"role": "assistant",
                 "content": null, "tool_calls": [{"id": "call_1", "type": "function",
                 "function": {"name": "calendar__create_event", "arguments": "{\"title\": \"hedge trimming\"}"}}]}}],
               "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120}}),
        json!({"id": "gen-2", "object": "chat.completion", "created": 2, "model": "anthropic/claude-opus-5.5",
               "choices": [{"index": 0, "finish_reason": "stop",
                 "message": {"role": "assistant", "content": "Added it."}}],
               "usage": {"prompt_tokens": 150, "completion_tokens": 10, "total_tokens": 160}}),
    ]);
    triad_agent::http::install_crypto_provider();
    let brain = ModelBrain::connect(
        "openrouter/anthropic/claude-opus-5.5",
        Some(&url),
        Some("test-key"),
        reqwest::Client::new(),
    )
    .unwrap();
    assert_eq!(brain.provider(), "openrouter/anthropic/claude-opus-5.5");
    let all_tools = tools.list();
    let agent = Agent {
        brain: Arc::new(brain),
        tools: Arc::new(tools),
        persona: None,
        limits: Limits::default(),
        utc_offset_minutes: 0,
        persona_path: None,
        all_tools: all_tools.clone(),
        warnings: Vec::new(),
    };
    let input = TurnInput {
        kind: Kind::Dm,
        channel: "agent".into(),
        system: "SYSTEM-PROMPT-MARKER".into(),
        transcript: Vec::new(),
        asker: "7c1d09ee".into(),
        asker_tier: "host".into(),
        ask: "add hedge trimming".into(),
        tools: all_tools,
        unavailable: Vec::new(),
        host: "7c1d09ee".into(),
    };
    let result = run_turn(&agent, ToolSet::All, &NoAudit, &input)
        .await
        .unwrap();
    assert_eq!(result.text, "Added it.");
    assert_eq!(result.output.requests, 2);
    assert_eq!(result.output.input_tokens, Some(250));
    assert_eq!(result.output.output_tokens, Some(30));
    let calls = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
    assert!(calls.contains("calendar.create_event") && calls.contains("hedge trimming"));
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0]["model"], "anthropic/claude-opus-5.5");
    let first = seen[0].to_string();
    assert!(
        first.contains("SYSTEM-PROMPT-MARKER") && first.contains("calendar__create_event"),
        "{first}"
    );
    let second = seen[1].to_string();
    assert!(
        second.contains("event 42") && second.contains("call_1"),
        "{second}"
    );
}

/// Model references are checked before connecting: the shape, the vendor,
/// and the vendor's key variable.
#[test]
fn model_references_are_checked() {
    use triad_agent::brain::{check_model, vendors};
    let v = vendors();
    for vendor in ["anthropic", "openai", "openrouter", "gcp.gemini", "ollama"] {
        assert!(v.contains(&vendor), "{vendor} missing from {v:?}");
    }
    assert!(
        check_model("claude-opus-5-5")
            .unwrap_err()
            .contains("vendor/model")
    );
    assert!(
        check_model("nosuchvendor/x")
            .unwrap_err()
            .contains("known:")
    );
    // A local server needs no key.
    check_model("ollama/qwen3:4b").unwrap();
    // A hosted vendor whose key is not set names the variable.
    if std::env::var_os("VENICE_API_KEY").is_none() {
        assert!(
            check_model("venice/some-model")
                .unwrap_err()
                .contains("VENICE_API_KEY")
        );
    }
}
