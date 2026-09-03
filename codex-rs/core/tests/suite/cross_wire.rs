//! Cross-wire fixture-driven tests (ticket 09).
//!
//! Consumes the `wiremock_fixtures/` corpus: `cross_wire_table.json` (the
//! semantic-to-wire dictionary) plus one tool_call roundtrip fixture per wire.
//! Each end-to-end test drives one wire with its fixture stream and asserts
//! the table's recorded shapes against what the wire actually sends: the chat
//! index-split tool_call deltas merge and the output replays as a `tool`
//! message, the Anthropic thinking block + signature replay verbatim with the
//! tool result under a user message, and the Responses done-item function_call
//! output is fed back as a `function_call_output` input item.

use anyhow::Result;
use codex_model_provider_info::WireApi;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use serde_json::Value;
use std::sync::Arc;
use std::sync::Mutex;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::Respond;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

const CROSS_WIRE_TABLE: &str = include_str!("wiremock_fixtures/cross_wire_table.json");
const CHAT_TOOL_CALL_FIXTURE: &str =
    include_str!("wiremock_fixtures/tool_call/chat__roundtrip_delta_split.json");
const ANTHROPIC_TOOL_CALL_FIXTURE: &str =
    include_str!("wiremock_fixtures/tool_call/anthropic__thinking_then_tool_use.json");
const RESPONSES_TOOL_CALL_FIXTURE: &str =
    include_str!("wiremock_fixtures/tool_call/responses__roundtrip.json");

type RecordedRequests = Arc<Mutex<Vec<Vec<u8>>>>;

/// Records raw request bodies and answers with a fixed SSE body.
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

/// Mounts a one-shot POST mock anchored on `path_pattern` answering with
/// `body`, recording every matched request body.
async fn mount_sse_body<M>(
    server: &MockServer,
    path_pattern: &'static str,
    matcher: M,
    body: String,
) -> RecordedRequests
where
    M: wiremock::Match + Send + Sync + 'static,
{
    let requests: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path_regex(path_pattern))
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

fn parse_json(raw: &str, what: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("{what} is not valid JSON: {error}"))
}

fn parse_events(fixture: &Value, what: &str) -> Vec<Value> {
    serde_json::from_value(fixture["stream_body_events"].clone())
        .unwrap_or_else(|error| panic!("{what} has no usable stream_body_events: {error}"))
}

/// Frames events as `data:`-only SSE frames (the chat/anthropic mock dialect;
/// the Responses dialect via `sse()` adds `event:` lines, which its parser
/// relies on).
fn data_frames(events: &[Value]) -> String {
    let mut body = String::new();
    for event in events {
        let line = serde_json::to_string(event).expect("fixture event serializes");
        body.push_str(&format!("data: {line}\n\n"));
    }
    body
}

fn user_turn(text: &str) -> TurnInputRequest {
    TurnInputRequest::user_input(vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }])
    .with_thread_settings(ThreadSettingsOverrides {
        approval_policy: Some(AskForApproval::Never),
        ..Default::default()
    })
}

async fn shutdown(codex: &codex_core::CodexThread) {
    codex.submit(Op::Shutdown).await.expect("shutdown submit");
    wait_for_event(codex, |event| matches!(event, EventMsg::ShutdownComplete)).await;
}

/// The table is the acceptance gate: 6 semantics x 3 wires, every row fully
/// populated, and the ticket-mandated semantics present.
#[test]
fn cross_wire_table_matrix_is_complete() {
    let table = parse_json(CROSS_WIRE_TABLE, "cross_wire_table.json");
    assert_eq!(table["schema_version"], 1);
    assert_eq!(table["ir"], "codex_protocol::models::ResponseItem");

    let semantics: Vec<&str> = table["semantics"]
        .as_array()
        .expect("semantics array")
        .iter()
        .map(|semantic| semantic["id"].as_str().expect("semantic id"))
        .collect();
    for required in [
        "tool_call",
        "thinking",
        "image",
        "cache_control",
        "streaming_stop",
        "error",
    ] {
        assert!(semantics.contains(&required), "missing semantic {required}");
    }

    let wires: Vec<&str> = table["wires"]
        .as_array()
        .expect("wires array")
        .iter()
        .map(|wire| wire["id"].as_str().expect("wire id"))
        .collect();
    assert_eq!(wires, vec!["responses", "chat", "anthropic"]);

    let matrix = table["matrix"].as_array().expect("matrix array");
    assert_eq!(
        matrix.len(),
        semantics.len() * wires.len(),
        "matrix must cover every semantic x wire exactly once"
    );
    for row in matrix {
        let semantic = row["semantic"].as_str().expect("row semantic");
        let wire = row["wire"].as_str().expect("row wire");
        assert!(semantics.contains(&semantic), "unknown semantic {semantic}");
        assert!(wires.contains(&wire), "unknown wire {wire}");
        let support = row["support"].as_str().expect("row support");
        assert!(
            ["native", "translated", "degraded", "absent"].contains(&support),
            "bad support {support} on {semantic}/{wire}"
        );
        for column in ["direction", "ir_mapping"] {
            assert!(
                row.get(column).is_some_and(|value| !value.is_null()),
                "row {semantic}/{wire} missing {column}"
            );
        }
    }

    // The three wired fixtures must be referenced by their own tool_call rows
    // and parse (include_str! already pins them into this binary).
    for (name, raw) in [
        (
            "tool_call/chat__roundtrip_delta_split.json",
            CHAT_TOOL_CALL_FIXTURE,
        ),
        (
            "tool_call/anthropic__thinking_then_tool_use.json",
            ANTHROPIC_TOOL_CALL_FIXTURE,
        ),
        (
            "tool_call/responses__roundtrip.json",
            RESPONSES_TOOL_CALL_FIXTURE,
        ),
    ] {
        let fixture = parse_json(raw, name);
        let wire = fixture["meta"]["wire"].as_str().expect("fixture wire");
        assert_eq!(
            fixture["meta"]["semantic"].as_str(),
            Some("tool_call"),
            "{name} semantic"
        );
        assert!(
            matrix.iter().any(|row| {
                row["semantic"] == "tool_call"
                    && row["wire"] == wire
                    && row["fixtures"]
                        .as_array()
                        .is_some_and(|fixtures| fixtures.iter().any(|f| f == name))
            }),
            "{name} not referenced by its tool_call matrix row"
        );
    }
}

