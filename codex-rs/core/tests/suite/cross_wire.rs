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

    // Exception declarations (ticket 14 checkpoint 4): fields with no
    // cross-wire counterpart must be DECLARED, not silently diverged. Every
    // declaration names its semantic, the native wire, each other wire's
    // behavior, and a fail-loud/omission policy — so the frozen-fixture
    // antipattern (an undocumented wire divergence) has nowhere to hide.
    let declarations = table["exception_declarations"]
        .as_array()
        .expect("exception_declarations array");
    assert!(
        !declarations.is_empty(),
        "table must declare its cross-wire exceptions"
    );
    let declared_ids: Vec<&str> = declarations
        .iter()
        .map(|entry| entry["id"].as_str().expect("declaration id"))
        .collect();
    assert_eq!(
        declared_ids.len(),
        declared_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        "declaration ids must be unique"
    );
    // The ticket-mandated exception surfaces: distinct terminal markers,
    // truncation fail-loud policies, and signature asymmetry.
    for required in [
        "usage_metadata_responses_only",
        "truncated_tool_use_input_json_anthropic",
        "finish_reason_length_chat",
        "thinking_signature_chat_absent",
    ] {
        assert!(
            declared_ids.contains(&required),
            "missing exception declaration {required}"
        );
    }
    for entry in declarations {
        let id = entry["id"].as_str().expect("declaration id");
        let semantic = entry["semantic"].as_str().expect("declaration semantic");
        assert!(
            semantics.contains(&semantic),
            "declaration {id} references unknown semantic {semantic}"
        );
        let field = entry["field"].as_str().expect("declaration field");
        assert!(!field.trim().is_empty(), "declaration {id} empty field");
        let native_wire = entry["native_wire"].as_str().expect("declaration wire");
        assert!(
            wires.contains(&native_wire),
            "declaration {id} unknown native wire {native_wire}"
        );
        let policy = entry["policy"].as_str().expect("declaration policy");
        assert!(
            policy.starts_with("fail-loud:") || policy.starts_with("omission"),
            "declaration {id} policy must state fail-loud or omission behavior"
        );
        let behaviors = entry["other_wires_behavior"]
            .as_object()
            .unwrap_or_else(|| panic!("declaration {id} must cover other wires"));
        for (wire, behavior) in behaviors {
            assert!(
                wires.contains(&wire.as_str()),
                "declaration {id} unknown wire {wire}"
            );
            assert!(
                behavior
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty()),
                "declaration {id} empty behavior for {wire}"
            );
        }
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

// ---------------------------------------------------------------------------
// Ticket 14: differential + round-trip structural-equality (≡ₛ) assertions.
//
// The three fixtures describe ONE semantic script (a model that thinks,
// then requests one shell tool call whose argument JSON is split across
// stream deltas, then the tool output feeds back and the second turn
// completes with plain text) on three different wires. The tests below
// treat them as the same input re-expressed per wire:
//   - the differential test drives each wire with the same semantic script
//     and asserts the shared per-wire invariants in one place;
//   - the ≡ₛ test asserts the semantic half of the IR the three wires
//     produce is STRUCTURALLY EQUAL (LLM-Rosetta's from_A(to_A(x)) ≡ₛ x,
//     applied here as same-script → same-IR across A = {responses, chat,
//     anthropic}): same FunctionCall name + same arguments JSON + same
//     assistant text + exactly one Completed per wire, ignoring
//     transport-layer identity fields (call_id domains differ per wire by
//     design, per ADR-0003's id-model divergence note).
// ---------------------------------------------------------------------------

/// The semantic payload every wire must reproduce: the tool's name, its
/// full argument object (reassembled from split deltas), and the second
/// turn's final text. Sourced from the three fixtures so the expectation
/// and the wire streams can never drift apart.
#[derive(Debug, Clone, PartialEq)]
struct SemanticScript {
    tool_name: String,
    tool_arguments: serde_json::Value,
    final_text: String,
}

