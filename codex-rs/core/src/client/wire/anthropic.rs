//! Anthropic Messages wire (fork addition): request building and streaming
//! dispatch for `WireApi::Anthropic` providers (goose-blueprint in-process
//! transport).

use std::sync::Arc;

use codex_api::ApiError;
use codex_api::MessagesClient as ApiMessagesClient;
use codex_api::MessagesOptions as ApiMessagesOptions;
use codex_api::TransportError;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_otel::SessionTelemetry;
use codex_protocol::error::Result;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_response_debug_context::extract_response_debug_context;
use codex_response_debug_context::extract_response_debug_context_from_api_error;
use codex_rollout_trace::InferenceTraceContext;
use codex_tools::create_tools_json_for_anthropic;
use serde_json::json;
use tracing::debug;
use tracing::warn;

use super::content_items_to_text;
use super::preempted_tool_call_result;
use crate::client::ANTHROPIC_MESSAGES_ENDPOINT;
use crate::client::AuthRequestTelemetryContext;
use crate::client::ClientRouting;
use crate::client::DEFAULT_ANTHROPIC_MAX_TOKENS;
use crate::client::ModelClientSession;
use crate::client::PendingUnauthorizedRetry;
use crate::client::RequestRouteTelemetry;
use crate::client::handle_unauthorized;
use crate::client::map_response_stream;
use crate::client_common::Prompt;
use crate::client_common::ResponseStream;
use crate::responses_metadata::CodexResponsesMetadata;

/// Upstream class discriminator for the ADR-0009 conditional thinking-replay
/// rule: first-party Anthropic enforces signed thinking blocks on manual
/// tool-use turns, while third-party /anthropic-compatible endpoints
/// (DeepSeek/Kimi family gateways) never sign blocks and expect them back
/// verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnthropicUpstreamKind {
    /// An `anthropic.com` host: tiered signature enforcement applies.
    FirstParty,
    /// Everything else (gateways, compat endpoints, unset base_url): unsigned
    /// blocks must be preserved.
    Compatible,
}

impl AnthropicUpstreamKind {
    /// Host-based classification. Anything that is not an `anthropic.com`
    /// host fails open to `Compatible`: dropping a block a compatible
    /// endpoint expects back is a guaranteed 400 (D-009 finding 4), while
    /// preserving is the contract-neutral choice; first-party-only branches
    /// (the conditional drop, guard G1) then never see non-first-party
    /// traffic.
    pub(crate) fn from_base_url(base_url: Option<&str>) -> Self {
        let Some(base_url) = base_url else {
            return Self::Compatible;
        };
        let after_scheme = base_url
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(base_url);
        let authority = after_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or(after_scheme);
        let host = authority
            .rsplit('@')
            .next()
            .unwrap_or(authority)
            .split(':')
            .next()
            .unwrap_or(authority);
        let host = host.to_ascii_lowercase();
        if host == "anthropic.com" || host.ends_with(".anthropic.com") {
            Self::FirstParty
        } else {
            Self::Compatible
        }
    }
}

/// The `thinking` parameter track a built request carries (ADR-0005 modes).
/// Only `Manual` creates the ADR-0009 hard-400 shape: an `enabled` budget
/// makes the server enforce a leading thinking block on tool-use turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnthropicThinkingMode {
    Disabled,
    Manual,
    Adaptive,
}

fn thinking_mode_of(
    thinking: Option<&super::reasoning_effort::AnthropicThinking>,
) -> AnthropicThinkingMode {
    match thinking
        .and_then(|t| t.thinking.get("type"))
        .and_then(serde_json::Value::as_str)
    {
        Some("adaptive") => AnthropicThinkingMode::Adaptive,
        Some(_) => AnthropicThinkingMode::Manual,
        None => AnthropicThinkingMode::Disabled,
    }
}

/// How unsigned (signatureless) reasoning blocks are treated on the way out
/// (ADR-0009 conditional rule, replacing the old unconditional drop red line
/// from ADR-0003).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnsignedReplay {
    /// Compatible upstream: replay the block verbatim without a signature
    /// field.
    Preserve,
    /// First-party upstream on a branch the server does not enforce: drop the
    /// block with a loud warn.
    DropWarn,
}

