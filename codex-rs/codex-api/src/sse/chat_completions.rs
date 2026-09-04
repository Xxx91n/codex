use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::telemetry::SseTelemetry;
use codex_client::ByteStream;
use codex_client::StreamResponse;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;
use tracing::trace;

const OPENAI_MODEL_HEADER: &str = "openai-model";

#[derive(Debug, Default)]
struct AggregatedToolCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    #[serde(default)]
    delta: ChatDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatDelta {
    #[serde(default)]
    content: Option<String>,
    /// Chain-of-thought delta: `reasoning_content` is the DeepSeek-established
    /// de-facto standard; vLLM 0.18+ renamed it to `reasoning`, so both names
    /// coalesce (first non-empty wins) during the ecosystem migration.
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct ChatToolCallDelta {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ChatFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct ChatFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    #[serde(default)]
    completion_tokens_details: Option<ChatCompletionTokensDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatCompletionTokensDetails {
    #[serde(default)]
    reasoning_tokens: Option<i64>,
}

impl From<ChatUsage> for TokenUsage {
    fn from(usage: ChatUsage) -> Self {
        TokenUsage {
            input_tokens: usage.prompt_tokens,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: usage.completion_tokens,
            reasoning_output_tokens: usage
                .completion_tokens_details
                .and_then(|details| details.reasoning_tokens)
                .unwrap_or(0),
            total_tokens: usage.total_tokens,
            codex_rollout_budget_units: None,
        }
    }
}

pub fn spawn_chat_completions_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) -> ResponseStream {
    let server_model = stream_response
        .headers
        .get(OPENAI_MODEL_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let upstream_request_id = stream_response
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(async move {
        if let Some(model) = server_model {
            let _ = tx_event.send(Ok(ResponseEvent::ServerModel(model))).await;
        }
        process_chat_sse(stream_response.bytes, tx_event, idle_timeout, telemetry).await;
    });

    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

async fn process_chat_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) {
    let mut stream = stream.eventsource();
    let mut created_emitted = false;
    let mut response_id: Option<String> = None;
    let mut usage: Option<TokenUsage> = None;
    let mut assistant_text = String::new();
    let mut assistant_item_open = false;
    let mut tool_calls: Vec<AggregatedToolCall> = Vec::new();
    let mut last_server_model: Option<String> = None;
    let mut length_truncated = false;
    // Chain-of-thought aggregation: `None` while no reasoning segment is open.
    // Chat has no explicit block boundaries, so the segment closes at the
    // first content/tool_calls delta (the switch point) or at [DONE].
    let mut reasoning_text: Option<String> = None;
    let mut reasoning_done = false;

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
                        "stream closed before chat completion finished".into(),
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
            close_reasoning_segment(&tx_event, &mut reasoning_text, &mut reasoning_done).await;
            if let Err(err) = emit_chat_completion_items(
                &tx_event,
                &assistant_text,
                &tool_calls,
                response_id.unwrap_or_default(),
                usage,
                length_truncated,
            )
            .await
            {
                let _ = tx_event.send(Err(err)).await;
            }
            return;
        }

        let event: ChatChunk = match serde_json::from_str(&sse.data) {
            Ok(event) => event,
            Err(err) => {
                trace!("ignoring non-json chat chunk: {err}");
                continue;
            }
        };

        if !created_emitted {
            let _ = tx_event.send(Ok(ResponseEvent::Created)).await;
            created_emitted = true;
        }

        if response_id.is_none() {
            response_id = event.id.clone();
        }
        if let Some(chunk_usage) = event.usage {
            usage = Some(chunk_usage.into());
        }
        if let Some(model) = event.model {
            let changed = match last_server_model.as_ref() {
                Some(last) => last != &model,
                None => true,
            };
            if changed {
                let _ = tx_event
                    .send(Ok(ResponseEvent::ServerModel(model.clone())))
                    .await;
                last_server_model = Some(model);
            }
        }

        for choice in event.choices {
            if let Some(reasoning) = reasoning_delta(&choice.delta) {
                if reasoning_done {
                    // The segment closed at its switch point; a later
                    // reasoning delta would have no active item to attach to.
                    // No known chat-compatible provider interleaves reasoning
                    // after content/tool_calls.
                    debug!("ignoring reasoning delta after reasoning segment closed");
                } else {
                    if reasoning_text.is_none() {
                        // Emit OutputItemAdded up front so ReasoningContentDelta
                        // events have an active item in core (same protocol as
                        // the Anthropic wire's thinking blocks).
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
                        reasoning_text = Some(String::new());
                    }
                    if let Some(text) = reasoning_text.as_mut() {
                        text.push_str(&reasoning);
                    }
                    let _ = tx_event
                        .send(Ok(ResponseEvent::ReasoningContentDelta {
                            delta: reasoning,
                            content_index: 0,
                        }))
                        .await;
                }
            }
            // The first content or tool_calls delta marks the end of the
            // reasoning segment: emit the complete Reasoning item now.
            if choice.delta.content.is_some() || choice.delta.tool_calls.is_some() {
                close_reasoning_segment(&tx_event, &mut reasoning_text, &mut reasoning_done).await;
            }
            if let Some(content) = choice.delta.content {
                if !assistant_item_open {
                    let msg = ResponseItem::Message {
                        id: None,
                        role: "assistant".to_string(),
                        content: vec![],
                        phase: None,
                        internal_chat_message_metadata_passthrough: None,
                    };
                    let _ = tx_event.send(Ok(ResponseEvent::OutputItemAdded(msg))).await;
                    assistant_item_open = true;
                }
                assistant_text.push_str(&content);
                let _ = tx_event
                    .send(Ok(ResponseEvent::OutputTextDelta(content)))
                    .await;
            }
            if let Some(delta_tool_calls) = choice.delta.tool_calls {
                merge_tool_call_deltas(&mut tool_calls, delta_tool_calls);
            }
            // A `length` finish means the upstream truncated output; surface it
            // as context-window exhaustion so core's retry/auto-compact path
            // engages instead of treating the truncated turn as complete.
            if choice.finish_reason.as_deref() == Some("length") {
                length_truncated = true;
            }
        }
    }
}

