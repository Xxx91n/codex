use super::*;
use codex_client::TransportError;
use futures::TryStreamExt;
use tokio_util::io::ReaderStream;

#[test]
fn input_json_delta_fragments_concatenate_by_block_index() {
    let mut tool_uses: BTreeMap<usize, AggregatedToolUse> = BTreeMap::new();
    tool_uses.insert(
        0,
        AggregatedToolUse {
            id: "toolu_1".to_string(),
            name: "exec_command".to_string(),
            partial_json: String::new(),
        },
    );
    for fragment in ["{\"cmd\":\"p", "wd\"}"] {
        if let Some(entry) = tool_uses.get_mut(&0) {
            entry.partial_json.push_str(fragment);
        }
    }
    let entry = &tool_uses[&0];
    assert!(serde_json::from_str::<serde_json::Value>(&entry.partial_json).is_ok());
    assert_eq!(entry.partial_json, "{\"cmd\":\"pwd\"}");
}

#[tokio::test]
async fn finish_messages_stream_emits_tool_message_and_completed() {
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
    let mut tool_uses: BTreeMap<usize, AggregatedToolUse> = BTreeMap::new();
    tool_uses.insert(
        0,
        AggregatedToolUse {
            id: "toolu_1".to_string(),
            name: "exec_command".to_string(),
            partial_json: "{\"cmd\":\"pwd\"}".to_string(),
        },
    );

    finish_messages_stream(
        &tx,
        "done",
        &tool_uses,
        &std::collections::BTreeMap::new(),
        "msg_1".to_string(),
        None,
        Some("end_turn"),
    )
    .await;

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
            assert_eq!(call_id, "toolu_1");
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
            assert_eq!(response_id, "msg_1");
            assert_eq!(end_turn, Some(true));
        }
        other => panic!("unexpected third event: {other:?}"),
    }
}

#[tokio::test]
async fn finish_messages_stream_serializes_no_argument_tool_use_as_empty_object() {
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
    let mut tool_uses: BTreeMap<usize, AggregatedToolUse> = BTreeMap::new();
    tool_uses.insert(
        0,
        AggregatedToolUse {
            id: "toolu_1".to_string(),
            name: "notify".to_string(),
            // A no-argument tool_use carries no input_json_delta at all.
            partial_json: String::new(),
        },
    );

    finish_messages_stream(
        &tx,
        "",
        &tool_uses,
        &std::collections::BTreeMap::new(),
        "msg_2".to_string(),
        None,
        None,
    )
    .await;

    let first = rx.recv().await.expect("event").expect("ok event");
    match first {
        ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { arguments, .. }) => {
            assert_eq!(arguments, "{}");
        }
        other => panic!("unexpected first event: {other:?}"),
    }
}

#[tokio::test]
async fn finish_messages_stream_fails_loudly_on_truncated_tool_input() {
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
    let mut tool_uses: BTreeMap<usize, AggregatedToolUse> = BTreeMap::new();
    tool_uses.insert(
        0,
        AggregatedToolUse {
            id: "toolu_1".to_string(),
            name: "exec_command".to_string(),
            // max_tokens cut the input JSON off mid-way; substituting "{}" here
            // would fake a no-argument call (goose #7527/#7840 lesson).
            partial_json: "{\"cmd\":\"pw".to_string(),
        },
    );

    finish_messages_stream(
        &tx,
        "",
        &tool_uses,
        &std::collections::BTreeMap::new(),
        "msg_3".to_string(),
        None,
        Some("max_tokens"),
    )
    .await;

    let first = rx.recv().await.expect("event");
    match first {
        Err(ApiError::Stream(message)) => {
            assert!(message.contains("exec_command"));
            assert!(message.contains("max_tokens"));
        }
        other => panic!("expected terminal stream error, got: {other:?}"),
    }
    drop(tx);
    assert!(rx.recv().await.is_none(), "no completion after truncation");
}

#[test]
fn error_frame_payload_deserializes_official_shape() {
    let event: MessageEvent = serde_json::from_str(
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    )
    .expect("error frame parses");
    assert_eq!(event.event_type, "error");
    let body = event.error.expect("error body");
    assert_eq!(body.error_type.as_deref(), Some("overloaded_error"));
    assert_eq!(body.message.as_deref(), Some("Overloaded"));
}

