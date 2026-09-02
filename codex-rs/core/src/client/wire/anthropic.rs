//! Anthropic Messages wire (fork addition): request building and streaming
//! dispatch for `WireApi::Anthropic` providers (goose-blueprint in-process
//! transport).

use std::sync::Arc;

use codex_api::ApiError;
use codex_api::MessagesClient as ApiMessagesClient;
use codex_api::MessagesOptions as ApiMessagesOptions;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_otel::SessionTelemetry;
use codex_protocol::error::Result;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_response_debug_context::extract_response_debug_context;
use codex_response_debug_context::extract_response_debug_context_from_api_error;
use codex_rollout_trace::InferenceTraceContext;
use codex_tools::create_tools_json_for_anthropic;
use serde_json::json;
use tracing::debug;

use super::content_items_to_text;
use crate::client::ANTHROPIC_MESSAGES_ENDPOINT;
use crate::client::AuthRequestTelemetryContext;
use crate::client::DEFAULT_ANTHROPIC_MAX_TOKENS;
use crate::client::ModelClientSession;
use crate::client::PendingUnauthorizedRetry;
use crate::client::RequestRouteTelemetry;
use crate::client::handle_unauthorized;
use crate::client::map_response_stream;
use crate::client_common::Prompt;
use crate::client_common::ResponseStream;
use crate::responses_metadata::CodexResponsesMetadata;

impl ModelClientSession {
    /// Streams a turn via the Anthropic Messages API.
    ///
    /// Fork addition: goose-blueprint in-process transport so anthropic-native
    /// upstreams work without an external translation layer.
    pub(crate) async fn stream_anthropic_messages(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        responses_metadata: &CodexResponsesMetadata,
        inference_trace: &InferenceTraceContext,
    ) -> Result<ResponseStream> {
        let auth_manager = self.client.state.provider.auth_manager();
        let mut auth_recovery = auth_manager
            .as_ref()
            .map(AuthManager::unauthorized_recovery);
        let mut provider_auth_recovery_attempted = false;
        let mut pending_retry = PendingUnauthorizedRetry::default();
        loop {
            let client_setup = self.client.current_client_setup().await?;
            let transport = self
                .client
                .build_api_transport(&client_setup.api_provider, ANTHROPIC_MESSAGES_ENDPOINT)?;
            let request_auth_context = AuthRequestTelemetryContext::new(
                client_setup.auth.as_ref().map(CodexAuth::auth_mode),
                client_setup.api_auth.as_ref(),
                client_setup.agent_identity_telemetry.clone(),
                pending_retry,
            );
            let (request_telemetry, sse_telemetry) = Self::build_streaming_telemetry(
                session_telemetry,
                request_auth_context,
                RequestRouteTelemetry::for_endpoint(ANTHROPIC_MESSAGES_ENDPOINT),
                self.client.state.auth_env_telemetry.clone(),
            );
            let compression = self.responses_request_compression(client_setup.auth.as_ref());
            let responses_options = self
                .build_responses_options(
                    responses_metadata,
                    compression,
                    /*use_responses_lite*/ false,
                )
                .await;
            let mut options = ApiMessagesOptions {
                session_id: responses_options.session_id,
                thread_id: responses_options.thread_id,
                session_source: responses_options.session_source,
                extra_headers: responses_options.extra_headers,
                compression: responses_options.compression,
            };

            let request = self.build_messages_request(prompt, model_info)?;
            let client =
                ApiMessagesClient::new(transport, client_setup.api_provider, client_setup.api_auth)
                    .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
            let inference_trace_attempt = inference_trace.start_attempt();
            inference_trace_attempt.add_request_headers(&mut options.extra_headers);
            inference_trace_attempt.record_started(&request);

            match client.stream_request(request, options).await {
                Ok(stream) => {
                    let (stream, _) = map_response_stream(
                        stream,
                        session_telemetry.clone(),
                        inference_trace_attempt,
                        Arc::clone(&self.client.state.provider),
                    );
                    return Ok(stream);
                }
                Err(ApiError::Transport(unauthorized_transport))
                    if self
                        .client
                        .state
                        .provider
                        .is_recoverable_auth_error(&unauthorized_transport) =>
                {
                    let response_debug_context =
                        extract_response_debug_context(&unauthorized_transport);
                    inference_trace_attempt.record_failed(
                        &unauthorized_transport,
                        response_debug_context.request_id.as_deref(),
                        /*output_items*/ &[],
                    );
                    pending_retry = PendingUnauthorizedRetry::from_recovery(
                        handle_unauthorized(
                            unauthorized_transport,
                            &mut auth_recovery,
                            &mut provider_auth_recovery_attempted,
                            session_telemetry,
                            &self.client.state.provider,
                            self.client.event_sender.as_ref(),
                            responses_metadata.turn_id.as_deref(),
                        )
                        .await?,
                    );
                    continue;
                }
                Err(err) => {
                    let response_debug_context =
                        extract_response_debug_context_from_api_error(&err);
                    let err = self.client.state.provider.map_api_error(err);
                    inference_trace_attempt.record_failed(
                        &err,
                        response_debug_context.request_id.as_deref(),
                        /*output_items*/ &[],
                    );
                    return Err(err);
                }
            }
        }
    }

