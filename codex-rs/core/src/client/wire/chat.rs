//! Chat Completions wire (fork addition): request building and streaming
//! dispatch for `WireApi::Chat` providers, adapted from PR #12234.

use std::sync::Arc;

use codex_api::ApiError;
use codex_api::ChatCompletionsClient as ApiChatCompletionsClient;
use codex_api::ChatCompletionsOptions as ApiChatCompletionsOptions;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_otel::SessionTelemetry;
use codex_protocol::error::Result;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_response_debug_context::extract_response_debug_context;
use codex_response_debug_context::extract_response_debug_context_from_api_error;
use codex_rollout_trace::InferenceTraceContext;
use codex_tools::create_tools_json_for_chat_completions;
use serde_json::json;

use super::content_items_to_text;
use crate::client::AuthRequestTelemetryContext;
use crate::client::CHAT_COMPLETIONS_ENDPOINT;
use crate::client::ModelClientSession;
use crate::client::PendingUnauthorizedRetry;
use crate::client::RequestRouteTelemetry;
use crate::client::handle_unauthorized;
use crate::client::map_response_stream;
use crate::client_common::Prompt;
use crate::client_common::ResponseStream;
use crate::responses_metadata::CodexResponsesMetadata;

impl ModelClientSession {
    #[allow(clippy::too_many_arguments)]
    /// Streams a turn via the Chat Completions API.
    ///
    /// Fork addition: upstream codex removed this wire; this is the PR #12234
    /// blueprint adapted to the current client surface so chat-only providers
    /// work in-process without an external translation layer.
    pub(crate) async fn stream_chat_completions(
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
                .build_api_transport(&client_setup.api_provider, CHAT_COMPLETIONS_ENDPOINT)?;
            let request_auth_context = AuthRequestTelemetryContext::new(
                client_setup.auth.as_ref().map(CodexAuth::auth_mode),
                client_setup.api_auth.as_ref(),
                client_setup.agent_identity_telemetry.clone(),
                pending_retry,
            );
            let (request_telemetry, sse_telemetry) = Self::build_streaming_telemetry(
                session_telemetry,
                request_auth_context,
                RequestRouteTelemetry::for_endpoint(CHAT_COMPLETIONS_ENDPOINT),
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
            let mut options = ApiChatCompletionsOptions {
                session_id: responses_options.session_id,
                thread_id: responses_options.thread_id,
                session_source: responses_options.session_source,
                extra_headers: responses_options.extra_headers,
                compression: responses_options.compression,
            };

            let request = self.build_chat_request(prompt, model_info)?;
            let client = ApiChatCompletionsClient::new(
                transport,
                client_setup.api_provider,
                client_setup.api_auth,
            )
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

    /// Builds a request body for Chat Completions-style providers from the
    /// Responses-shaped prompt.
    ///
    /// Chat Completions has no Responses-only controls (store, prompt-cache,
    /// reasoning, include); those degrade away per CONTEXT.md's degradation
    /// table.
    pub(crate) fn build_chat_request(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
    ) -> Result<serde_json::Value> {
        let instructions = &prompt.base_instructions.text;
        let input = prompt.get_formatted_input_for_request(/*use_responses_lite*/ false);
        let messages = build_chat_messages(instructions, input);
        let tools = create_tools_json_for_chat_completions(&prompt.tools)?;

        let mut request = json!({
            "model": model_info.slug.clone(),
            "messages": messages,
            "stream": true,
            "stream_options": {
                "include_usage": true,
            }
        });

        if !tools.is_empty()
            && let Some(obj) = request.as_object_mut()
        {
            obj.insert("tools".to_string(), serde_json::Value::Array(tools));
            obj.insert(
                "tool_choice".to_string(),
                serde_json::Value::String("auto".to_string()),
            );
            obj.insert(
                "parallel_tool_calls".to_string(),
                serde_json::Value::Bool(prompt.parallel_tool_calls),
            );
        }

        Ok(request)
    }
}

/// Converts Responses-shaped conversation items into Chat Completions
/// messages (fork addition, adapted from PR #12234).
///
/// Consecutive tool calls are coalesced into a single assistant `tool_calls`
/// array: strict OpenAI-compatible servers reject a replay that splits
/// parallel calls across multiple assistant messages (HTTP 400, "must be
/// followed by tool messages"), per the official function-calling guide.
pub(crate) fn build_chat_messages(
    instructions: &str,
    input: Vec<ResponseItem>,
) -> Vec<serde_json::Value> {
    let mut messages = Vec::new();
    let mut pending_tool_calls: Vec<serde_json::Value> = Vec::new();

    if !instructions.trim().is_empty() {
        messages.push(json!({
            "role": "system",
            "content": instructions,
        }));
    }

    macro_rules! flush_tool_calls {
        () => {
            if !pending_tool_calls.is_empty() {
                messages.push(json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": std::mem::take(&mut pending_tool_calls),
                }));
            }
        };
    }

    for item in input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                flush_tool_calls!();
                if let Some(text) = content_items_to_text(&content) {
                    messages.push(json!({
                        "role": map_chat_role(&role),
                        "content": text,
                    }));
                }
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            }
            | ResponseItem::CustomToolCall {
                name,
                input: arguments,
                call_id,
                ..
            } => {
                pending_tool_calls.push(json!({
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments,
                    }
                }));
            }
            ResponseItem::LocalShellCall {
                call_id,
                id,
                action,
                ..
            } => {
                flush_tool_calls!();
                let call_id = call_id.or_else(|| id.map(|id| id.to_string()));
                if let Some(call_id) = call_id {
                    messages.push(json!({
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": "shell",
                                "arguments": serde_json::to_string(&action).unwrap_or_else(|_| "{}".to_string()),
                            }
                        }]
                    }));
                }
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                flush_tool_calls!();
                let call_id = call_id.unwrap_or_default();
                let text = output
                    .text_content()
                    .map(ToString::to_string)
                    .or_else(|| output.body.to_text())
                    .unwrap_or_default();
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": text,
                }));
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                flush_tool_calls!();
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": output,
                }));
            }
            _ => {}
        }
    }
    flush_tool_calls!();

    messages
}

pub(crate) fn map_chat_role(role: &str) -> &str {
    match role {
        "developer" => "system",
        "user" | "assistant" | "system" | "tool" => role,
        _ => "user",
    }
}
