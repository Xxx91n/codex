use super::*;
use crate::client::wire::PREEMPTED_TOOL_CALL_RESULT;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemContent;
use pretty_assertions::assert_eq;
use serde_json::json;

fn function_call(call_id: &str, arguments: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: "shell".to_string(),
        namespace: None,
        arguments: arguments.to_string(),
        encrypted_function_args: None,
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn interrupted_result(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some(call_id.to_string()),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(PREEMPTED_TOOL_CALL_RESULT.to_string()),
            success: Some(false),
        },
        internal_chat_message_metadata_passthrough: None,
    }
}

fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn thinking(text: &str, signature: Option<&str>) -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: Some(vec![ReasoningItemContent::ReasoningText {
            text: text.to_string(),
        }]),
        encrypted_content: signature.map(ToString::to_string),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn redacted_thinking(data: &str, signature: &str) -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: None,
        encrypted_content: Some(format!("{data}\0{signature}")),
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn whole_tool_use_is_kept_and_paired_with_the_interrupted_result() {
    let arguments = r#"{"command":"echo keep"}"#;
    let pending = vec![function_call("toolu-whole", arguments)];

    assert_eq!(
        reconcile_preempted(pending),
        vec![
            function_call("toolu-whole", arguments),
            interrupted_result("toolu-whole"),
        ],
    );
}

#[test]
fn truncated_tool_use_is_dropped_whole() {
    let pending = vec![function_call("toolu-cut", r#"{"command":"ec"#)];

    assert_eq!(reconcile_preempted(pending), vec![]);
}

#[test]
fn signed_thinking_and_received_text_survive() {
    let signed = thinking("thought to the end", "signature-abc");
    let pending = vec![
        signed.clone(),
        assistant_message("half an answer"),
        thinking("cut off inside the block", None),
    ];

    assert_eq!(
        reconcile_preempted(pending),
        vec![signed, assistant_message("half an answer")],
        "a signed block is text-complete, an unsigned one was cut off mid-block"
    );
}

#[test]
fn redacted_thinking_round_trips_only_with_both_halves() {
    let complete = redacted_thinking("opaque-payload", "signature-abc");
    let pending = vec![
        complete.clone(),
        redacted_thinking("opaque-payload", ""),
        redacted_thinking("", "signature-abc"),
    ];

    assert_eq!(reconcile_preempted(pending), vec![complete]);
}

#[test]
fn reconciled_preemption_replays_alternating_with_the_result_behind_its_call() {
    let arguments = r#"{"command":"echo keep"}"#;
    let mut history = reconcile_preempted(vec![
        thinking("cut off inside the block", None),
        function_call("toolu-whole", arguments),
        function_call("toolu-cut", r#"{"command":"ec"#),
        assistant_message("half an answer"),
    ]);
    history.push(user_message(
        "<turn_aborted>interrupted by a new message</turn_aborted>",
    ));
    history.push(user_message("the new message"));

    let messages = build_messages_messages(
        history,
        ThinkingEmission::Standard {
            unsigned: UnsignedReplay::Preserve,
        },
    );

    assert_eq!(
        messages,
        vec![
            json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "toolu-whole", "name": "shell", "input": {"command": "echo keep"}},
                ]
            }),
            json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "toolu-whole", "content": PREEMPTED_TOOL_CALL_RESULT},
                ]
            }),
            json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "half an answer"}],
            }),
            json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "<turn_aborted>interrupted by a new message</turn_aborted>"},
                    {"type": "text", "text": "the new message"},
                ]
            }),
        ],
        "the marker and the new input share the user message behind the tool result: \
         the Messages API rejects consecutive user messages, and neither the cut-off \
         thinking block nor the truncated tool_use may appear"
    );
}
