use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::telemetry::SseTelemetry;
use codex_client::ByteStream;
use codex_client::StreamResponse;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;
use tracing::trace;

#[derive(Debug, Default)]
struct AggregatedToolUse {
    id: String,
    name: String,
    /// Anthropic streams tool input as `input_json_delta` string fragments;
    /// concatenate and JSON-parse once at block stop (official contract).
    partial_json: String,
}

/// Streaming accumulator for an extended-thinking content block. The block
/// closes with a `signature_delta`; the signature is the encrypted blob the
/// replay side must echo back verbatim on tool-use turns, so it lands in
/// `ResponseItem::Reasoning::encrypted_content`.
#[derive(Debug, Default)]
struct AggregatedThinking {
    text: String,
    signature: Option<String>,
}

/// Streaming accumulator for a `redacted_thinking` content block. This
/// block carries an opaque encrypted `data` payload (no plaintext text)
/// and a `signature` from `signature_delta`. Both must round-trip
/// verbatim: the replay side re-emits them as a `redacted_thinking` block,
/// never merging their bytes into a thinking/text block (ticket 27 def-①).
#[derive(Debug, Default)]
struct AggregatedRedactedThinking {
    data: Option<String>,
    signature: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    message: Option<MessageStartBody>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    content_block: Option<ContentBlockStart>,
    #[serde(default)]
    delta: Option<MessageDelta>,
    #[serde(default)]
    usage: Option<MessageUsage>,
    #[serde(default)]
    error: Option<MessagesErrorBody>,
}

