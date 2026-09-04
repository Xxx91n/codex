use anyhow::Result;
use codex_model_provider_info::WireApi;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::user_input::UserInput;
use std::sync::Arc;
use std::sync::Mutex;

use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::Respond;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

type RecordedRequests = Arc<Mutex<Vec<Vec<u8>>>>;

/// Chat completions clients POST to the chat/completions path, which the
/// shared responses harness does not match, so record requests locally.
struct RecordingRespond {
    requests: RecordedRequests,
    body: String,
}

impl Respond for RecordingRespond {
    fn respond(&self, request: &Request) -> wiremock::ResponseTemplate {
        self.requests.lock().unwrap().push(request.body.clone());
        sse_response(self.body.clone())
    }
}

/// Mounts a one-shot chat-completions SSE mock and returns a handle to the
/// raw request bodies the mock received.
async fn mount_chat_sse_once_match<M>(
    server: &MockServer,
    matcher: M,
    body: String,
) -> RecordedRequests
where
    M: wiremock::Match + Send + Sync + 'static,
{
    let requests: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path_regex(".*/chat/completions$"))
        .and(matcher)
        .respond_with(RecordingRespond {
            requests: Arc::clone(&requests),
            body,
        })
        .up_to_n_times(1)
        .mount(server)
        .await;
    requests
}

/// Chat Completions SSE body: chunked tool-call deltas (merged by the client),
/// a usage frame, then [DONE].
fn chat_sse_tool_call(call_id: &str, tool_name: &str, arguments_json: &str) -> String {
    let mid = arguments_json.len() / 2;
    let (a, b) = arguments_json.split_at(mid);
    let chunk = |delta: serde_json::Value, finish: Option<&str>| {
        format!(
            "data: {}

",
            serde_json::json!({
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": finish,
                }]
            })
        )
    };
    let mut body = String::new();
    body.push_str(&chunk(
        serde_json::json!({
            "tool_calls": [{
                "index": 0,
                "id": call_id,
                "type": "function",
                "function": {"name": tool_name, "arguments": a},
            }]
        }),
        None,
    ));
    body.push_str(&chunk(
        serde_json::json!({
            "tool_calls": [{
                "index": 0,
                "function": {"arguments": b},
            }]
        }),
        None,
    ));
    body.push_str(&chunk(serde_json::json!({}), Some("tool_calls")));
    body.push_str(&format!(
        "data: {}

",
        serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 5,
                "total_tokens": 17,
            }
        })
    ));
    body.push_str(
        "data: [DONE]

",
    );
    body
}

fn chat_sse_final_text(text: &str) -> String {
    format!(
        "data: {}

data: {}

data: [DONE]

",
        serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {"content": text},
                "finish_reason": null,
            }]
        }),
        serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 4,
                "total_tokens": 24,
            }
        })
    )
}

/// Chat Completions SSE body with a `reasoning_content` segment followed by a
/// tool call, mirroring how DeepSeek-R1-class upstreams stream
/// chain-of-thought before a tool round (the exact shape where DeepSeek/Kimi
/// require the reasoning history to be replayed).
fn chat_sse_reasoning_then_tool_call(
    reasoning: &str,
    call_id: &str,
    tool_name: &str,
    arguments_json: &str,
) -> String {
    let mid = arguments_json.len() / 2;
    let (a, b) = arguments_json.split_at(mid);
    let chunk = |delta: serde_json::Value, finish: Option<&str>| {
        format!(
            "data: {}

",
            serde_json::json!({
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": finish,
                }]
            })
        )
    };
    let mut body = String::new();
    body.push_str(&chunk(
        serde_json::json!({"reasoning_content": reasoning}),
        None,
    ));
    body.push_str(&chunk(
        serde_json::json!({
            "tool_calls": [{
                "index": 0,
                "id": call_id,
                "type": "function",
                "function": {"name": tool_name, "arguments": a},
            }]
        }),
        None,
    ));
    body.push_str(&chunk(
        serde_json::json!({
            "tool_calls": [{
                "index": 0,
                "function": {"arguments": b},
            }]
        }),
        None,
    ));
    body.push_str(&chunk(serde_json::json!({}), Some("tool_calls")));
    body.push_str(&format!(
        "data: {}

",
        serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 8,
                "total_tokens": 20,
                "completion_tokens_details": {"reasoning_tokens": 6}
            }
        })
    ));
    body.push_str(
        "data: [DONE]

",
    );
    body
}

