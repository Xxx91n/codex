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
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
}

impl MessageUsage {
    fn into_token_usage(self, prior_input_tokens: i64) -> TokenUsage {
        let input_tokens = if self.input_tokens > 0 {
            self.input_tokens
        } else {
            prior_input_tokens
        };
        TokenUsage {
            input_tokens,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: self.output_tokens,
            reasoning_output_tokens: 0,
            total_tokens: input_tokens + self.output_tokens,
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
            emit_messages_completion(
                &tx_event,
                &assistant_text,
                &tool_uses,
                response_id.unwrap_or_default(),
                output_usage.map(|usage| usage.into_token_usage(input_tokens)),
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
                    let _ = event.delta.and_then(|d| d.stop_reason);
                    output_usage = Some(usage);
                }
            }
            "message_stop" => {
                emit_messages_completion(
                    &tx_event,
                    &assistant_text,
                    &tool_uses,
                    response_id.unwrap_or_default(),
                    output_usage.map(|usage| usage.into_token_usage(input_tokens)),
                )
                .await;
                return;
            }
            "content_block_stop" | "ping" | "error" => {}
            other => {
                trace!("ignoring unhandled messages SSE event: {other}");
            }
        }
    }
}

async fn emit_messages_completion(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    assistant_text: &str,
    tool_uses: &BTreeMap<usize, AggregatedToolUse>,
    response_id: String,
    usage: Option<TokenUsage>,
) {
    for (index, call) in tool_uses {
        if call.name.trim().is_empty() {
            continue;
        }
        // Guard against max_tokens truncation leaving partial JSON that will
        // not parse; fall back to an empty object rather than failing.
        let arguments = if serde_json::from_str::<serde_json::Value>(&call.partial_json).is_ok() {
            call.partial_json.clone()
        } else {
            "{}".to_string()
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
        }))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn emit_messages_completion_emits_tool_message_and_completed() {
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

        emit_messages_completion(&tx, "done", &tool_uses, "msg_1".to_string(), None).await;

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
    async fn emit_messages_completion_guards_truncated_partial_json() {
        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
        let mut tool_uses: BTreeMap<usize, AggregatedToolUse> = BTreeMap::new();
        tool_uses.insert(
            0,
            AggregatedToolUse {
                id: "toolu_1".to_string(),
                name: "exec_command".to_string(),
                partial_json: "{\"cmd\":\"pw".to_string(),
            },
        );

        emit_messages_completion(&tx, "", &tool_uses, "msg_2".to_string(), None).await;

        let first = rx.recv().await.expect("event").expect("ok event");
        match first {
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { arguments, .. }) => {
                assert_eq!(arguments, "{}");
            }
            other => panic!("unexpected first event: {other:?}"),
        }
    }
}
