use super::*;
use crate::client::wire::PREEMPTED_TOOL_CALL_RESULT;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemContent;
use pretty_assertions::assert_eq;

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

fn open_reasoning(text: &str) -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: Some(vec![ReasoningItemContent::ReasoningText {
            text: text.to_string(),
        }]),
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn whole_tool_call_is_kept_and_paired_with_the_interrupted_result() {
    let arguments = r#"{"cmd":"echo keep"}"#;
    let pending = vec![function_call("call-whole", arguments)];

    let reconciled = reconcile_preempted(pending);

    assert_eq!(
        reconciled,
        vec![
            function_call("call-whole", arguments),
            interrupted_result("call-whole"),
        ],
        "a call whose argument JSON arrived whole keeps its bytes and gets the fixed result"
    );
}

#[test]
fn truncated_tool_call_is_dropped_whole() {
    let pending = vec![function_call("call-cut", r#"{"cmd":"ec"#)];

    assert_eq!(reconcile_preempted(pending), vec![]);
}

#[test]
fn retained_and_dropped_calls_keep_stream_order_with_each_pair_adjacent() {
    let whole = r#"{"cmd":"echo keep"}"#;
    let pending = vec![
        function_call("call-cut", r#"{"cmd":"ec"#),
        function_call("call-first", whole),
        function_call("call-second", ""),
    ];

    let reconciled = reconcile_preempted(pending);

    assert_eq!(
        reconciled,
        vec![
            function_call("call-first", whole),
            interrupted_result("call-first"),
            function_call("call-second", ""),
            interrupted_result("call-second"),
        ],
        "every retained call is followed immediately by its own result"
    );
}

#[test]
fn received_text_is_kept() {
    let pending = vec![assistant_message("partial answer")];

    assert_eq!(reconcile_preempted(pending), pending);
}

#[test]
fn unfinished_reasoning_content_is_dropped() {
    let pending = vec![open_reasoning("half a thought"), assistant_message("text")];

    assert_eq!(
        reconcile_preempted(pending),
        vec![assistant_message("text")]
    );
}

#[test]
fn blank_assistant_text_is_not_replayed() {
    let pending = vec![assistant_message("   ")];

    assert_eq!(reconcile_preempted(pending), vec![]);
}