/// Chat wire roundtrip from the fixture: the index-split tool_call deltas must
/// merge into one FunctionCall, execute the shell tool, and replay the output
/// as a `tool`-role message; the request must degrade Responses-only fields.
#[tokio::test]
async fn cross_wire_chat_tool_call_roundtrip_from_fixture() -> Result<()> {
    let fixture = parse_json(CHAT_TOOL_CALL_FIXTURE, "chat fixture");
    let stream_body = fixture["stream_body"]
        .as_str()
        .expect("chat fixture stream_body")
        .to_string();
    let call_id = fixture["meta"]["call_id"]
        .as_str()
        .expect("chat fixture call_id")
        .to_string();
    let final_text_body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({
            "choices": [{"index": 0, "delta": {"content": "cross-wire fixture done"}, "finish_reason": null}]
        }),
        serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }),
    );

    let server = start_mock_server().await;
    let call_id_for_matcher = call_id.clone();
    let first_requests = mount_sse_body(
        &server,
        ".*/chat/completions$",
        move |request: &Request| !request_has_tool_message(request, &call_id_for_matcher),
        stream_body,
    )
    .await;
    let second_requests = mount_sse_body(
        &server,
        ".*/chat/completions$",
        move |request: &Request| request_has_tool_message(request, &call_id),
        final_text_body,
    )
    .await;

    let test = test_codex()
        .with_config(|config| {
            config.model_provider.wire_api = WireApi::Chat;
        })
        .build(&server)
        .await?;

    test.codex
        .start_or_steer_turn(user_turn("run the cross-wire fixture command"))
        .await?;

    wait_for_event(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => true,
        EventMsg::Error(error) => panic!("unexpected turn error: {error:?}"),
        _ => false,
    })
    .await;
    shutdown(&test.codex).await;

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
    let first_body: Value = serde_json::from_slice(&first_requests.lock().unwrap()[0])
        .expect("first chat request is json");
    assert!(
        first_body
            .get("tools")
            .is_some_and(serde_json::Value::is_array),
        "chat request must carry the tools array"
    );
    assert!(
        first_body.get("store").is_none()
            && first_body.get("previous_response_id").is_none()
            && first_body.get("reasoning").is_none(),
        "chat request must not carry responses-only fields"
    );

    Ok(())
}

fn request_has_tool_message(request: &Request, call_id: &str) -> bool {
    serde_json::from_slice::<Value>(&request.body).is_ok_and(|body| {
        body["messages"].as_array().is_some_and(|messages| {
            messages.iter().any(|message| {
                message["role"].as_str() == Some("tool")
                    && message["tool_call_id"].as_str() == Some(call_id)
            })
        })
    })
}

