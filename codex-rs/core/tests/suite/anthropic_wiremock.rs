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

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

type RecordedRequests = Arc<Mutex<Vec<Vec<u8>>>>;

/// Messages API clients POST to the messages path, which the shared responses
/// harness does not match, so record requests locally.
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

/// Mounts a one-shot Messages API SSE mock and returns a handle to the raw
/// request bodies the mock received.
async fn mount_messages_sse_once_match<M>(
    server: &MockServer,
    matcher: M,
    body: String,
) -> RecordedRequests
where
    M: wiremock::Match + Send + Sync + 'static,
{
    let requests: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path_regex(".*/messages$"))
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

/// Anthropic Messages SSE body for a tool_use response: message_start, a
/// tool_use content block whose input arrives fragmented across
/// input_json_delta events, then message_delta (stop_reason tool_use) and
/// message_stop.
fn messages_sse_tool_use(call_id: &str, tool_name: &str, arguments_json: &str) -> String {
    let mid = arguments_json.len() / 2;
    let (a, b) = arguments_json.split_at(mid);
    let frame = |payload: serde_json::Value| {
        format!(
            "data: {}

",
            serde_json::to_string(&payload).expect("json serializes")
        )
    };
    let mut body = String::new();
    body.push_str(&frame(serde_json::json!({
        "type": "message_start",
        "message": {
            "id": "msg_roundtrip_1",
            "model": "claude-mock-1",
            "usage": {"input_tokens": 12, "output_tokens": 0},
        }
    })));
    body.push_str(&frame(serde_json::json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {"type": "tool_use", "id": call_id, "name": tool_name},
    })));
    body.push_str(&frame(serde_json::json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "input_json_delta", "partial_json": a},
    })));
    body.push_str(&frame(serde_json::json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "input_json_delta", "partial_json": b},
    })));
    body.push_str(&frame(serde_json::json!({
        "type": "content_block_stop",
        "index": 0,
    })));
    body.push_str(&frame(serde_json::json!({
        "type": "message_delta",
        "delta": {"stop_reason": "tool_use"},
        "usage": {"output_tokens": 5},
    })));
    body.push_str(&frame(serde_json::json!({"type": "message_stop"})));
    body
}

/// Anthropic Messages SSE body for a final text answer.
fn messages_sse_final_text(text: &str) -> String {
    format!(
        "data: {}

data: {}

data: {}

data: {}

data: {}

data: {}

",
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_roundtrip_2",
                "model": "claude-mock-1",
                "usage": {"input_tokens": 20, "output_tokens": 0},
            }
        }),
        serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""},
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": text},
        }),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 4},
        }),
        serde_json::json!({"type": "message_stop"}),
    )
}

/// True when the request messages include a `tool_result` block for the given
/// tool_use id (i.e. the tool output got fed back upstream).
fn body_reports_tool_output(request: &Request, call_id: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(&request.body).is_ok_and(|body| {
        body.get("messages")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message.get("role").and_then(serde_json::Value::as_str) == Some("user")
                        && message
                            .get("content")
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(|blocks| {
                                blocks.iter().any(|block| {
                                    block.get("type").and_then(serde_json::Value::as_str)
                                        == Some("tool_result")
                                        && block
                                            .get("tool_use_id")
                                            .and_then(serde_json::Value::as_str)
                                            == Some(call_id)
                                })
                            })
                })
            })
    })
}