#[test]
fn thinking_delta_and_signature_frames_parse() {
    let delta: MessageEvent = serde_json::from_str(
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
    )
    .expect("thinking_delta parses");
    assert_eq!(
        delta.delta.as_ref().unwrap().thinking.as_deref(),
        Some("hmm")
    );

    let sig: MessageEvent = serde_json::from_str(
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-abc"}}"#,
    )
    .expect("signature_delta parses");
    assert_eq!(
        sig.delta.as_ref().unwrap().signature.as_deref(),
        Some("sig-abc")
    );

    let start: MessageEvent = serde_json::from_str(
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
    )
    .expect("thinking block start parses");
    assert_eq!(start.content_block.as_ref().unwrap().block_type, "thinking");
}

#[tokio::test]
async fn finish_messages_stream_flushes_thinking_block_as_done_without_added() {
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
    let tool_uses: BTreeMap<usize, AggregatedToolUse> = BTreeMap::new();
    let mut thinking: BTreeMap<usize, AggregatedThinking> = BTreeMap::new();
    thinking.insert(
        0,
        AggregatedThinking {
            text: "let me reason".to_string(),
            signature: Some("sig-xyz".to_string()),
        },
    );

    finish_messages_stream(
        &tx,
        "",
        &tool_uses,
        &thinking,
        "msg_t".to_string(),
        None,
        Some("end_turn"),
    )
    .await;

    let mut events = Vec::new();
    // Close the channel before draining: finish_messages_stream only takes
    // &tx, so without an explicit drop the recv loop would never see EOF.
    drop(tx);
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    // The thinking block was never Added before the flush (no
    // content_block_start occurred), so flush must emit exactly one Done
    // and zero Added events.
    let added = events
        .iter()
        .filter(|ev| matches!(ev, Ok(ResponseEvent::OutputItemAdded(_))))
        .count();
    assert_eq!(added, 0, "flush must not emit Added: {events:?}");
    let done = events
        .iter()
        .find_map(|ev| match ev {
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
                content,
                encrypted_content,
                ..
            })) => Some((content.clone(), encrypted_content.clone())),
            _ => None,
        })
        .expect("reasoning item done from flush");
    assert_eq!(done.1, Some("sig-xyz".to_string()));
    let text = match &done.0.as_ref().unwrap()[0] {
        codex_protocol::models::ReasoningItemContent::ReasoningText { text } => text.clone(),
        other => panic!("unexpected content: {other:?}"),
    };
    assert_eq!(text, "let me reason");
}

#[tokio::test]
async fn truncated_stream_never_double_adds_active_thinking_block() {
    // Regression for the PONYTAIL yellow edge: a stream truncated by
    // max_tokens never sends content_block_stop for the thinking block that
    // already emitted OutputItemAdded on content_block_start, so the flush
    // path must NOT emit a second Added for it.
    let thinking_start = serde_json::json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {"type": "thinking", "id": "blk_1"}
    })
    .to_string();
    let thinking_delta = serde_json::json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "thinking_delta", "thinking": "partial reasoning"}
    })
    .to_string();
    let message_delta = serde_json::json!({
        "type": "message_delta",
        "delta": {"stop_reason": "max_tokens"},
        "usage": {"input_tokens": 10, "output_tokens": 5}
    })
    .to_string();
    let message_stop = serde_json::json!({"type": "message_stop"}).to_string();

    let mut body = String::new();
    for (kind, data) in [
        (
            "message_start",
            serde_json::json!({
                "type": "message_start",
                "message": {"id": "msg_trunc", "model": "claude-x",
                            "usage": {"input_tokens": 10, "output_tokens": 1}}
            })
            .to_string(),
        ),
        ("content_block_start", thinking_start),
        ("content_block_delta", thinking_delta),
        ("message_delta", message_delta),
        ("message_stop", message_stop),
    ] {
        body.push_str(&format!("event: {kind}\ndata: {data}\n\n"));
    }

    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(16);
    let stream = ReaderStream::new(std::io::Cursor::new(body))
        .map_err(|err| TransportError::Network(err.to_string()));
    tokio::spawn(super::process_messages_sse(
        Box::pin(stream),
        tx,
        std::time::Duration::from_secs(30),
        /*telemetry*/ None,
    ));

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }

    let added = events
        .iter()
        .filter(|ev| matches!(ev, Ok(ResponseEvent::OutputItemAdded(_))))
        .count();
    let done_reasoning = events
        .iter()
        .filter(|ev| {
            matches!(
                ev,
                Ok(ResponseEvent::OutputItemDone(
                    ResponseItem::Reasoning { .. }
                ))
            )
        })
        .count();
    assert_eq!(
        added, 1,
        "expected exactly one Added, got {added}: {events:?}"
    );
    assert_eq!(
        done_reasoning, 1,
        "expected exactly one reasoning Done from flush, got {done_reasoning}: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, Ok(ResponseEvent::Completed { .. }))),
        "stream should still complete after flush: {events:?}"
    );
}