    /// Builds a request body for the Anthropic Messages API from the
    /// Responses-shaped prompt.
    ///
    /// The Messages API has no Responses-only controls (store, prompt-cache,
    /// reasoning, include); those degrade away per CONTEXT.md's degradation
    /// table. `system` is a top-level field rather than a message; tool
    /// results travel as `tool_result` blocks under a `user` message.
    pub(crate) fn build_messages_request(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
    ) -> Result<serde_json::Value> {
        let instructions = &prompt.base_instructions.text;
        let input = prompt.get_formatted_input_for_request(/*use_responses_lite*/ false);
        let messages = build_messages_messages(input);
        let tools = create_tools_json_for_anthropic(&prompt.tools)?;

        let mut request = json!({
            "model": model_info.slug.clone(),
            "messages": messages,
            "max_tokens": self
                .client
                .state
                .provider
                .info()
                .anthropic_max_tokens
                .unwrap_or(DEFAULT_ANTHROPIC_MAX_TOKENS),
            "stream": true,
        });

        let provider = self.client.state.provider.info();
        let mut tools = tools;

        if let Some(obj) = request.as_object_mut() {
            if provider.anthropic_prompt_caching.unwrap_or(false) {
                // Prompt caching: the system prompt becomes a block array so
                // the breakpoint marker has somewhere to live; the last tool
                // definition gets the same marker (one breakpoint per span).
                obj.insert(
                    "system".to_string(),
                    json!([{
                        "type": "text",
                        "text": instructions.clone(),
                        "cache_control": { "type": "ephemeral" },
                    }]),
                );
                if let Some(last) = tools.last_mut() {
                    last.as_object_mut().map(|t| {
                        t.insert("cache_control".to_string(), json!({ "type": "ephemeral" }))
                    });
                }
            } else {
                obj.insert(
                    "system".to_string(),
                    serde_json::Value::String(instructions.clone()),
                );
            }
            if !tools.is_empty() {
                obj.insert("tools".to_string(), serde_json::Value::Array(tools));
            }

            // Extended thinking: budget must be in [1024, max_tokens - 1].
            // Clamp rather than fail — a provider-level misconfig should not
            // abort a turn (degradation table: log + clamp).
            if let Some(budget) = provider.anthropic_thinking_budget {
                let max_tokens = provider
                    .anthropic_max_tokens
                    .unwrap_or(DEFAULT_ANTHROPIC_MAX_TOKENS);
                let clamped = budget.clamp(1024, max_tokens.saturating_sub(1));
                if clamped != budget {
                    debug!("anthropic thinking budget {budget} out of range; clamped to {clamped}");
                }
                obj.insert(
                    "thinking".to_string(),
                    json!({ "type": "enabled", "budget_tokens": clamped }),
                );
            }
        }

        Ok(request)
    }
}