/// End-to-end roundtrip over the Anthropic Messages wire: the mock upstream
/// asks for a local shell tool call via tool_use content blocks, codex
/// executes it, feeds the output back as a tool_result block, and the second
/// upstream answer yields the final assistant text.
#[tokio::test]
async fn anthropic_wire_tool_call_roundtrip() -> Result<()> {
    let server = start_mock_server().await;
    let call_id = "toolu_roundtrip_1";
    let args = r#"{"cmd":"echo anthropic-wire-roundtrip"}"#;

    let first_requests = mount_messages_sse_once_match(
        &server,
        |request: &Request| !body_reports_tool_output(request, call_id),
        messages_sse_tool_use(call_id, "shell", args),
    )
    .await;
    let second_requests = mount_messages_sse_once_match(
        &server,
        move |request: &Request| body_reports_tool_output(request, call_id),
        messages_sse_final_text("roundtrip complete"),
    )
    .await;

    let test = test_codex()
        .with_config(|config| {
            config.model_provider.wire_api = WireApi::Anthropic;
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
        "expected exactly one tool_use request"
    );
    assert_eq!(
        second_requests.lock().unwrap().len(),
        1,
        "expected exactly one follow-up request with tool output"
    );

    Ok(())
}

/// Session-level reasoning effort must reach the Anthropic Messages wire
/// (ticket 11 / ADR-0005): manual-track budgets the session effort into thinking.budget_tokens.
#[tokio::test]
async fn anthropic_wire_reasoning_effort_budget() -> Result<()> {
    let server = start_mock_server().await;

    let requests = mount_messages_sse_once_match(
        &server,
        |_request: &Request| true,
        messages_sse_final_text("reasoning effort complete"),
    )
    .await;

    let test = test_codex()
        .with_config(|config| {
            config.model_provider.wire_api = WireApi::Anthropic;
            // legacy budget present to prove session effort takes precedence
            config.model_reasoning_effort =
                Some(codex_protocol::openai_models::ReasoningEffort::Medium);
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
        body["thinking"]["type"], "enabled",
        "manual track must speak enabled+budget_tokens"
    );
    assert_eq!(
        body["thinking"]["budget_tokens"], 2_048,
        "medium session effort must bucket to the 2048 budget"
    );
    assert!(body.get("output_config").is_none());

    Ok(())
}

/// Session-level reasoning effort must reach the Anthropic Messages wire
/// (ticket 11 / ADR-0005): adaptive-track translates the session effort into output_config.effort.
#[tokio::test]
async fn anthropic_wire_reasoning_effort_adaptive() -> Result<()> {
    let server = start_mock_server().await;

    let requests = mount_messages_sse_once_match(
        &server,
        |_request: &Request| true,
        messages_sse_final_text("reasoning effort complete"),
    )
    .await;

    let test = test_codex()
        .with_config(|config| {
            config.model_provider.wire_api = WireApi::Anthropic;
            config.model_provider.anthropic_adaptive_thinking = true;
            config.model_reasoning_effort =
                Some(codex_protocol::openai_models::ReasoningEffort::Medium);
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
        body["thinking"]["type"], "adaptive",
        "adaptive track must speak thinking.type=adaptive"
    );
    assert_eq!(
        body["output_config"]["effort"], "medium",
        "adaptive track must carry the session effort verbatim"
    );
    assert!(body["thinking"].get("budget_tokens").is_none());

    Ok(())
}

/// Anthropic Messages SSE body: an optional leading thinking-family block
/// (signed or unsigned thinking, or a redacted_thinking block) followed by a
/// tool_use block with fragmented input JSON, ending in stop_reason tool_use.
/// ADR-0009 G6 fixture generator: exercises all three outbound replay paths.
fn messages_sse_thinking_then_tool_use(
    thinking: Option<(&str, Option<&str>)>,
    redacted: Option<(&str, &str)>,
    call_id: &str,
    tool_name: &str,
    arguments_json: &str,
) -> String {
    let frame = |payload: serde_json::Value| {
        format!(
            "data: {}

",
            serde_json::to_string(&payload).expect("json serializes")
        )
    };
    let mut body = String::new();
    body.push_str(&frame(serde_json::json!({
        "type": "message_start",
        "message": {
            "id": format!("msg_think_{call_id}"),
            "model": "claude-mock-1",
            "usage": {"input_tokens": 12, "output_tokens": 0},
        }
    })));
    let mut index = 0usize;
    if let Some((data, signature)) = redacted {
        // redacted_thinking: payload at content_block_start.data, signature
        // via a later signature_delta (ticket 27 def-1 contract).
        body.push_str(&frame(serde_json::json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {"type": "redacted_thinking", "data": data},
        })));
        body.push_str(&frame(serde_json::json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "signature_delta", "signature": signature},
        })));
        body.push_str(&frame(
            serde_json::json!({"type": "content_block_stop", "index": index}),
        ));
        index += 1;
    }
    if let Some((text, signature)) = thinking {
        body.push_str(&frame(serde_json::json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {"type": "thinking", "thinking": ""},
        })));
        body.push_str(&frame(serde_json::json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "thinking_delta", "thinking": text},
        })));
        if let Some(signature) = signature {
            body.push_str(&frame(serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "signature_delta", "signature": signature},
            })));
        }
        body.push_str(&frame(
            serde_json::json!({"type": "content_block_stop", "index": index}),
        ));
        index += 1;
    }
    let mid = arguments_json.len() / 2;
    let (a, b) = arguments_json.split_at(mid);
    body.push_str(&frame(serde_json::json!({
        "type": "content_block_start",
        "index": index,
        "content_block": {"type": "tool_use", "id": call_id, "name": tool_name},
    })));
    body.push_str(&frame(serde_json::json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "input_json_delta", "partial_json": a},
    })));
    body.push_str(&frame(serde_json::json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "input_json_delta", "partial_json": b},
    })));
    body.push_str(&frame(
        serde_json::json!({"type": "content_block_stop", "index": index}),
    ));
    body.push_str(&frame(serde_json::json!({
        "type": "message_delta",
        "delta": {"stop_reason": "tool_use"},
        "usage": {"output_tokens": 5},
    })));
    body.push_str(&frame(serde_json::json!({"type": "message_stop"})));
    body
}