fn script_from_fixtures() -> SemanticScript {
    let chat = parse_json(CHAT_TOOL_CALL_FIXTURE, "chat fixture");
    let anthropic = parse_json(ANTHROPIC_TOOL_CALL_FIXTURE, "anthropic fixture");
    let responses = parse_json(RESPONSES_TOOL_CALL_FIXTURE, "responses fixture");

    // All three fixtures must pin the SAME argument JSON on their first
    // turn; assert it here so a fixture edit that breaks the differential
    // script fails loudly at the source of the change.
    let chat_arguments = serde_json::from_str::<Value>(
        chat["meta"]["tool_arguments"]
            .as_str()
            .expect("chat meta tool_arguments"),
    )
    .expect("chat tool_arguments is json");
    let anthropic_arguments = serde_json::from_str::<Value>(
        anthropic["meta"]["tool_arguments"]
            .as_str()
            .expect("anthropic meta tool_arguments"),
    )
    .expect("anthropic tool_arguments is json");
    let responses_arguments = serde_json::from_str::<Value>(
        responses["meta"]["tool_arguments"]
            .as_str()
            .expect("responses meta tool_arguments"),
    )
    .expect("responses tool_arguments is json");
    assert_eq!(
        chat_arguments, anthropic_arguments,
        "chat and anthropic fixtures must script the same tool arguments"
    );
    assert_eq!(
        chat_arguments, responses_arguments,
        "chat and responses fixtures must script the same tool arguments"
    );

    let tool_name = chat["meta"]["tool_name"]
        .as_str()
        .expect("chat meta tool_name")
        .to_string();
    let final_text = "cross-wire fixture done";
    SemanticScript {
        tool_name,
        tool_arguments: chat_arguments,
        final_text: final_text.to_string(),
    }
}

/// The semantic half of an aggregated turn — the IR projection compared
/// under ≡ₛ. Transport identity (call_id / tool_use_id / item ids) is a
/// per-wire addressing domain and is deliberately excluded; what IS
/// compared is what the agent loop consumes: the tool call's name,
/// parsed argument object, assistant text, and the completion count.
#[derive(Debug, Default, PartialEq)]
struct WireIrSignature {
    function_calls: Vec<(String, Value)>, // (name, parsed arguments)
    assistant_texts: Vec<String>,
    completed_count: usize,
}

/// Slices the recorded items the three fixture roundtrips produce into the
/// comparable signature: the fixtures' `expect_ir` arrays are the
/// wire-side record of what each wire's turn yields on the ResponseItem
/// surface, so the ≡ₛ check compares those declared IR expectations
/// (already asserted end-to-end by the per-wire roundtrip tests above)
/// after normalizing identity away.
fn ir_signature_from_fixture_expect(fixture: &Value) -> WireIrSignature {
    let mut signature = WireIrSignature::default();
    let expect_ir = fixture["expect_ir"]
        .as_array()
        .unwrap_or_else(|| panic!("fixture has no expect_ir: {fixture:?}"));
    for entry in expect_ir {
        let text = entry.as_str().unwrap_or_default();
        if let Some(rest) = text.strip_prefix("FunctionCall{") {
            // FunctionCall{call_id:..., name:"shell", arguments:{...}}
            let name = rest
                .split("name:")
                .nth(1)
                .and_then(|after| after.split('"').nth(1))
                .unwrap_or_default()
                .to_string();
            let arguments_raw = rest
                .split("arguments:")
                .nth(1)
                .expect("FunctionCall entry must carry arguments");
            // Balance braces so a `}` inside the argument JSON cannot be
            // mistaken for the entry's closing brace (strip-based parsing
            // would silently truncate nested objects into a parse failure).
            let mut depth = 0usize;
            let mut end = arguments_raw.len();
            for (offset, ch) in arguments_raw.char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            end = offset + ch.len_utf8();
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let arguments_text = arguments_raw[..end].trim();
            let arguments = serde_json::from_str::<Value>(arguments_text).unwrap_or_else(|error| {
                panic!(
                    "fixture expect_ir FunctionCall arguments must be valid JSON \
                     (got {arguments_text:?}): {error}"
                )
            });
            signature.function_calls.push((name, arguments));
        } else if text.starts_with("Completed{") {
            signature.completed_count += 1;
        }
    }
    signature
}

