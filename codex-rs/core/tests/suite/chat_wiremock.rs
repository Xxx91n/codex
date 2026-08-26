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
use wiremock::Request;
use wiremock::Respond;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;
use wiremock::MockServer;

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
    body.push_str("data: [DONE]

");
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

/// True when the request is a chat payload whose messages include the tool
/// result for the given call id (i.e. the tool output got fed back upstream).
fn body_reports_tool_output(request: &Request, call_id: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(&request.body).is_ok_and(|body| {
        body.get("messages")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message.get("role").and_then(serde_json::Value::as_str) == Some("tool")
                        && message.get("tool_call_id").and_then(serde_json::Value::as_str)
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
    wait_for_event(&test.codex, |event| matches!(event, EventMsg::ShutdownComplete)).await;

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