/// The assistant message blocks of a recorded Messages request body (first
/// assistant turn), empty when the request carries no assistant message.
fn recorded_assistant_blocks(request_body: &[u8]) -> Vec<serde_json::Value> {
    let body: serde_json::Value =
        serde_json::from_slice(request_body).expect("request body is json");
    body["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .find(|message| message["role"] == "assistant")
        .and_then(|message| message["content"].as_array().map(|blocks| blocks.to_vec()))
        .unwrap_or_default()
}

/// Drives one tool round against the mock: the upstream first answers with a
/// thinking-family block + shell tool_use, then with final text once the tool
/// result is fed back. Returns the raw bodies of the two requests.
async fn drive_thinking_tool_round(
    server: &MockServer,
    first_body: String,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let call_id = "toolu_thinking_replay";
    let args = r#"{"cmd":"echo thinking-replay"}"#;
    let first_requests = mount_messages_sse_once_match(
        server,
        move |request: &Request| !body_reports_tool_output(request, call_id),
        first_body,
    )
    .await;
    let second_requests = mount_messages_sse_once_match(
        server,
        move |request: &Request| body_reports_tool_output(request, call_id),
        messages_sse_final_text("thinking replay complete"),
    )
    .await;

    let test = test_codex()
        .with_config(|config| {
            config.model_provider.wire_api = WireApi::Anthropic;
        })
        .build(server)
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

    let first = first_requests.lock().unwrap().first().cloned();
    let second = second_requests.lock().unwrap().first().cloned();
    Ok((
        first.expect("tool_use request recorded"),
        second.expect("follow-up request recorded"),
    ))
}

/// G6 path 1 (ADR-0009): a signed thinking block from a tool round must be
/// replayed verbatim — text and signature byte-equal — as the leading block
/// of the assistant message on the follow-up request.
#[tokio::test]
async fn anthropic_wire_thinking_replay_signed_block_roundtrip() -> Result<()> {
    let server = start_mock_server().await;
    let (_first, second) = drive_thinking_tool_round(
        &server,
        messages_sse_thinking_then_tool_use(
            Some(("let me plan", Some("sig-live-1"))),
            None,
            "toolu_thinking_replay",
            "shell",
            r#"{"cmd":"echo thinking-replay"}"#,
        ),
    )
    .await;

    let blocks = recorded_assistant_blocks(&second);
    assert_eq!(blocks.len(), 2, "thinking + tool_use: {blocks:?}");
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["thinking"], "let me plan");
    assert_eq!(
        blocks[0]["signature"], "sig-live-1",
        "signed replay must be verbatim, never tampered (ADR-0003 keeps this half of the red line)"
    );
    assert_eq!(blocks[1]["type"], "tool_use");
    Ok(())
}

