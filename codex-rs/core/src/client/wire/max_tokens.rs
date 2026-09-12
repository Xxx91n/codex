//! Anthropic `max_tokens` budget resolution (fork addition, ticket 29 / A-007).
//!
//! Priority chain per D-006: explicit provider config beats model catalog
//! metadata beats the built-in 8192 fallback. The result is the minimum of
//! the resolved source and the managed cap so the budget stays bounded even
//! when metadata or config advertise huge values. Resolution is pure and
//! deterministic: the same inputs always produce the same budget, which
//! keeps prompt-cache keys stable (effort/budget values are part of the
//! Anthropic cache key).

use codex_protocol::openai_models::ModelInfo;

use crate::client::DEFAULT_ANTHROPIC_MAX_TOKENS;

/// Managed ceiling applied on top of any resolved source so a metadata or
/// config entry cannot unbound the output budget (D-006 step 1 "管理性 cap").
pub(crate) const ANTHROPIC_MAX_TOKENS_MANAGED_CAP: u32 = 128_000;

/// The resolved `max_tokens` budget plus whether the built-in fallback was
/// used. Callers surface `fallback_used` as a warning: an invisible default
/// is how the 8k wall stayed unnoticed (D-006 step 2 "兜底必须可见").
pub(crate) struct ResolvedMaxTokens {
    pub(crate) value: u32,
    pub(crate) fallback_used: bool,
}

/// Resolves the Anthropic Messages wire `max_tokens` budget.
///
/// Priority: explicit `anthropic_max_tokens` provider config, then the
/// model-catalog `max_output_tokens` metadata (the `context_window`
/// mechanism), then the built-in 8192 fallback. Every source is clamped to
/// the managed cap. `explicit_is_platform` reports whether the winning
/// source is provider-level config — Bedrock/Vertex-style deployments set
/// it explicitly, which is the documented platform guard for joint
/// input+output budget reservation (D-006 step 2).
pub(crate) fn resolve_max_tokens(
    model_info: &ModelInfo,
    explicit: Option<u32>,
) -> ResolvedMaxTokens {
    let (value, fallback_used) = match explicit {
        // Explicit provider config wins and carries the platform-guard duty:
        // Bedrock/Vertex operators reserve joint budget through it, so it is
        // trusted (still capped) rather than intersected with metadata.
        Some(explicit) => (explicit.min(ANTHROPIC_MAX_TOKENS_MANAGED_CAP), false),
        None => match model_info.max_output_tokens {
            Some(metadata) => (
                u32::try_from(metadata.clamp(0, i64::from(ANTHROPIC_MAX_TOKENS_MANAGED_CAP)))
                    .unwrap_or(DEFAULT_ANTHROPIC_MAX_TOKENS),
                false,
            ),
            // Unknown or alias model: the catalog has no ceiling; the built-in
            // 8192 fallback applies and must be visible to the operator.
            None => (DEFAULT_ANTHROPIC_MAX_TOKENS, true),
        },
    };
    ResolvedMaxTokens {
        value,
        fallback_used,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::config_types::ReasoningSummary;
    use codex_protocol::openai_models::ConfigShellToolType;
    use codex_protocol::openai_models::InputModality;
    use codex_protocol::openai_models::ModelVisibility;
    use codex_protocol::openai_models::TruncationPolicyConfig;
    use codex_protocol::openai_models::WebSearchToolType;

    fn model_info_with_max_output(max_output_tokens: Option<i64>) -> ModelInfo {
        ModelInfo {
            slug: "claude-test".to_string(),
            display_name: "claude-test".to_string(),
            description: None,
            default_reasoning_level: None,
            supported_reasoning_levels: Vec::new(),
            shell_type: ConfigShellToolType::UnifiedExec,
            visibility: ModelVisibility::None,
            supported_in_api: true,
            priority: 0,
            additional_speed_tiers: Vec::new(),
            service_tiers: Vec::new(),
            default_service_tier: None,
            availability_nux: None,
            upgrade: None,
            model_messages: None,
            include_skills_usage_instructions: false,
            include_plugin_usage_instructions: false,
            include_apps_usage_instructions: true,
            supports_reasoning_summary_parameter: true,
            default_reasoning_summary: ReasoningSummary::Auto,
            support_verbosity: false,
            default_verbosity: None,
            apply_patch_tool_type: None,
            web_search_tool_type: WebSearchToolType::Text,
            truncation_policy: TruncationPolicyConfig::bytes(/*limit*/ 10_000),
            supports_image_detail_original: false,
            context_window: None,
            max_context_window: None,
            auto_compact_token_limit: None,
            comp_hash: None,
            effective_context_window_percent: 95,
            experimental_supported_tools: Vec::new(),
            input_modalities: vec![InputModality::Text],
            used_fallback_model_metadata: false,
            supports_search_tool: false,
            use_responses_lite: false,
            node_repl_auto_review_required: false,
            node_repl_disabled: false,
            auto_review_model_override: None,
            model_specialty: None,
            tool_mode: None,
            multi_agent_version: None,
            multi_agent_reasoning_effort: None,
            available_access_programs: None,
            guardian: None,
            supports_experimental_context: false,
            max_output_tokens,
        }
    }

    #[test]
    fn explicit_config_wins_over_metadata() {
        let model = model_info_with_max_output(Some(64_000));
        let resolved = resolve_max_tokens(&model, Some(200_000));
        assert_eq!(resolved.value, ANTHROPIC_MAX_TOKENS_MANAGED_CAP);
        assert!(!resolved.fallback_used);

        let resolved = resolve_max_tokens(&model, Some(32_000));
        assert_eq!(resolved.value, 32_000);
        assert!(!resolved.fallback_used);
    }

    #[test]
    fn metadata_is_used_when_explicit_config_absent() {
        let model = model_info_with_max_output(Some(64_000));
        let resolved = resolve_max_tokens(&model, None);
        assert_eq!(resolved.value, 64_000);
        assert!(!resolved.fallback_used);

        // Metadata above the managed cap is clamped.
        let model = model_info_with_max_output(Some(500_000));
        let resolved = resolve_max_tokens(&model, None);
        assert_eq!(resolved.value, ANTHROPIC_MAX_TOKENS_MANAGED_CAP);
    }

    #[test]
    fn unknown_model_falls_back_with_visibility() {
        let model = model_info_with_max_output(None);
        let resolved = resolve_max_tokens(&model, None);
        assert_eq!(resolved.value, DEFAULT_ANTHROPIC_MAX_TOKENS);
        assert!(resolved.fallback_used);
    }

    #[test]
    fn explicit_config_absent_metadata_negative_never_panics() {
        // A corrupt catalog entry must not crash the wire; it degrades to the
        // built-in fallback via the try_from guard.
        let model = model_info_with_max_output(Some(-1));
        let resolved = resolve_max_tokens(&model, None);
        assert_eq!(resolved.value, DEFAULT_ANTHROPIC_MAX_TOKENS);
        assert!(resolved.fallback_used);
    }
}