/// True when the request is a chat payload whose messages include the tool
/// result for the given call id (i.e. the tool output got fed back upstream).
fn body_reports_tool_output(request: &Request, call_id: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(&request.body).is_ok_and(|body| {
        body.get("messages")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message.get("role").and_then(serde_json::Value::as_str) == Some("tool")
                        && message
                            .get("tool_call_id")
                            .and_then(serde_json::Value::as_str)
                            == Some(call_id)
                })
            })
    })
}

/// End-to-end roundtrip over the chat wire: the mock upstream asks for a local
/// shell tool call via streaming tool_call deltas, codex executes it, feeds
/// the output back as a tool-role message, and the second upstream answer
/// yields the final assistant text.
#[tokio::test]
async fn chat_wire_tool_call_roundtrip() -> Result<()> {
    let server = start_mock_server().await;
    let call_id = "chat-roundtrip-call-1";
    let args = r#"{"cmd":"echo chat-wire-roundtrip"}"#;

    let first_requests = mount_chat_sse_once_match(
        &server,
        |request: &Request| !body_reports_tool_output(request, call_id),
        chat_sse_tool_call(call_id, "shell", args),
    )
    .await;
    let second_requests = mount_chat_sse_once_match(
        &server,
        move |request: &Request| body_reports_tool_output(request, call_id),
        chat_sse_final_text("roundtrip complete"),
    )
    .await;

    let test = test_codex()
        .with_config(|config| {
            config.model_provider.wire_api = WireApi::Chat;
        })
        .build(&server)
        .await?;

    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "run the echo command".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(
                codex_protocol::protocol::ThreadSettingsOverrides {
                    approval_policy: Some(AskForApproval::Never),
                    ..Default::default()
                },
            ),
        )
        .await?;

    wait_for_event(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => true,
        EventMsg::Error(error) => panic!("unexpected turn error: {error:?}"),
        _ => false,
    })
    .await;

    test.codex.submit(Op::Shutdown).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ShutdownComplete)
    })
    .await;

    assert_eq!(
        first_requests.lock().unwrap().len(),
        1,
        "expected exactly one tool-call request"
    );
    assert_eq!(
        second_requests.lock().unwrap().len(),
        1,
        "expected exactly one follow-up request with tool output"
    );

    Ok(())
}

/// True when the request is a chat payload whose messages include an
/// assistant `reasoning_content` replay (i.e. the thinking history got fed
/// back upstream, as DeepSeek/Kimi tool rounds require).
fn body_reports_reasoning_replay(request: &Request, reasoning: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(&request.body).is_ok_and(|body| {
        body.get("messages")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
                        && message
                            .get("reasoning_content")
                            .and_then(serde_json::Value::as_str)
                            == Some(reasoning)
                })
            })
    })
}

/// Roundtrip with chain-of-thought: the first upstream answer streams a
/// `reasoning_content` segment before requesting a tool call; the follow-up
/// request must replay that reasoning verbatim as assistant
/// `reasoning_content` (DeepSeek/Kimi contract: missing reasoning history in
/// tool rounds 400s or degrades the follow-up). The second mount only matches
/// when both the tool output and the verbatim reasoning replay are present,
/// so reaching TurnComplete proves the reasoning survived the full
/// round-trip: SSE delta -> ResponseItem::Reasoning -> history -> replay.
#[tokio::test]
async fn chat_wire_reasoning_content_roundtrip() -> Result<()> {
    let server = start_mock_server().await;
    let call_id = "chat-reasoning-call-1";
    let reasoning = "step-by-step thinking trace";
    let args = r#"{"cmd":"echo reasoning-roundtrip"}"#;

    let first_requests = mount_chat_sse_once_match(
        &server,
        |request: &Request| !body_reports_tool_output(request, call_id),
        chat_sse_reasoning_then_tool_call(reasoning, call_id, "shell", args),
    )
    .await;
    let second_requests = mount_chat_sse_once_match(
        &server,
        move |request: &Request| {
            body_reports_tool_output(request, call_id)
                && body_reports_reasoning_replay(request, reasoning)
        },
        chat_sse_final_text("reasoning roundtrip complete"),
    )
    .await;

    let test = test_codex()
        .with_config(|config| {
            config.model_provider.wire_api = WireApi::Chat;
        })
        .build(&server)
        .await?;

    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "run the echo command".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(
                codex_protocol::protocol::ThreadSettingsOverrides {
                    approval_policy: Some(AskForApproval::Never),
                    ..Default::default()
                },
            ),
        )
        .await?;

    wait_for_event(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => true,
        EventMsg::Error(error) => panic!("unexpected turn error: {error:?}"),
        _ => false,
    })
    .await;

    test.codex.submit(Op::Shutdown).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ShutdownComplete)
    })
    .await;

    assert_eq!(
        first_requests.lock().unwrap().len(),
        1,
        "expected exactly one reasoning turn request"
    );
    assert_eq!(
        second_requests.lock().unwrap().len(),
        1,
        "expected exactly one follow-up request with verbatim reasoning replay"
    );

    Ok(())
}