/// G6 path 2 (ADR-0009): an unsigned thinking block from a compatible
/// endpoint (the mock server is not an anthropic.com host) must go back
/// verbatim without a fabricated signature field.
#[tokio::test]
async fn anthropic_wire_thinking_replay_unsigned_preserved_on_compatible_upstream() -> Result<()> {
    let server = start_mock_server().await;
    let (_first, second) = drive_thinking_tool_round(
        &server,
        messages_sse_thinking_then_tool_use(
            Some(("let me plan unsigned", None)),
            None,
            "toolu_thinking_replay",
            "shell",
            r#"{"cmd":"echo thinking-replay"}"#,
        ),
    )
    .await;

    let blocks = recorded_assistant_blocks(&second);
    assert_eq!(
        blocks.len(),
        2,
        "compatible endpoints never sign blocks: dropping the unsigned block would 400 the next tool round (D-009 finding 4), got {blocks:?}"
    );
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["thinking"], "let me plan unsigned");
    assert!(
        blocks[0].get("signature").is_none(),
        "preserved replay must not fabricate a signature field"
    );
    assert_eq!(blocks[1]["type"], "tool_use");
    Ok(())
}

/// G6 path 3 (ADR-0009 / ticket 27 def-1): a redacted_thinking block must
/// round-trip with its opaque payload and signature on the follow-up request.
#[tokio::test]
async fn anthropic_wire_thinking_replay_redacted_block_roundtrip() -> Result<()> {
    let server = start_mock_server().await;
    let (_first, second) = drive_thinking_tool_round(
        &server,
        messages_sse_thinking_then_tool_use(
            None,
            Some(("opaque-redacted-payload", "sig-redacted-1")),
            "toolu_thinking_replay",
            "shell",
            r#"{"cmd":"echo thinking-replay"}"#,
        ),
    )
    .await;

    let blocks = recorded_assistant_blocks(&second);
    assert_eq!(blocks.len(), 2, "redacted + tool_use: {blocks:?}");
    assert_eq!(blocks[0]["type"], "redacted_thinking");
    assert_eq!(blocks[0]["data"], "opaque-redacted-payload");
    assert_eq!(blocks[0]["signature"], "sig-redacted-1");
    assert_eq!(blocks[1]["type"], "tool_use");
    Ok(())
}

/// Responds 400 (thinking replay rejected) exactly once, then streams final
/// text, recording every request body.
struct RejectOnceThenSse {
    calls: AtomicUsize,
    requests: RecordedRequests,
    sse_body: String,
}

impl Respond for RejectOnceThenSse {
    fn respond(&self, request: &Request) -> wiremock::ResponseTemplate {
        self.requests.lock().unwrap().push(request.body.clone());
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return wiremock::ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "message": "Invalid signature in thinking block (id: thinking)",
                }
            }));
        }
        sse_response(self.sse_body.clone())
    }
}

/// Guard G3 (ADR-0009): a 400 whose text names a rejected thinking replay
/// must trigger exactly one degraded retry — the retry carries no thinking
/// parameter — and the turn must still complete instead of dying.
#[tokio::test]
async fn anthropic_wire_thinking_replay_400_invalid_signature_degrades_and_retries() -> Result<()> {
    let server = start_mock_server().await;
    let requests: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path_regex(".*/messages$"))
        .respond_with(RejectOnceThenSse {
            calls: AtomicUsize::new(0),
            requests: Arc::clone(&requests),
            sse_body: messages_sse_final_text("recovered after 400"),
        })
        .mount(&server)
        .await;

    let test = test_codex()
        .with_config(|config| {
            config.model_provider.wire_api = WireApi::Anthropic;
            config.model_reasoning_effort =
                Some(codex_protocol::openai_models::ReasoningEffort::Medium);
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
    assert_eq!(recorded.len(), 2, "one 400 then one degraded retry");
    let first: serde_json::Value = serde_json::from_slice(&recorded[0]).expect("json");
    let second: serde_json::Value = serde_json::from_slice(&recorded[1]).expect("json");
    assert_eq!(
        first["thinking"]["type"], "enabled",
        "the doomed first attempt carried the manual thinking parameter"
    );
    assert!(
        second.get("thinking").is_none(),
        "the G3 retry must strip the thinking parameter (and blocks)"
    );
    Ok(())
}