/// Converts Responses-shaped input items to Anthropic `messages` blocks.
///
/// Consecutive assistant tool calls are coalesced into one assistant message
/// (Anthropic expects strict user/assistant alternation); tool results travel
/// as `tool_result` content blocks inside a user message. Per goose's
/// llm-bridge-rust#9287 lesson, a no-argument tool_use serializes `input` as
/// `{}` rather than `null`. Instructions travel as the top-level `system`
/// field (see `build_messages_request`), so they are not part of the messages
/// array.
pub(crate) fn build_messages_messages(input: Vec<ResponseItem>) -> Vec<serde_json::Value> {
    let mut messages: Vec<serde_json::Value> = Vec::new();
    let mut pending_assistant_blocks: Vec<serde_json::Value> = Vec::new();

    let flush_assistant =
        |messages: &mut Vec<serde_json::Value>,
         pending_assistant_blocks: &mut Vec<serde_json::Value>| {
            if !pending_assistant_blocks.is_empty() {
                messages.push(json!({
                    "role": "assistant",
                    "content": std::mem::take(pending_assistant_blocks),
                }));
            }
        };

    for item in input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let is_assistant = role == "assistant";
                let text = content_items_to_text(&content);
                let image_blocks = content_items_to_image_blocks(&content);
                if text.is_none() && image_blocks.is_empty() {
                    continue;
                }
                if is_assistant {
                    // Assistant turns cannot carry images on the Messages
                    // wire; images degrade to a mention in the text (already
                    // dropped by content_items_to_text context).
                    if let Some(text) = text {
                        pending_assistant_blocks.push(json!({
                            "type": "text",
                            "text": text,
                        }));
                    }
                } else {
                    flush_assistant(&mut messages, &mut pending_assistant_blocks);
                    // The Messages API only knows user/assistant; developer
                    // and system degrades to user per the degradation table.
                    let mut blocks: Vec<serde_json::Value> = Vec::new();
                    if let Some(text) = text {
                        blocks.push(json!({"type": "text", "text": text}));
                    }
                    blocks.extend(image_blocks);
                    messages.push(json!({
                        "role": "user",
                        "content": blocks,
                    }));
                }
            }
            ResponseItem::Reasoning {
                content,
                encrypted_content,
                ..
            } => {
                // Thinking replay: Anthropic requires thinking blocks be
                // passed back verbatim (text + signature) inside tool-use
                // rounds; without the signature the server 400s the whole
                // turn. Without a signature we drop the block — validation
                // was relaxed for non-tool turns (2026 steering docs).
                if let Some(signature) = encrypted_content {
                    let thinking: String = content
                        .unwrap_or_default()
                        .iter()
                        .map(|fragment| match fragment {
                            ReasoningItemContent::ReasoningText { text }
                            | ReasoningItemContent::Text { text } => text.clone(),
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    pending_assistant_blocks.push(json!({
                        "type": "thinking",
                        "thinking": thinking,
                        "signature": signature,
                    }));
                }
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                pending_assistant_blocks
                    .push(anthropic_tool_use_block(&name, &call_id, &arguments));
            }
            ResponseItem::CustomToolCall {
                call_id,
                name,
                input,
                ..
            } => {
                pending_assistant_blocks.push(anthropic_tool_use_block(&name, &call_id, &input));
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                flush_assistant(&mut messages, &mut pending_assistant_blocks);
                let text = output
                    .text_content()
                    .map(ToString::to_string)
                    .or_else(|| output.body.to_text())
                    .unwrap_or_default();
                push_anthropic_tool_result(&mut messages, &call_id.unwrap_or_default(), text);
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                flush_assistant(&mut messages, &mut pending_assistant_blocks);
                let text = output
                    .text_content()
                    .map(ToString::to_string)
                    .or_else(|| output.body.to_text())
                    .unwrap_or_default();
                push_anthropic_tool_result(&mut messages, &call_id, text);
            }
            _ => {}
        }
    }
    flush_assistant(&mut messages, &mut pending_assistant_blocks);

    messages
}

fn anthropic_tool_use_block(name: &str, id: &str, arguments: &str) -> serde_json::Value {
    // No-argument tools must serialize input as {} (not null) or the Messages
    // API rejects the replayed tool_use block with a 400.
    let input = match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(value) if !value.is_null() => value,
        _ => {
            debug!(
                "anthropic replay of tool '{name}' had unparseable arguments; using empty object"
            );
            json!({})
        }
    };
    json!({
        "type": "tool_use",
        "id": id,
        "name": name,
        "input": input,
    })
}

fn push_anthropic_tool_result(messages: &mut Vec<serde_json::Value>, call_id: &str, text: String) {
    let block = json!({
        "type": "tool_result",
        "tool_use_id": call_id,
        "content": text,
    });
    // Coalesce consecutive tool results into one user message: the Messages
    // API requires strict user/assistant alternation.
    if let Some(last) = messages.last_mut()
        && last["role"] == "user"
        && let Some(content) = last["content"].as_array_mut()
        && content.first().and_then(|block| block["type"].as_str()) == Some("tool_result")
    {
        content.push(block);
        return;
    }
    messages.push(json!({
        "role": "user",
        "content": [block],
    }));
}

/// Converts Responses `input_image` content items to Anthropic
/// `image` blocks. Only base64 data-URIs are accepted — the Messages API
/// has no URL-fetch variant, so plain http(s) URLs degrade to being dropped
/// (the text sibling already carries the fallback marker).
fn content_items_to_image_blocks(
    content: &[codex_protocol::models::ContentItem],
) -> Vec<serde_json::Value> {
    let mut blocks = Vec::new();
    for item in content {
        let codex_protocol::models::ContentItem::InputImage { image_url, .. } = item else {
            continue;
        };
        let Some(rest) = image_url.strip_prefix("data:") else {
            debug!("anthropic replay: non-data-URI image dropped: {image_url:?}");
            continue;
        };
        let Some((media_type, data)) = rest.split_once(";base64,") else {
            debug!("anthropic replay: malformed data-URI image dropped");
            continue;
        };
        blocks.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": data,
            },
        }));
    }
    blocks
}