/// Plain first turn over the chat wire (empty history, text only): the request
/// opens with a system message built from base instructions, then the user
/// message; the streamed text answer completes the turn. Complements the
/// tool-call roundtrip, which covers the tool-history class.
#[tokio::test]
async fn chat_wire_plain_text_turn() -> Result<()> {
    let server = start_mock_server().await;

    let requests = mount_chat_sse_once_match(
        &server,
        |_request: &Request| true,
        chat_sse_final_text("plain text complete"),
    )
    .await;

    let test = test_codex()
        .with_config(|config| {
            config.model_provider.wire_api = WireApi::Chat;
        })
        .build(&server)
        .await?;

    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "say hello back".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(
                codex_protocol::protocol::ThreadSettingsOverrides {
                    approval_policy: Some(AskForApproval::Never),
                    ..Default::default()
                },
            ),
        )
        .await?;

    wait_for_event(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => true,
        EventMsg::Error(error) => panic!("unexpected turn error: {error:?}"),
        _ => false,
    })
    .await;

    test.codex.submit(Op::Shutdown).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ShutdownComplete)
    })
    .await;

    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 1, "expected exactly one request");
    let body: serde_json::Value =
        serde_json::from_slice(&recorded[0]).expect("request body is json");
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(
        messages.first().and_then(|m| m["role"].as_str()),
        Some("system"),
        "chat request must open with the system message"
    );
    assert!(
        messages.iter().any(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|c| c.contains("say hello back"))
        }),
        "chat request must contain the user message"
    );
    assert!(
        body.get("store").is_none()
            && body.get("previous_response_id").is_none()
            && body.get("reasoning").is_none(),
        "chat request must not carry responses-only fields"
    );

    Ok(())
}


/// Session-level reasoning effort must reach the chat wire as the top-level
/// `reasoning_effort` field (ticket 11 / ADR-0005): the OpenAI-compatible
/// effort knob rides on the outbound request body.
#[tokio::test]
async fn chat_wire_reasoning_effort_passthrough() -> Result<()> {
    let server = start_mock_server().await;

    let requests = mount_chat_sse_once_match(
        &server,
        |_request: &Request| true,
        chat_sse_final_text("reasoning effort complete"),
    )
    .await;

    let test = test_codex()
        .with_config(|config| {
            config.model_provider.wire_api = WireApi::Chat;
            config.model_reasoning_effort =
                Some(codex_protocol::openai_models::ReasoningEffort::High);
        })
        .build(&server)
        .await?;

    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "say hello back".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(
                codex_protocol::protocol::ThreadSettingsOverrides {
                    approval_policy: Some(AskForApproval::Never),
                    ..Default::default()
                },
            ),
        )
        .await?;

    wait_for_event(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => true,
        EventMsg::Error(error) => panic!("unexpected turn error: {error:?}"),
        _ => false,
    })
    .await;

    test.codex.submit(Op::Shutdown).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ShutdownComplete)
    })
    .await;

    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 1, "expected exactly one request");
    let body: serde_json::Value =
        serde_json::from_slice(&recorded[0]).expect("request body is json");
    assert_eq!(
        body["reasoning_effort"], "high",
        "chat request must carry the session reasoning effort"
    );

    Ok(())
}