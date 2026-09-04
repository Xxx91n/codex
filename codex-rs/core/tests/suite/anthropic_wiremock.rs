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