/// Payload of a terminal `event: error` frame (official streaming contract):
/// `{"type": "error", "error": {"type": "overloaded_error", "message": "..."}}`.
/// The official SDK raises on it instead of continuing to consume the stream.
#[derive(Debug, Deserialize)]
struct MessagesErrorBody {
    #[serde(rename = "type")]
    #[serde(default)]
    error_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageStartBody {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<MessageUsage>,
}

#[derive(Debug, Deserialize)]
struct ContentBlockStart {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    /// Opaque encrypted payload carried by `redacted_thinking` blocks:
    /// `content_block.data`. The replay side must echo it back verbatim
    /// alongside the signature (ticket 27 def-①).
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageDelta {
    #[serde(rename = "type")]
    delta_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
}

impl MessageUsage {
    fn into_token_usage(self, prior_input_tokens: i64) -> TokenUsage {
        // Defect ② (ticket 27 / A-004): Anthropic reports cache usage in
        // separate fields that `MessageUsage` previously dropped, so a
        // provider advertising prompt caching zeroed its own metering.
        // `cache_read_input_tokens` = tokens served from cache ->
        // `cached_input_tokens`; `cache_creation_input_tokens` = tokens
        // written into cache -> `cache_write_input_tokens`. The total is
        // the sum of all three input-side components plus output (aligned
        // to Anthropic's own total_tokens definition, which includes the
        // cache terms the way openai prompt_tokens already does — see the
        // cross-wire totals reconciliation note in A-004).
        let input_tokens = if self.input_tokens > 0 {
            self.input_tokens
        } else {
            prior_input_tokens
        };
        let cached_input_tokens = self.cache_read_input_tokens;
        let cache_write_input_tokens = self.cache_creation_input_tokens;
        let output_tokens = self.output_tokens;
        TokenUsage {
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens: 0,
            total_tokens: input_tokens
                + cached_input_tokens
                + cache_write_input_tokens
                + output_tokens,
            codex_rollout_budget_units: None,
        }
    }
}

pub fn spawn_anthropic_messages_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) -> ResponseStream {
    let upstream_request_id = stream_response
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(async move {
        process_messages_sse(stream_response.bytes, tx_event, idle_timeout, telemetry).await;
    });

    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

async fn process_messages_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) {
    let mut stream = stream.eventsource();
    let mut created_emitted = false;
    let mut response_id: Option<String> = None;
    let mut input_tokens: i64 = 0;
    let mut output_usage: Option<MessageUsage> = None;
    let mut assistant_text = String::new();
    let mut assistant_item_open = false;
    // Buffers keyed by content-block index, per the official streaming
    // contract; goose uses the same pattern so interleaved blocks stay sane.
    let mut tool_uses: BTreeMap<usize, AggregatedToolUse> = BTreeMap::new();
    let mut thinking_blocks: BTreeMap<usize, AggregatedThinking> = BTreeMap::new();
    let mut redacted_blocks: BTreeMap<usize, AggregatedRedactedThinking> = BTreeMap::new();
    let mut stop_reason: Option<String> = None;

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                debug!("SSE Error: {e:#}");
                let _ = tx_event.send(Err(ApiError::Stream(e.to_string()))).await;
                return;
            }
            Ok(None) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "stream closed before messages response finished".into(),
                    )))
                    .await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        if sse.data.trim() == "[DONE]" {
            finish_messages_stream(
                &tx_event,
                &assistant_text,
                &tool_uses,
                &thinking_blocks,
                &redacted_blocks,
                response_id.unwrap_or_default(),
                output_usage.map(|usage| usage.into_token_usage(input_tokens)),
                stop_reason.as_deref(),
            )
            .await;
            return;
        }

        let event: MessageEvent = match serde_json::from_str(&sse.data) {
            Ok(event) => event,
            Err(err) => {
                trace!("ignoring non-json messages event: {err}");
                continue;
            }
        };

        if !created_emitted {
            let _ = tx_event.send(Ok(ResponseEvent::Created)).await;
            created_emitted = true;
        }

        match event.event_type.as_str() {
            "message_start" => {
                if let Some(message) = event.message {
                    if response_id.is_none() {
                        response_id = message.id;
                    }
                    if let Some(model) = message.model {
                        let _ = tx_event.send(Ok(ResponseEvent::ServerModel(model))).await;
                    }
                    if let Some(usage) = message.usage {
                        input_tokens = usage.input_tokens;
                    }
                }
            }
            "content_block_start" => {
                if let (Some(index), Some(block)) = (event.index, &event.content_block)
                    && block.block_type == "thinking"
                {
                    thinking_blocks.entry(index).or_default();
                    // Emit OutputItemAdded up front so thinking_delta events
                    // (ReasoningContentDelta) have an active item in core.
                    let item = ResponseItem::Reasoning {
                        id: None,
                        summary: Vec::new(),
                        content: None,
                        encrypted_content: None,
                        internal_chat_message_metadata_passthrough: None,
                    };
                    let _ = tx_event
                        .send(Ok(ResponseEvent::OutputItemAdded(item)))
                        .await;
                }
                if let (Some(index), Some(block)) = (event.index, &event.content_block)
                if let (Some(index), Some(block)) = (event.index, &event.content_block)
                    && block.block_type == "redacted_thinking"
                {
                    // Preserve the opaque block verbatim (encrypted payload
                    // arrives as `content_block.data`; signature arrives via a
                    // later signature_delta). Dropped to `other => trace!`
                    // would silently vanish the block (ticket 27 def-①).
                    redacted_blocks.insert(
                        index,
                        AggregatedRedactedThinking {
                            data: block.data.clone(),
                            signature: None,
                        },
                    );
                }
                    && block.block_type == "tool_use"
                {
                    tool_uses.insert(
                        index,
                        AggregatedToolUse {
                            id: block.id.clone().unwrap_or_default(),
                            name: block.name.clone().unwrap_or_default(),
                            partial_json: String::new(),
                        },
                    );
                }
            }
            "content_block_delta" => {
                let index = event.index.unwrap_or(0);
                if let Some(delta) = event.delta {
                    match delta.delta_type.as_deref() {
                        Some("thinking_delta") => {
                            if let Some(thinking) = delta.thinking {
                                if let Some(entry) = thinking_blocks.get_mut(&index) {
                                    entry.text.push_str(&thinking);
                                }
                                let _ = tx_event
                                    .send(Ok(ResponseEvent::ReasoningContentDelta {
                                        delta: thinking,
                                        content_index: index as i64,
                                    }))
                                    .await;
                            }
                        }
                        Some("signature_delta") => {
                            if let Some(signature) = delta.signature {
                                if let Some(entry) = thinking_blocks.get_mut(&index) {
                                    entry.signature = Some(signature.clone());
                                }
                                // Redacted blocks also close with a signature;
                                // route it there so the opaque payload +
                                // signature round-trip together (ticket 27 def-①).
                                if let Some(entry) = redacted_blocks.get_mut(&index) {
                                    entry.signature = Some(signature);
                                }
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(fragment) = delta.partial_json
                                && let Some(entry) = tool_uses.get_mut(&index)
                            {
                                entry.partial_json.push_str(&fragment);
                            }
                        }
                        _ => {
                            if let Some(text) = delta.text {
                                if !assistant_item_open {
                                    let msg = ResponseItem::Message {
                                        id: None,
                                        role: "assistant".to_string(),
                                        content: vec![],
                                        phase: None,
                                        internal_chat_message_metadata_passthrough: None,
                                    };
                                    let _ = tx_event
                                        .send(Ok(ResponseEvent::OutputItemAdded(msg)))
                                        .await;
                                    assistant_item_open = true;
                                }
                                assistant_text.push_str(&text);
                                let _ = tx_event
                                    .send(Ok(ResponseEvent::OutputTextDelta(text)))
                                    .await;
                            }
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(usage) = event.usage {
                    output_usage = Some(usage);
                }
                if let Some(reason) = event.delta.and_then(|d| d.stop_reason) {
                    // stop_reason arrives only on message_delta per the official
                    // streaming contract; max_tokens means the output (possibly
                    // a tool_use's input JSON) was truncated.
                    if reason == "max_tokens" {
                        debug!("messages stream stopped at max_tokens; output may be truncated");
                    }
                    stop_reason = Some(reason.clone());
                    // Ticket 29 / A-007 step 0: budget-policy decisions must be
                    // measurable. Emit the upstream-declared terminal reason plus
                    // the observed output-token count at the same code position
                    // that handles stop_reason (ticket 27 defect ④); the default
                    // hook is a no-op, so Chat and Responses wires are unaffected.
                    if let Some(t) = telemetry.as_ref() {
                        let output_tokens = output_usage.as_ref().map(|u| u.output_tokens);
                        t.on_stop_reason(&reason, output_tokens);
                    }
                }
            }
            "message_stop" => {
                finish_messages_stream(
                    &tx_event,
                    &assistant_text,
                    &tool_uses,
                &thinking_blocks,
                &redacted_blocks,
                    response_id.unwrap_or_default(),
                    output_usage.map(|usage| usage.into_token_usage(input_tokens)),
                    stop_reason.as_deref(),
                )
                .await;
                return;
            }
            "content_block_stop" => {
                let index = event.index.unwrap_or(0);
                if let Some(thinking) = thinking_blocks.remove(&index) {
                    let item = ResponseItem::Reasoning {
                        id: None,
                        summary: Vec::new(),
                        content: Some(vec![
                            codex_protocol::models::ReasoningItemContent::ReasoningText {
                                text: thinking.text,
                            },
                        ]),
                        encrypted_content: thinking.signature,
                        internal_chat_message_metadata_passthrough: None,
                    };
                    let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
                }
                if let Some(redacted) = redacted_blocks.remove(&index) {
                    // Defect ① (ticket 27 / A-002): redacted_thinking round-
                    // trips as `content: None, encrypted_content: Some("data\0sig")`.
                    // The `\0` delimiter is fork-internal (the wire contract is
                    // unchanged: outbound splits to emit the native
                    // `{type:redacted_thinking, data, signature}` block). This
                    // encoding survives `should_serialize_reasoning_content`
                    // (content: None is never skipped; encrypted_content is
                    // always serialized) and `event_mapping` (content None
                    // surfaces as empty raw_content, so the opaque bytes do
                    // not leak into the UI). The discriminator vs. signed
                    // thinking is structural: signed thinking always carries
                    // `content: Some([ReasoningText{..}])` from the SSE; the
                    // anthropic outbound arm matches `content: None` first.
                    let data = redacted.data.unwrap_or_default();
                    let signature = redacted.signature.unwrap_or_default();
                    let combined = format!("{data}\0{signature}");
                    let item = ResponseItem::Reasoning {
                        id: None,
                        summary: Vec::new(),
                        content: None,
                        encrypted_content: Some(combined),
                        internal_chat_message_metadata_passthrough: None,
                    };
                    let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
                }
            }
            "ping" => {}
            "error" => {
                // Official SDK behavior: an error frame is terminal — surface it
                // instead of hanging until the idle timeout.
                let body = event.error;
                let kind = body
                    .as_ref()
                    .and_then(|b| b.error_type.as_deref())
                    .unwrap_or("unknown_error")
                    .to_string();
                let detail = body
                    .and_then(|b| b.message)
                    .unwrap_or_else(|| sse.data.clone());
                debug!("messages stream error frame: {kind}: {detail}");
                let _ = tx_event
                    .send(Err(ApiError::Stream(format!("{kind}: {detail}"))))
                    .await;
                return;
            }
            other => {
                trace!("ignoring unhandled messages SSE event: {other}");
            }
        }
    }
}

/// Emits the buffered items and the completion marker, or a terminal error
/// when the stream left a tool_use with truncated input JSON: replaying a
/// silently-substituted argument object would corrupt the agent loop (goose
/// issue #7527 / PR #7840 lesson — fail loud, do not fake a call).
async fn finish_messages_stream(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    assistant_text: &str,
    tool_uses: &BTreeMap<usize, AggregatedToolUse>,
    thinking_blocks: &BTreeMap<usize, AggregatedThinking>,
    redacted_blocks: &BTreeMap<usize, AggregatedRedactedThinking>,
    response_id: String,
    usage: Option<TokenUsage>,
    stop_reason: Option<&str>,
) {
    // Defect ④ (ticket 27 / A-006): stop_reason=max_tokens on a plain-text
    // turn means the upstream truncated the output with no tool_use and no
    // thinking captured — replaying would silently fake a legal empty turn.
    // Surface context-window exhaustion (the Responses/Chat-path semantics)
    // so core's auto-compact retry engages instead of treating the truncated
    // turn as complete. A tool_use turn still fails loud on its own
    // truncated-JSON path; a thinking turn still flushes its partial block.
    if stop_reason == Some("max_tokens")
        && thinking_blocks.is_empty()
        && redacted_blocks.is_empty()
        && tool_uses.is_empty()
    {
        debug!("messages stream stopped at max_tokens with no faithful output");
        let _ = tx_event.send(Err(ApiError::ContextWindowExceeded)).await;
        return;
    }
    for (index, thinking) in thinking_blocks {
        let item = ResponseItem::Reasoning {
            id: None,
            summary: Vec::new(),
            content: Some(vec![
                codex_protocol::models::ReasoningItemContent::ReasoningText {
                    text: thinking.text.clone(),
                },
            ]),
            encrypted_content: thinking.signature.clone(),
            internal_chat_message_metadata_passthrough: None,
        };
        let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
        let _ = index;
    }
    // Redacted thinking blocks carry an opaque encrypted payload (no
    // plaintext); flush any that never reached content_block_stop as their
    // faithful IR form so the bytes survive into the replay history
    // (ticket 27 def-①). Outbound rebuilds the native block
    // from the fork-internal "\0" combined encoding, same as the
    // content_block_stop path above.
    for (_index, redacted) in redacted_blocks {
        // Flush: same fork-internal `data\0sig` combined encoding as the
        // `content_block_stop` path (see comment there).
        let data = redacted.data.unwrap_or_default();
        let signature = redacted.signature.unwrap_or_default();
        let combined = format!("{data}\0{signature}");
        let item = ResponseItem::Reasoning {
            id: None,
            summary: Vec::new(),
            content: None,
            encrypted_content: Some(combined),
            internal_chat_message_metadata_passthrough: None,
        };
        let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
    }
    for (index, call) in tool_uses {
        if call.name.trim().is_empty() {
            continue;
        }
        // No-argument tool_use streams carry no input_json_delta; an empty
        // buffer legitimately means {}. A non-empty buffer that fails to parse
        // is truncated mid-JSON (max_tokens) and must surface as an error.
        let arguments = if call.partial_json.is_empty() {
            "{}".to_string()
        } else if serde_json::from_str::<serde_json::Value>(&call.partial_json).is_ok() {
            call.partial_json.clone()
        } else {
            let _ = tx_event
                .send(Err(ApiError::Stream(format!(
                    "tool_use '{}' input JSON truncated (stop_reason={})",
                    call.name,
                    stop_reason.unwrap_or("unknown"),
                ))))
                .await;
            return;
        };
        let call_id = if call.id.is_empty() {
            format!("toolu_stream_{index}")
        } else {
            call.id.clone()
        };
        let item = ResponseItem::FunctionCall {
            id: None,
            name: call.name.clone(),
            namespace: None,
            arguments,
            encrypted_function_args: None,
            call_id,
            internal_chat_message_metadata_passthrough: None,
        };
        if tx_event
            .send(Ok(ResponseEvent::OutputItemDone(item)))
            .await
            .is_err()
        {
            return;
        }
    }

    if !assistant_text.trim().is_empty() {
        let message = ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: assistant_text.to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
        if tx_event
            .send(Ok(ResponseEvent::OutputItemDone(message)))
            .await
            .is_err()
        {
            return;
        }
    }

    let _ = tx_event
        .send(Ok(ResponseEvent::Completed {
            response_id,
            token_usage: usage,
            end_turn: Some(true),
            usage_metadata: None,
        }))
        .await;
}

#[cfg(test)]
#[path = "messages_tests.rs"]
mod tests;
