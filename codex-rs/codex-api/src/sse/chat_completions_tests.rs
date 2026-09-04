use super::*;
use codex_client::TransportError;
use futures::TryStreamExt;
use tokio_util::io::ReaderStream;

/// Drives `process_chat_sse` over an SSE body and collects every event until
/// the channel closes.
async fn run_chat_sse(body: String) -> Vec<Result<ResponseEvent, ApiError>> {
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(64);
    let stream = ReaderStream::new(std::io::Cursor::new(body))
        .map_err(|err| TransportError::Network(err.to_string()));
    tokio::spawn(super::process_chat_sse(
        Box::pin(stream),
        tx,
        std::time::Duration::from_secs(30),
        /*telemetry*/ None,
    ));
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    events
}

fn chunk(delta: serde_json::Value, finish: Option<&str>) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish,
            }]
        })
    )
}

fn assert_reasoning_done(events: &[Result<ResponseEvent, ApiError>], expected_text: &str) -> usize {
    let positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(index, ev)| match ev {
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
                content,
                encrypted_content,
                ..
            })) => {
                assert_eq!(
                    encrypted_content.as_deref(),
                    None,
                    "chat reasoning carries no signature"
                );
                let content = content.as_ref().expect("reasoning content present");
                let text = match &content[0] {
                    ReasoningItemContent::ReasoningText { text } => text,
                    other => panic!("unexpected content part: {other:?}"),
                };
                assert_eq!(text, expected_text, "reasoning text must round-trip");
                Some(index)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        positions.len(),
        1,
        "expected exactly one reasoning Done: {events:?}"
    );
    positions[0]
}

#[tokio::test]
async fn chat_sse_reasoning_then_content_synthesizes_reasoning_item() {
    let mut body = String::new();
    body.push_str(&chunk(
        serde_json::json!({"reasoning_content": "chain "}),
        None,
    ));
    body.push_str(&chunk(
        serde_json::json!({"reasoning_content": "of thought"}),
        None,
    ));
    body.push_str(&chunk(serde_json::json!({"content": "answer"}), None));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 5,
                "total_tokens": 17,
                "completion_tokens_details": {"reasoning_tokens": 42}
            }
        })
    ));
    body.push_str("data: [DONE]\n\n");

    let events = run_chat_sse(body).await;

    // Event-order contract: start (Added) precedes the deltas, the complete
    // Reasoning item closes at the switch point, then the message stream.
    assert!(
        matches!(events[0], Ok(ResponseEvent::Created)),
        "first event must be Created: {events:?}"
    );
    assert!(
        matches!(
            events[1],
            Ok(ResponseEvent::OutputItemAdded(
                ResponseItem::Reasoning { .. }
            ))
        ),
        "reasoning Added must precede deltas: {events:?}"
    );
    assert!(
        matches!(
            &events[2],
            Ok(ResponseEvent::ReasoningContentDelta { delta, content_index: 0 })
                if delta == "chain "
        ),
        "first delta: {events:?}"
    );
    assert!(
        matches!(
            &events[3],
            Ok(ResponseEvent::ReasoningContentDelta { delta, content_index: 0 })
                if delta == "of thought"
        ),
        "second delta: {events:?}"
    );
    let done_at = assert_reasoning_done(&events, "chain of thought");
    assert!(
        done_at < events.len()
            && events[done_at + 1..].iter().any(|ev| matches!(
                ev,
                Ok(ResponseEvent::OutputItemAdded(ResponseItem::Message { .. }))
            )),
        "message Added must follow reasoning Done: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, Ok(ResponseEvent::OutputTextDelta(text)) if text == "answer")),
        "text delta: {events:?}"
    );
    let completed = events
        .iter()
        .find_map(|ev| match ev {
            Ok(ResponseEvent::Completed { token_usage, .. }) => token_usage.clone(),
            _ => None,
        })
        .expect("completed event");
    assert_eq!(completed.reasoning_output_tokens, 42);
}