/// Coalesces the reasoning delta across the ecosystem's field-name split:
/// `reasoning_content` (DeepSeek/SGLang/Kimi) vs `reasoning` (vLLM 0.18+
/// rename, with `reasoning_content` deprecated to null). The first non-empty
/// value wins.
fn reasoning_delta(delta: &ChatDelta) -> Option<String> {
    delta
        .reasoning_content
        .clone()
        .filter(|text| !text.is_empty())
        .or_else(|| delta.reasoning.clone().filter(|text| !text.is_empty()))
}

/// Emits the completed Reasoning item for an open reasoning segment. Chat has
/// no block-stop event, so this runs at the first content/tool_calls delta
/// (the switch point) and again at [DONE] for reasoning-only turns. An empty
/// segment produces no item; a closed segment never re-emits.
async fn close_reasoning_segment(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    reasoning_text: &mut Option<String>,
    reasoning_done: &mut bool,
) {
    if let Some(text) = reasoning_text.take()
        && !*reasoning_done
    {
        *reasoning_done = true;
        if text.trim().is_empty() {
            return;
        }
        let item = ResponseItem::Reasoning {
            id: None,
            summary: Vec::new(),
            content: Some(vec![ReasoningItemContent::ReasoningText { text }]),
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
        };
        let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
    }
}

fn merge_tool_call_deltas(
    aggregated: &mut Vec<AggregatedToolCall>,
    deltas: Vec<ChatToolCallDelta>,
) {
    for delta in deltas {
        let index = delta.index.unwrap_or(0);
        if aggregated.len() <= index {
            aggregated.resize_with(index + 1, AggregatedToolCall::default);
        }
        let entry = &mut aggregated[index];
        if let Some(id) = delta.id {
            entry.id = Some(id);
        }
        if let Some(function) = delta.function {
            if let Some(name) = function.name {
                if entry.name.is_empty() {
                    entry.name = name;
                } else {
                    entry.name.push_str(&name);
                }
            }
            if let Some(arguments) = function.arguments {
                entry.arguments.push_str(&arguments);
            }
        }
    }
}

async fn emit_chat_completion_items(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    assistant_text: &str,
    tool_calls: &[AggregatedToolCall],
    response_id: String,
    usage: Option<TokenUsage>,
    length_truncated: bool,
) -> Result<(), ApiError> {
    // A truncated turn has no faithful replay: surface context-window
    // exhaustion (the Responses-path semantics) rather than emitting partial
    // items as if the turn completed.
    if length_truncated {
        debug!("chat completions stream finished with finish_reason=length");
        return Err(ApiError::ContextWindowExceeded);
    }

    for (index, call) in tool_calls.iter().enumerate() {
        if call.name.trim().is_empty() {
            continue;
        }
        let call_id = call
            .id
            .clone()
            .unwrap_or_else(|| format!("chat_tool_call_{index}"));
        let item = ResponseItem::FunctionCall {
            id: None,
            name: call.name.clone(),
            namespace: None,
            arguments: call.arguments.clone(),
            encrypted_function_args: None,
            call_id,
            internal_chat_message_metadata_passthrough: None,
        };
        if tx_event
            .send(Ok(ResponseEvent::OutputItemDone(item)))
            .await
            .is_err()
        {
            return Ok(());
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
            return Ok(());
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
    Ok(())
}

#[cfg(test)]
#[path = "chat_completions_tests.rs"]
mod tests;