/// Thinking-family block emission for `build_messages_messages`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThinkingEmission {
    /// Normal build: signed thinking and redacted_thinking replay verbatim;
    /// unsigned blocks follow the [`UnsignedReplay`] dispatch.
    Standard { unsigned: UnsignedReplay },
    /// Guard degradation (G1 pre-flight, G3 recovery): the request carries no
    /// thinking parameter, so every thinking-family block (signed, redacted,
    /// unsigned) is stripped for a coherent no-thinking request.
    Degrade,
}

impl ModelClientSession {
    /// Streams a turn via the Anthropic Messages API.
    ///
    /// Fork addition: goose-blueprint in-process transport so anthropic-native
    /// upstreams work without an external translation layer.
    pub(crate) async fn stream_anthropic_messages(
        &self,
        prompt: &Prompt,
        effort: Option<ReasoningEffortConfig>,
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
        // Guard G3 (ADR-0009): one bounded adaptive retry when the upstream
        // 400s on the thinking replay itself (thinking_400_recovery_class).
        let mut thinking_degraded = false;
        loop {
            let client_setup = self
                .client
                .current_client_setup(ClientRouting::Workspace)
                .await?;
            let transport = client_setup.build_api_transport(ANTHROPIC_MESSAGES_ENDPOINT)?;
            let request_auth_context = AuthRequestTelemetryContext::new(
                client_setup.auth.as_ref().map(CodexAuth::auth_mode),
                client_setup.api_auth.as_ref(),
                client_setup.agent_identity_telemetry.clone(),
                pending_retry,
            )
            .map_err(|error| self.client.state.provider.map_api_error(error))?;
            let (request_telemetry, sse_telemetry) = Self::build_streaming_telemetry(
                session_telemetry,
                request_auth_context,
                RequestRouteTelemetry::for_endpoint(ANTHROPIC_MESSAGES_ENDPOINT),
                self.client.state.auth_env_telemetry.clone(),
            );
            let executor = crate::client::ResponsesHttpExecutor::new(&self.client);
            let compression = executor.responses_request_compression(client_setup.auth.as_ref());
            let responses_options = executor
                .build_responses_options(
                    &self.turn_state,
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

            let request = self.build_messages_request_degraded(
                prompt,
                model_info,
                effort.clone(),
                thinking_degraded,
            )?;
            let client =
                ApiMessagesClient::new(transport, client_setup.api_provider, client_setup.api_auth)
                    .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
            let inference_trace_attempt = inference_trace.start_attempt();
            inference_trace_attempt.add_request_headers(&mut options.extra_headers);
            inference_trace_attempt.record_started(&request);

            let interceptors = crate::model_request::prepare(
                &self.client.request_contributors,
                &self.client.state.thread_id.to_string(),
                &model_info.slug,
                codex_extension_api::ModelRequestKind::Generation,
                &mut None,
            );
            match client.stream_request(request, options).await {
                Ok(stream) => {
                    let stream = map_response_stream(
                        stream,
                        session_telemetry.clone(),
                        inference_trace_attempt,
                        Arc::clone(&self.client.state.provider),
                        interceptors,
                        /*last_response_tx*/ None,
                        Some(reconcile_preempted),
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
                    if !thinking_degraded
                        && let Some(class) =
                            thinking_400_text(&err).and_then(thinking_400_recovery_class)
                    {
                        let response_debug_context =
                            extract_response_debug_context_from_api_error(&err);
                        inference_trace_attempt.record_failed(
                            &err,
                            response_debug_context.request_id.as_deref(),
                            /*output_items*/ &[],
                        );
                        warn!(
                            "anthropic G3 (ADR-0009): upstream rejected the thinking replay ({class}); retrying once with thinking degraded for this request",
                        );
                        thinking_degraded = true;
                        continue;
                    }
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
    #[allow(dead_code)] // test-only facade; production emits via build_messages_request_degraded (G3 flag, ADR-0009)
    pub(crate) fn build_messages_request(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        effort: Option<ReasoningEffortConfig>,
    ) -> Result<serde_json::Value> {
        self.build_messages_request_degraded(
            prompt, model_info, effort, /*thinking_degraded*/ false,
        )
    }

    /// Same builder with guard G3's degraded shape forced (ADR-0009): the
    /// request is produced without a `thinking` parameter and without any
    /// thinking-family blocks, used for the one bounded retry after a
    /// [`thinking_400_recovery_class`] rejection.
    pub(crate) fn build_messages_request_degraded(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        effort: Option<ReasoningEffortConfig>,
        thinking_degraded: bool,
    ) -> Result<serde_json::Value> {
        let instructions = &prompt.base_instructions.text;
        let input = prompt.get_formatted_input_for_request(model_info);
        let tools = create_tools_json_for_anthropic(&prompt.tools)?;

        let provider = self.client.state.provider.info();

        // Reasoning effort translation (ticket 11 / ADR-0005): a session
        // effort drives the thinking parameter on the track the provider
        // selected (adaptive for Claude 4.6+ deployments, manual bucketed
        // budget otherwise). The legacy provider-level budget stays the
        // non-effort path so plain budget configs behave exactly as before.
        // Ticket 29 / A-007: explicit provider config beats model-catalog
        // metadata beats the built-in fallback, and the winning value is
        // capped so the budget stays bounded. Platform deployments
        // (Bedrock/Vertex-style) set `anthropic_max_tokens` explicitly,
        // which is the documented guard for joint input+output budget
        // reservation (D-006 step 2). The fallback must be visible.
        let resolved_max_tokens =
            super::max_tokens::resolve_max_tokens(model_info, provider.anthropic_max_tokens);
        let max_tokens = resolved_max_tokens.value;
        if resolved_max_tokens.fallback_used {
            warn!(
                "anthropic max_tokens: model {} has no max_output_tokens metadata and no explicit anthropic_max_tokens config; falling back to the built-in {DEFAULT_ANTHROPIC_MAX_TOKENS} budget",
                model_info.slug,
            );
        }
        let mut thinking = match effort.as_ref() {
            Some(effort) => {
                if provider.anthropic_thinking_budget.is_some() {
                    debug!(
                        "session reasoning_effort takes precedence over provider.anthropic_thinking_budget",
                    );
                }
                super::reasoning_effort::anthropic_thinking_from_effort(
                    effort,
                    provider.anthropic_adaptive_thinking,
                    max_tokens,
                )
            }
            None => provider.anthropic_thinking_budget.and_then(|budget| {
                let clamped = super::reasoning_effort::clamp_thinking_budget(budget, max_tokens);
                let Some(clamped) = clamped else {
                    debug!(
                        "anthropic thinking budget {budget} cannot fit under max_tokens {max_tokens}; omitting thinking",
                    );
                    return None;
                };
                if clamped != budget {
                    debug!("anthropic thinking budget {budget} out of range; clamped to {clamped}");
                }
                Some(super::reasoning_effort::AnthropicThinking {
                    thinking: json!({ "type": "enabled", "budget_tokens": clamped }),
                    output_config: None,
                })
            }),
        };

        // ADR-0009: the upstream class and the thinking track together
        // dispatch how signatureless thinking blocks replay; guard
        // degradation strips every thinking-family block.
        let upstream = AnthropicUpstreamKind::from_base_url(provider.base_url.as_deref());
        let thinking_mode = thinking_mode_of(thinking.as_ref());
        let emission = if thinking_degraded {
            ThinkingEmission::Degrade
        } else {
            ThinkingEmission::Standard {
                unsigned: match upstream {
                    AnthropicUpstreamKind::Compatible => UnsignedReplay::Preserve,
                    AnthropicUpstreamKind::FirstParty => UnsignedReplay::DropWarn,
                },
            }
        };
        let mut messages = build_messages_messages(input.clone(), emission);

        // Guard G1 (ADR-0009): on a first-party upstream, manual thinking
        // with a trailing tool_use turn that does not lead with a thinking
        // block is a guaranteed 400 (an unsigned block was just dropped
        // above). Degrade the round loudly instead of sending the doomed
        // shape bare.
        if !thinking_degraded
            && upstream == AnthropicUpstreamKind::FirstParty
            && thinking_mode == AnthropicThinkingMode::Manual
            && manual_tool_use_leads_without_thinking(&messages)
        {
            warn!(
                "anthropic G1 (ADR-0009): thinking=enabled with a trailing tool_use turn lacking a leading thinking block would be hard-400ed; dropping the thinking parameter and all thinking blocks for this request",
            );
            thinking = None;
            messages = build_messages_messages(input, ThinkingEmission::Degrade);
        }
        if thinking_degraded {
            thinking = None;
        }

        let mut request = json!({
            "model": model_info.slug.clone(),
            "messages": messages,
            "max_tokens": max_tokens,
            "stream": true,
        });

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

            if let Some(thinking) = thinking {
                obj.insert("thinking".to_string(), thinking.thinking);
                if let Some(output_config) = thinking.output_config {
                    obj.insert("output_config".to_string(), output_config);
                }
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
///
/// Thinking-family blocks are emitted per `emission` (ADR-0009): signed
/// thinking and redacted_thinking replay verbatim on a standard build,
/// unsigned blocks dispatch on the upstream class (preserve on compatible
/// endpoints, drop with a loud warn on unenforced first-party branches), and
/// a degraded build (guards G1/G3) strips every thinking-family block.
pub(crate) fn build_messages_messages(
    input: Vec<ResponseItem>,
    emission: ThinkingEmission,
) -> Vec<serde_json::Value> {
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
                    push_anthropic_user_blocks(&mut messages, blocks);
                }
            }
            // Defect ① (ticket 27 / A-002): a redacted_thinking block
            // round-trips through the IR as `content: None,
            // encrypted_content: Some("data\0sig")` (fork-internal
            // delimiter; the wire contract is unchanged: outbound
            // splits to emit the native {type:redacted_thinking,
            // data, signature} block). The `content: None` shape is
            // the structural discriminator vs. signed thinking (which
            // always carries `content: Some([ReasoningText{..}])` from
            // the SSE), and survives `should_serialize_reasoning_content`
            // (None is never skipped) and `event_mapping` (None surfaces
            // as empty raw_content, so the opaque bytes never leak into
            // the UI). This arm must come before the general signed-
            // thinking arm below.
            ResponseItem::Reasoning {
                content: None,
                encrypted_content: Some(combined),
                ..
            } if combined.contains('\0') => {
                if matches!(emission, ThinkingEmission::Degrade) {
                    continue;
                }
                let (data, signature) = match combined.split_once('\0') {
                    Some((d, s)) => (d.to_string(), s.to_string()),
                    None => (combined.clone(), String::new()),
                };
                pending_assistant_blocks.push(json!({
                    "type": "redacted_thinking",
                    "data": data,
                    "signature": signature,
                }));
            }
            ResponseItem::Reasoning {
                content,
                encrypted_content: Some(signature),
                ..
            } => {
                if matches!(emission, ThinkingEmission::Degrade) {
                    continue;
                }
                // Thinking replay: Anthropic requires signed thinking blocks
                // be passed back verbatim (text + signature) inside tool-use
                // rounds; tampering with the signature 400s the whole turn.
                // Signatureless blocks never reach this arm: their handling
                // moved from the old unconditional-drop red line (ADR-0003)
                // to the ADR-0009 conditional dispatch in the unsigned arm
                // below, escalated to full degradation by guard G1 in
                // `build_messages_request_degraded`.
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
            ResponseItem::Reasoning {
                content,
                encrypted_content: None,
                ..
            } => {
                // ADR-0009 conditional rule for signatureless thinking:
                // compatible endpoints never sign blocks, so the block must
                // go back verbatim or the next tool round 400s; first-party
                // upstreams may omit it on unenforced branches (drop + loud
                // warn), and the manual+tool_use shape is escalated to full
                // degradation by guard G1 before the request is sent.
                let ThinkingEmission::Standard { unsigned } = emission else {
                    continue;
                };
                let thinking: String = content
                    .unwrap_or_default()
                    .iter()
                    .map(|fragment| match fragment {
                        ReasoningItemContent::ReasoningText { text }
                        | ReasoningItemContent::Text { text } => text.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("");
                match unsigned {
                    UnsignedReplay::Preserve => {
                        if thinking.is_empty() {
                            debug!("anthropic replay: skipping empty unsigned thinking block");
                            continue;
                        }
                        pending_assistant_blocks.push(json!({
                            "type": "thinking",
                            "thinking": thinking,
                        }));
                    }
                    UnsignedReplay::DropWarn => {
                        warn!(
                            "anthropic ADR-0009: dropped unsigned thinking block on a first-party upstream (unenforced branch per the conditional replay rule)",
                        );
                    }
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
                // `success: Some(false)` is the IR's only failure signal — `None`
                // and `Some(true)` both mean success, the way the dispatcher reads it.
                push_anthropic_tool_result(
                    &mut messages,
                    &call_id.unwrap_or_default(),
                    text,
                    output.success == Some(false),
                );
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
                push_anthropic_tool_result(
                    &mut messages,
                    &call_id,
                    text,
                    output.success == Some(false),
                );
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

fn push_anthropic_tool_result(
    messages: &mut Vec<serde_json::Value>,
    call_id: &str,
    text: String,
    is_error: bool,
) {
    let mut block = json!({
        "type": "tool_result",
        "tool_use_id": call_id,
        "content": text,
    });
    // The field is absent on success because the Messages API already defaults
    // it to false; sending it only for failures keeps the payload minimal.
    if is_error {
        block["is_error"] = json!(true);
    }
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

/// Coalesces consecutive non-assistant items into the user message already at
/// the end of the list, the same alternation duty [`push_anthropic_tool_result`]
/// serves: the Messages API rejects two consecutive user messages, and a
/// reconciled preemption legitimately leaves a tool result, the interrupt marker
/// and the new user message adjacent (spec S-04).
fn push_anthropic_user_blocks(
    messages: &mut Vec<serde_json::Value>,
    blocks: Vec<serde_json::Value>,
) {
    if let Some(last) = messages.last_mut()
        && last["role"] == "user"
        && let Some(content) = last["content"].as_array_mut()
    {
        content.extend(blocks);
        return;
    }
    messages.push(json!({
        "role": "user",
        "content": blocks,
    }));
}

/// Converts Responses `input_image` content items to Anthropic `image` blocks.
///
/// Only base64 data-URIs can be carried as images: the Messages API has no
/// URL-fetch variant, and a hosted `file_id` is minted by one provider and is
/// meaningless to another (file ids never cross vendors). Both forms replay as
/// a visible text placeholder so a turn never loses its only trace of the image.
fn content_items_to_image_blocks(
    content: &[codex_protocol::models::ContentItem],
) -> Vec<serde_json::Value> {
    let mut blocks = Vec::new();
    for item in content {
        let codex_protocol::models::ContentItem::InputImage { image, .. } = item else {
            continue;
        };
        let image_url = match image {
            codex_protocol::models::ImageReference::Inline { image_url } => image_url,
            codex_protocol::models::ImageReference::File { file_id } => {
                warn!(
                    file_id = %file_id,
                    "anthropic replay: hosted file-id image cannot cross providers; kept as a text placeholder"
                );
                blocks.push(json!({
                    "type": "text",
                    "text": hosted_file_image_placeholder(file_id),
                }));
                continue;
            }
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

/// Text stand-in for an image the Messages wire cannot carry.
fn hosted_file_image_placeholder(file_id: &str) -> String {
    format!("[image omitted: hosted file `{file_id}` is not available to this provider]")
}

/// Guard G1 pre-flight (ADR-0009): a manual-thinking request whose final
/// assistant message replays a `tool_use` block must lead that message with
/// a `thinking`/`redacted_thinking` block, or the first-party API hard-400s
/// the whole turn (D-009 finding 2: thinking config present + last assistant
/// has tool_use + first block not thinking). Returns true for the shape that
/// must be degraded before send, never sent bare.
fn manual_tool_use_leads_without_thinking(messages: &[serde_json::Value]) -> bool {
    let Some(assistant) = messages.iter().rev().find(|message| {
        message.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
    }) else {
        return false;
    };
    let Some(blocks) = assistant
        .get("content")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    let leads_with_thinking = blocks.first().is_some_and(|block| {
        matches!(
            block.get("type").and_then(serde_json::Value::as_str),
            Some("thinking") | Some("redacted_thinking")
        )
    });
    let has_tool_use = blocks
        .iter()
        .any(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use"));
    has_tool_use && !leads_with_thinking
}

/// Guard G3 (ADR-0009): classify the three 400 response texts that mean the
/// thinking replay itself was rejected (D-009 finding 3). Recovery is the one
/// bounded degraded retry wired into `stream_anthropic_messages`.
pub(crate) fn thinking_400_recovery_class(message: &str) -> Option<&'static str> {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("expected `thinking` or `redacted_thinking`")
        || lowered.contains("expected thinking or redacted_thinking")
    {
        Some("expected-thinking-first")
    } else if lowered.contains("cannot be modified") {
        Some("thinking-immutable")
    } else if lowered.contains("invalid signature") {
        Some("invalid-signature")
    } else {
        None
    }
}

/// Pull the 400 payload text out of an API error for
/// [`thinking_400_recovery_class`]: both the structured [`ApiError::Api`]
/// shape and the raw transport-400 body qualify.
fn thinking_400_text(err: &ApiError) -> Option<&str> {
    match err {
        ApiError::Api { status, message } if status.as_u16() == 400 => Some(message),
        ApiError::Transport(TransportError::Http { status, body, .. })
            if status.as_u16() == 400 =>
        {
            body.as_deref()
        }
        _ => None,
    }
}

/// Reconciles the blocks the Messages stream still held in flight when the user
/// preempted it into the set that may enter history (spec S-03, S-04, S-06,
/// S-08, S-10).
///
/// Same pairing rule as the chat wire — a tool_use whose `input_json_delta`
/// arrived whole is kept and paired with the synthetic "never ran" result, a
/// truncated one is dropped whole, received text is kept — plus the thinking
/// family rule this wire owns: an unsigned block was cut off inside the block,
/// so it goes, and a signature is never fabricated to save it (wire red lines 3
/// and 4, ADR-0009 G1).
pub(crate) fn reconcile_preempted(pending: Vec<ResponseItem>) -> Vec<ResponseItem> {
    let mut retained = Vec::new();
    for item in pending {
        match &item {
            ResponseItem::FunctionCall {
                call_id, arguments, ..
            } => {
                if serde_json::from_str::<serde_json::Value>(arguments).is_ok() {
                    let result = preempted_tool_call_result(call_id);
                    retained.push(item);
                    retained.push(result);
                    continue;
                }
                debug!("messages preemption: dropping truncated tool_use {call_id}");
            }
            ResponseItem::Message { role, content, .. }
                if role.as_str() == "assistant" && content_items_to_text(content).is_some() =>
            {
                retained.push(item);
            }
            ResponseItem::Reasoning {
                content,
                encrypted_content,
                ..
            } => {
                if thinking_block_reached_completion(content.as_ref(), encrypted_content.as_deref())
                {
                    retained.push(item);
                } else {
                    debug!("messages preemption: dropping unfinished thinking block");
                }
            }
            _ => {}
        }
    }
    retained
}

/// Decides whether a block that never reached `content_block_stop` is still
/// faithful enough to replay.
///
/// `signature_delta` closes a thinking block's text, so a signed pending block
/// is text-complete and replays verbatim; an unsigned one was cut off mid-block
/// and has no signature to send back. A `redacted_thinking` block round-trips
/// only when both its opaque payload and its signature arrived, because the
/// outbound half rebuilds the native block from the fork-internal
/// `data\0signature` encoding and a half-delivered pair is not the block the
/// server sent.
fn thinking_block_reached_completion(
    content: Option<&Vec<ReasoningItemContent>>,
    encrypted_content: Option<&str>,
) -> bool {
    match (content, encrypted_content) {
        (Some(_), Some(signature)) => !signature.is_empty(),
        (None, Some(combined)) => combined
            .split_once('\0')
            .is_some_and(|(data, signature)| !data.is_empty() && !signature.is_empty()),
        (Some(_), None) | (None, None) => false,
    }
}

#[cfg(test)]
#[path = "anthropic_tests.rs"]
mod tests;