#[tokio::test]
async fn chat_sse_reasoning_only_turn_still_closes_reasoning_item() {
    let mut body = String::new();
    body.push_str(&chunk(
        serde_json::json!({"reasoning_content": "silent reasoning"}),
        None,
    ));
    body.push_str("data: [DONE]\n\n");

    let events = run_chat_sse(body).await;

    assert_reasoning_done(&events, "silent reasoning");
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, Ok(ResponseEvent::OutputTextDelta(_)))),
        "reasoning-only turn must not produce text deltas: {events:?}"
    );
    assert!(
        !events.iter().any(|ev| matches!(
            ev,
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { .. }))
        )),
        "reasoning-only turn must not produce a message item: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, Ok(ResponseEvent::Completed { .. }))),
        "turn must still complete: {events:?}"
    );
}

#[tokio::test]
async fn chat_sse_coalesces_vllm_renamed_reasoning_field() {
    let mut body = String::new();
    // vLLM 0.18+ renamed reasoning_content to reasoning; the old name shows
    // up as an explicit null.
    body.push_str(&chunk(
        serde_json::json!({"reasoning_content": null, "reasoning": "renamed "}),
        None,
    ));
    body.push_str(&chunk(serde_json::json!({"reasoning": "thinking"}), None));
    body.push_str(&chunk(serde_json::json!({"content": "ok"}), None));
    body.push_str("data: [DONE]\n\n");

    let events = run_chat_sse(body).await;

    assert_reasoning_done(&events, "renamed thinking");
}

#[tokio::test]
async fn chat_sse_empty_reasoning_delta_never_opens_reasoning_item() {
    let mut body = String::new();
    body.push_str(&chunk(serde_json::json!({"reasoning_content": ""}), None));
    body.push_str(&chunk(serde_json::json!({"content": "direct"}), None));
    body.push_str("data: [DONE]\n\n");

    let events = run_chat_sse(body).await;

    assert!(
        !events.iter().any(|ev| matches!(
            ev,
            Ok(ResponseEvent::OutputItemAdded(
                ResponseItem::Reasoning { .. }
            ))
        )),
        "empty reasoning must not open an item: {events:?}"
    );
    assert!(
        !events.iter().any(|ev| matches!(
            ev,
            Ok(ResponseEvent::OutputItemDone(
                ResponseItem::Reasoning { .. }
            ))
        )),
        "empty reasoning must not produce an item: {events:?}"
    );
}

#[test]
fn merge_tool_call_deltas_concatenates_partial_chunks() {
    let mut aggregated = Vec::new();
    merge_tool_call_deltas(
        &mut aggregated,
        vec![ChatToolCallDelta {
            index: Some(0),
            id: Some("call_1".to_string()),
            function: Some(ChatFunctionDelta {
                name: Some("exec_".to_string()),
                arguments: Some("{\"cmd\":\"".to_string()),
            }),
        }],
    );
    merge_tool_call_deltas(
        &mut aggregated,
        vec![ChatToolCallDelta {
            index: Some(0),
            id: None,
            function: Some(ChatFunctionDelta {
                name: Some("command".to_string()),
                arguments: Some("pwd\"}".to_string()),
            }),
        }],
    );

    assert_eq!(aggregated.len(), 1);
    assert_eq!(aggregated[0].id.as_deref(), Some("call_1"));
    assert_eq!(aggregated[0].name, "exec_command");
    assert_eq!(aggregated[0].arguments, "{\"cmd\":\"pwd\"}");
}

