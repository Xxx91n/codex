//! Per-wire request building and streaming modules (fork addition).
//!
//! The three registration points stay in `client.rs`: the `WireApi` enum
//! (codex-model-provider-info), `ModelProviderInfo`, and the `stream()`
//! dispatch match. Each per-wire module owns its request construction and
//! streaming loop so upstream syncs touch these files rather than
//! `client.rs`.

pub(crate) mod anthropic;
pub(crate) mod chat;
pub(crate) mod max_tokens;
pub(crate) mod reasoning_effort;

use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;

/// Result recorded for every tool call that a preemption interrupted before it
/// ran (spec S-06). One fixed sentence for all tools: no per-tool templates, no
/// reuse of the turn-level interrupt marker, and no claim that the call
/// succeeded.
pub(crate) const PREEMPTED_TOOL_CALL_RESULT: &str =
    "The tool call was interrupted before it ran and was not executed. Do not assume it succeeded.";

/// Pairs a retained tool call with the synthetic result that says it never ran
/// (spec S-04: the result goes directly behind its call, so neither API rejects
/// the replay for an unpaired call). Each wire's own `reconcile_preempted`
/// (`chat.rs` / `anthropic.rs`) decides which calls get one, because the
/// faithfulness rules are per-wire and must not be transplanted (see
/// wire/AGENTS.md "Does NOT carry over"); that reconciler is bound into the
/// stream at construction, so the turn loop never selects it (ADR-0015).
pub(crate) fn preempted_tool_call_result(call_id: &str) -> ResponseItem {
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

pub(crate) fn content_items_to_text(
    content: &[codex_protocol::models::ContentItem],
) -> Option<String> {
    let mut text_parts = Vec::new();
    for item in content {
        match item {
            codex_protocol::models::ContentItem::InputText { text }
            | codex_protocol::models::ContentItem::OutputText { text }
                if !text.trim().is_empty() =>
            {
                text_parts.push(text.clone());
            }
            _ => {}
        }
    }

    if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join("\n"))
    }
}