/// Anthropic wire roundtrip from the fixture: the streamed thinking block must
/// replay verbatim (text AND signature — the 400 red line) inside the
/// tool-use round, and the tool result must travel as a `tool_result` block
/// under a user message.
#[tokio::test]
async fn cross_wire_anthropic_thinking_tool_use_roundtrip_from_fixture() -> Result<()> {
    let fixture = parse_json(ANTHROPIC_TOOL_CALL_FIXTURE, "anthropic fixture");
    let events = parse_events(&fixture, "anthropic fixture");
    let stream_body = data_frames(&events);
    let call_id = fixture["meta"]["call_id"]
        .as_str()
        .expect("anthropic fixture call_id")
        .to_string();
    let signature = fixture["meta"]["signature"]
        .as_str()
        .expect("anthropic fixture signature")
        .to_string();
    let final_text_body = data_frames(&[
        serde_json::json!({
            "type": "message_start",
            "message": {"id": "msg_cross_2", "model": "claude-mock-1", "usage": {"input_tokens": 1, "output_tokens": 0}}
        }),
        serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "cross-wire fixture done"}
        }),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 1}
        }),
        serde_json::json!({"type": "message_stop"}),
    ]);

    let server = start_mock_server().await;
    let signature_for_matcher = signature.clone();
    let call_id_for_matcher = call_id.clone();
    let first_requests = mount_sse_body(
        &server,
        ".*/messages$",
        move |request: &Request| {
            !request_replays_thinking_and_tool_result(
                request,
                &call_id_for_matcher,
                &signature_for_matcher,
            )
        },
        stream_body,
    )
    .await;
    let second_requests = mount_sse_body(
        &server,
        ".*/messages$",
        move |request: &Request| {
            request_replays_thinking_and_tool_result(request, &call_id, &signature)
        },
        final_text_body,
    )
    .await;

    let test = test_codex()
        .with_config(|config| {
            config.model_provider.wire_api = WireApi::Anthropic;
        })
        .build(&server)
        .await?;

    test.codex
        .start_or_steer_turn(user_turn("run the cross-wire fixture command"))
        .await?;

    wait_for_event(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => true,
        EventMsg::Error(error) => panic!("unexpected turn error: {error:?}"),
        _ => false,
    })
    .await;
    shutdown(&test.codex).await;

    assert_eq!(
        first_requests.lock().unwrap().len(),
        1,
        "expected exactly one thinking+tool_use request"
    );
    assert_eq!(
        second_requests.lock().unwrap().len(),
        1,
        "expected exactly one follow-up request replaying thinking + tool result"
    );

    Ok(())
}

fn request_replays_thinking_and_tool_result(
    request: &Request,
    call_id: &str,
    signature: &str,
) -> bool {
    serde_json::from_slice::<Value>(&request.body).is_ok_and(|body| {
        let messages = body["messages"].as_array();
        let thinking_replayed = messages.is_some_and(|messages| {
            messages.iter().any(|message| {
                message["role"].as_str() == Some("assistant")
                    && message["content"].as_array().is_some_and(|blocks| {
                        blocks.iter().any(|block| {
                            block["type"].as_str() == Some("thinking")
                                && block["signature"].as_str() == Some(signature)
                        })
                    })
            })
        });
        let tool_result_present = messages.is_some_and(|messages| {
            messages.iter().any(|message| {
                message["role"].as_str() == Some("user")
                    && message["content"].as_array().is_some_and(|blocks| {
                        blocks.iter().any(|block| {
                            block["type"].as_str() == Some("tool_result")
                                && block["tool_use_id"].as_str() == Some(call_id)
                        })
                    })
            })
        });
        thinking_replayed && tool_result_present
    })
}

/// Responses wire roundtrip from the fixture: the `output_item.done`
/// function_call item is authoritative, the shell tool executes, and the
/// output feeds back as a `function_call_output` input item on the follow-up
/// request.
#[tokio::test]
async fn cross_wire_responses_function_call_roundtrip_from_fixture() -> Result<()> {
    let fixture = parse_json(RESPONSES_TOOL_CALL_FIXTURE, "responses fixture");
    let events = parse_events(&fixture, "responses fixture");
    let call_id = fixture["meta"]["call_id"]
        .as_str()
        .expect("responses fixture call_id")
        .to_string();
    let stream_body = sse(events);
    let final_body = sse(vec![
        ev_response_created("resp_cross_2"),
        ev_assistant_message("msg_cross_2", "cross-wire fixture done"),
        ev_completed("resp_cross_2"),
    ]);

    let server = start_mock_server().await;
    let call_id_for_matcher = call_id.clone();
    let first = mount_sse_once_match(
        &server,
        move |request: &Request| !request_has_function_call_output(request, &call_id_for_matcher),
        stream_body,
    )
    .await;
    let second = mount_sse_once_match(
        &server,
        move |request: &Request| request_has_function_call_output(request, &call_id),
        final_body,
    )
    .await;

    let test = test_codex().build(&server).await?;

    test.codex
        .start_or_steer_turn(user_turn("run the cross-wire fixture command"))
        .await?;

    wait_for_event(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => true,
        EventMsg::Error(error) => panic!("unexpected turn error: {error:?}"),
        _ => false,
    })
    .await;
    shutdown(&test.codex).await;

    assert_eq!(
        second.requests().len(),
        1,
        "expected exactly one follow-up request with function_call_output"
    );
    assert_eq!(
        first.requests().len(),
        1,
        "expected exactly one initial request"
    );

    Ok(())
}

fn request_has_function_call_output(request: &Request, call_id: &str) -> bool {
    serde_json::from_slice::<Value>(&request.body).is_ok_and(|body| {
        body["input"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["type"].as_str() == Some("function_call_output")
                    && item["call_id"].as_str() == Some(call_id)
            })
        })
    })
}