#[tokio::test]
async fn emit_chat_completion_items_emits_tool_message_and_completed() {
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
    let tool_calls = vec![AggregatedToolCall {
        id: Some("call_1".to_string()),
        name: "exec_command".to_string(),
        arguments: "{\"cmd\":\"pwd\"}".to_string(),
    }];

    let result = emit_chat_completion_items(
        &tx,
        "done",
        &tool_calls,
        "resp_1".to_string(),
        None,
        /*length_truncated*/ false,
    )
    .await;
    assert!(result.is_ok());

    let first = rx.recv().await.expect("event").expect("ok event");
    let second = rx.recv().await.expect("event").expect("ok event");
    let third = rx.recv().await.expect("event").expect("ok event");

    match first {
        ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
            name,
            call_id,
            arguments,
            ..
        }) => {
            assert_eq!(name, "exec_command");
            assert_eq!(call_id, "call_1");
            assert_eq!(arguments, "{\"cmd\":\"pwd\"}");
        }
        other => panic!("unexpected first event: {other:?}"),
    }

    match second {
        ResponseEvent::OutputItemDone(ResponseItem::Message { role, content, .. }) => {
            assert_eq!(role, "assistant");
            assert_eq!(
                content,
                vec![ContentItem::OutputText {
                    text: "done".to_string(),
                }]
            );
        }
        other => panic!("unexpected second event: {other:?}"),
    }

    match third {
        ResponseEvent::Completed {
            response_id,
            end_turn,
            ..
        } => {
            assert_eq!(response_id, "resp_1");
            assert_eq!(end_turn, Some(true));
        }
        other => panic!("unexpected third event: {other:?}"),
    }
}

#[tokio::test]
async fn emit_chat_completion_items_maps_length_finish_to_context_window_error() {
    let (tx, _rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);

    // finish_reason = "length" means the upstream truncated the turn; the
    //Responses path surfaces this as context-window exhaustion so the agent
    // loop can compact-and-retry instead of replaying a truncated turn.
    let result = emit_chat_completion_items(
        &tx,
        "partial text",
        &[],
        "resp_2".to_string(),
        None,
        /*length_truncated*/ true,
    )
    .await;

    assert!(matches!(result, Err(ApiError::ContextWindowExceeded)));
}

/// Defect category A (tool-call index/state machine): a parallel two-tool
/// stream where only the FIRST chunk of each tool carries `id`/`name`
/// (later fragments carry index + arguments only — the exact LiteLLM
/// #20711/#17246 hazard shape: `id: None` chunks must not be dropped, and
/// neither tool may bleed arguments into the other). The aggregate must
/// preserve BOTH calls with intact names and fully concatenated arguments.
#[tokio::test]
async fn chat_sse_two_parallel_tool_calls_survive_index_keyed_merge() {
    let mut body = String::new();
    // Tool 0: id+name on the first fragment only; later fragments are id-less.
    body.push_str(&chunk(
        serde_json::json!({
            "tool_calls": [{
                "index": 0,
                "id": "call_a",
                "type": "function",
                "function": {"name": "exec_command", "arguments": "{\"cmd\":\""},
            }]
        }),
        None,
    ));
    // Tool 1 interleaved BEFORE tool 0 finishes: deltas may interleave by
    // index, so the merge must be keyed on index, not arrival order alone.
    body.push_str(&chunk(
        serde_json::json!({
            "tool_calls": [{
                "index": 1,
                "id": "call_b",
                "type": "function",
                "function": {"name": "notify", "arguments": "{\"name\":\""},
            }]
        }),
        None,
    ));
    body.push_str(&chunk(
        serde_json::json!({
            "tool_calls": [{
                "index": 0,
                "function": {"arguments": "echo a\"}"},
            }]
        }),
        None,
    ));
    body.push_str(&chunk(
        serde_json::json!({
            "tool_calls": [{
                "index": 1,
                "function": {"arguments": "tone\"}"},
            }]
        }),
        None,
    ));
    body.push_str(&chunk(serde_json::json!({}), Some("tool_calls")));
    body.push_str("data: [DONE]\n\n");

    let events = run_chat_sse(body).await;

    let calls: Vec<(String, String, String)> = events
        .iter()
        .filter_map(|ev| match ev {
            Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            })) => Some((call_id.clone(), name.clone(), arguments.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        vec![
            (
                "call_a".to_string(),
                "exec_command".to_string(),
                "{\"cmd\":\"echo a\"}".to_string(),
            ),
            (
                "call_b".to_string(),
                "notify".to_string(),
                "{\"name\":\"tone\"}".to_string(),
            ),
        ],
        "both parallel tool calls must survive with intact names/arguments"
    );
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, Ok(ResponseEvent::Completed { .. }))),
        "turn must still complete: {events:?}"
    );
}