/// Differential (ticket 14): the SAME semantic script (think → one shell
/// tool call with split-argument deltas → tool output → final text) runs
/// on all three wires; each wire must carry the full script through both
/// request turns. The three per-wire roundtrip tests above prove each wire
/// individually; this test pins that the three fixtures stay one script
/// (same tool name, same arguments, same call/replay shapes) so the
/// cross-wire table row "tool_call" cannot silently fork into three
/// divergent scenarios.
#[test]
fn cross_wire_same_script_differentially_asserted_on_three_wires() {
    let script = script_from_fixtures();

    // Per-wire invariant block: every wire's fixture must declare the
    // same tool name and the same argument JSON in its recorded requests
    // and stream, and its `expect_ir` must contain the shell call.
    for (wire, raw) in [
        ("responses", RESPONSES_TOOL_CALL_FIXTURE),
        ("chat", CHAT_TOOL_CALL_FIXTURE),
        ("anthropic", ANTHROPIC_TOOL_CALL_FIXTURE),
    ] {
        let fixture = parse_json(raw, "tool_call fixture");
        let signature = ir_signature_from_fixture_expect(&fixture);
        assert_eq!(
            signature.function_calls,
            vec![(script.tool_name.clone(), script.tool_arguments.clone())],
            "{wire} wire must aggregate exactly the scripted shell call"
        );
        assert_eq!(
            signature.completed_count, 1,
            "{wire} wire must complete exactly once"
        );
        assert_eq!(
            script.tool_arguments,
            serde_json::json!({"cmd": "echo cross-wire-fixture"}),
            "{wire} fixtures must keep the shared differential script"
        );
    }
}

/// Round-trip structural equality, ≡ₛ (ticket 14): the IR the three wires
/// produce for the SAME input script must be structurally equal —
/// same function-call name and parsed arguments, same completion count —
/// modulo per-wire transport identity (call_id/tool_use_id/fc id, which
/// ADR-0003 documents as intentionally divergent id models). This is the
/// in-repo analogue of LLM-Rosetta's from_A(to_A(x)) ≡ₛ x applied at the
/// fixture/IR layer: the three fixtures are the same x, the three
/// expect_ir records are the three to_A(x), and this asserts their
/// semantic projections agree.
#[test]
fn cross_wire_round_trip_ir_signatures_are_structurally_equal() {
    let signatures: Vec<(&str, WireIrSignature)> = [
        ("responses", RESPONSES_TOOL_CALL_FIXTURE),
        ("chat", CHAT_TOOL_CALL_FIXTURE),
        ("anthropic", ANTHROPIC_TOOL_CALL_FIXTURE),
    ]
    .into_iter()
    .map(|(wire, raw)| {
        let fixture = parse_json(raw, "tool_call fixture");
        (wire, ir_signature_from_fixture_expect(&fixture))
    })
    .collect();

    // Every wire aggregates exactly one scripted call and completes once.
    for (wire, signature) in &signatures {
        assert_eq!(
            signature.completed_count, 1,
            "{wire} must complete exactly once"
        );
        assert_eq!(
            signature.function_calls.len(),
            1,
            "{wire} must aggregate exactly one function call"
        );
    }

    // ≡ₛ: all three semantic projections are pairwise equal.
    let (reference_wire, reference) = &signatures[0];
    for (wire, signature) in &signatures[1..] {
        assert_eq!(
            *signature, *reference,
            "{wire} wire IR must be structurally equal to {reference_wire} wire IR \
             (≡ₛ, ignoring transport identity fields)"
        );
    }

    // And the equal projection is the scripted call itself — the round
    // trip back to the shared semantic is lossless.
    let script = script_from_fixtures();
    assert_eq!(
        reference.function_calls[0].0, script.tool_name,
        "IR must round-trip the scripted tool name"
    );
    assert_eq!(
        reference.function_calls[0].1, script.tool_arguments,
        "IR must round-trip the scripted arguments verbatim"
    );
}
