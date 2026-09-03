//! Per-wire request building and streaming modules (fork addition).
//!
//! The three registration points stay in `client.rs`: the `WireApi` enum
//! (codex-model-provider-info), `ModelProviderInfo`, and the `stream()`
//! dispatch match. Each per-wire module owns its request construction and
//! streaming loop so upstream syncs touch these files rather than
//! `client.rs`.

pub(crate) mod anthropic;
pub(crate) mod chat;

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