/// Comparable event signature shared by the framing differential below:
/// keeps only fields with downstream semantics so two runs of the same
/// stream can be compared for structural equality (≡ₛ at the event level).
fn comparable_event_signatures(
    events: &[Result<ResponseEvent, ApiError>],
) -> Vec<(&'static str, String)> {
    events
        .iter()
        .filter_map(|ev| match ev {
            Ok(ResponseEvent::Created) => Some(("Created", String::new())),
            Ok(ResponseEvent::OutputItemAdded(item)) => match item {
                ResponseItem::Reasoning { .. } => Some(("ReasoningAdded", String::new())),
                ResponseItem::Message { .. } => Some(("MessageAdded", String::new())),
                _ => None,
            },
            Ok(ResponseEvent::ReasoningContentDelta { delta, .. }) => {
                Some(("ReasoningDelta", delta.clone()))
            }
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Reasoning { content, .. })) => {
                let text = match &content.as_ref()?[0] {
                    ReasoningItemContent::ReasoningText { text } => text.clone(),
                    _ => String::new(),
                };
                Some(("ReasoningDone", text))
            }
            Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            })) => Some(("FunctionCallDone", format!("{call_id}|{name}|{arguments}"))),
            Ok(ResponseEvent::OutputTextDelta(text)) => Some(("TextDelta", text.clone())),
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. })) => {
                let text = match &content[0] {
                    ContentItem::OutputText { text } => text.clone(),
                    _ => String::new(),
                };
                Some(("MessageDone", text))
            }
            Ok(ResponseEvent::Completed { response_id, .. }) => {
                Some(("Completed", response_id.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Defect category E (SSE framing): transport chunk boundaries carry no
/// semantics — the same stream re-chunked byte-by-byte (the adversarial
/// chunking profile from llm-stream-tck, applied by hand) must aggregate to
/// the exact same event signatures as the whole-stream read. A parser that
/// assumes one transport chunk == one SSE frame silently corrupts frames
/// split across TCP segments.
#[tokio::test]
async fn chat_sse_byte_by_byte_rechunking_yields_identical_items() {
    let mut whole = String::new();
    whole.push_str(&chunk(
        serde_json::json!({"reasoning_content": "think"}),
        None,
    ));
    whole.push_str(&chunk(
        serde_json::json!({
            "id": "chat_split",
            "tool_calls": [{
                "index": 0,
                "id": "call_split",
                "type": "function",
                "function": {"name": "exec_command", "arguments": "{\"cmd\":\"pwd\"}"}
            }]
        }),
        None,
    ));
    whole.push_str(&chunk(serde_json::json!({}), Some("tool_calls")));
    whole.push_str("data: [DONE]\n\n");

    // Whole-stream baseline.
    let whole_events = run_chat_sse(whole.clone()).await;

    // Byte-by-byte: feed the SAME bytes through the parser one byte at a
    // time. Each transport item is a single byte; frame reassembly must be
    // done entirely by the SSE layer.
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(64);
    let byte_items: Vec<Result<bytes::Bytes, TransportError>> = whole
        .into_bytes()
        .into_iter()
        .map(|byte| Ok(bytes::Bytes::from(vec![byte])))
        .collect();
    let stream = futures::stream::iter(byte_items);
    tokio::spawn(super::process_chat_sse(
        Box::pin(stream),
        tx,
        std::time::Duration::from_secs(30),
        /*telemetry*/ None,
    ));
    let mut split_events = Vec::new();
    while let Some(ev) = rx.recv().await {
        split_events.push(ev);
    }

    let whole_signatures = comparable_event_signatures(&whole_events);
    assert!(
        whole_signatures
            .iter()
            .any(|(kind, _)| *kind == "ReasoningDone")
            && whole_signatures
                .iter()
                .any(|(kind, _)| *kind == "FunctionCallDone"),
        "baseline must exercise reasoning + tool call: {whole_signatures:?}"
    );
    assert_eq!(
        whole_signatures,
        comparable_event_signatures(&split_events),
        "byte-by-byte re-chunking must not change the aggregated items"
    );
}

/// Defect category F (finish_reason mapping): a tool-call stream ending in
/// finish_reason="tool_calls" must synthesize exactly one Completed with
/// end_turn=Some(true) and usage_metadata=None (ADR-0002 Ruling 2), after
/// the function-call item — the terminal reason is data the turn loop
/// consumes, never a shortcut that skips the completion marker. Companion
/// to the length-mapping test: the two chat finish reasons with distinct
/// downstream semantics (tool_calls = the turn continues into a tool round;
/// length = ContextWindowExceeded) each get an explicit wire-level
/// assertion.
#[tokio::test]
async fn chat_sse_finish_reason_tool_calls_synthesizes_completed_with_end_turn() {
    let mut body = String::new();
    body.push_str(&chunk(
        serde_json::json!({
            "id": "chat_finish_marker",
            "tool_calls": [{
                "index": 0,
                "id": "call_finish",
                "type": "function",
                "function": {"name": "notify", "arguments": "{}"}
            }]
        }),
        None,
    ));
    body.push_str(&chunk(serde_json::json!({}), Some("tool_calls")));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({
            "id": "chat_finish_marker",
            "choices": [],
            "usage": {
                "prompt_tokens": 3,
                "completion_tokens": 1,
                "total_tokens": 4,
                "completion_tokens_details": {"reasoning_tokens": 0}
            }
        })
    ));
    body.push_str("data: [DONE]\n\n");

    let events = run_chat_sse(body).await;

    let mut tool_call_position = None;
    let mut completed_position = None;
    for (index, ev) in events.iter().enumerate() {
        match ev {
            Ok(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { call_id, .. }))
                if call_id == "call_finish" =>
            {
                assert!(tool_call_position.is_none(), "exactly one function call");
                tool_call_position = Some(index);
            }
            Ok(ResponseEvent::Completed { .. }) => {
                assert!(completed_position.is_none(), "exactly one Completed");
                completed_position = Some(index);
            }
            _ => {}
        }
    }
    let (tool_at, completed_at) = match (tool_call_position, completed_position) {
        (Some(tool_at), Some(completed_at)) => (tool_at, completed_at),
        _ => panic!("missing function call or Completed: {events:?}"),
    };
    assert!(
        tool_at < completed_at,
        "Completed must follow the function-call item: {events:?}"
    );
    match &events[completed_at] {
        Ok(ResponseEvent::Completed {
            response_id,
            end_turn,
            token_usage,
            usage_metadata,
            ..
        }) => {
            assert_eq!(response_id, "chat_finish_marker");
            assert_eq!(*end_turn, Some(true));
            assert!(
                usage_metadata.is_none(),
                "ADR-0002 Ruling 2: usage_metadata must be None"
            );
            assert_eq!(
                token_usage.as_ref().map(|usage| usage.input_tokens),
                Some(3),
                "usage frame must be parsed"
            );
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

/// Defect category C (termination invariants): the chat wire's terminator
/// is the explicit `data: [DONE]` frame — a stream whose bytes END before
/// [DONE] arrives was truncated upstream, and must surface as a terminal
/// Stream error with NO Completed event (a silent stop would replay a
/// partial turn as if it finished; the "opened but silent" stream is a
/// failure, not a normal end — dev.to/robinzzz four-state model, OmniRoute
/// #7699 semantics).
#[tokio::test]
async fn chat_sse_stream_closed_before_done_marker_is_terminal_error_not_completion() {
    let mut body = String::new();
    body.push_str(&chunk(
        serde_json::json!({"content": "partial answer that never got fin"}),
        None,
    ));
    // finish_reason and usage frame arrive, then the connection closes
    // WITHOUT the `data: [DONE]` sentinel.
    body.push_str(&chunk(serde_json::json!({}), Some("stop")));
    // No `data: [DONE]`; body ends here.

    let events = run_chat_sse(body).await;

    let terminal_error = events.iter().find(|ev| ev.is_err());
    assert!(
        matches!(terminal_error, Some(Err(ApiError::Stream(message))) if message
            .contains("stream closed")),
        "stream closed before [DONE] must be a terminal Stream error: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, Ok(ResponseEvent::Completed { .. }))),
        "no Completed may be synthesized for a truncated stream: {events:?}"
    );
}
