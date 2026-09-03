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
