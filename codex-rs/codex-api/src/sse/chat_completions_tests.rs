use super::*;

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
