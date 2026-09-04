//! Reasoning-effort translation (fork addition): pure mapping from the
//! session-level reasoning effort to per-wire request parameters.
//!
//! One user effort must behave consistently across the Chat and Anthropic
//! wires (ticket 11 / ADR-0005). The functions here are deterministic and
//! free of IO: the same effort always produces the same JSON, which keeps
//! prompt-cache breakpoints stable — effort/budget values are part of the
//! cache key on Anthropic- and Kimi-family upstreams.

use codex_protocol::openai_models::ReasoningEffort;
use serde_json::Value;
use serde_json::json;

/// Manual-track bucket for the lowest efforts. 1024 is the Anthropic API
/// floor: smaller `budget_tokens` values are rejected outright.
pub(crate) const THINKING_BUDGET_LOW: u32 = 1024;
/// Manual-track bucket for the middle effort (LiteLLM-compatible constant).
pub(crate) const THINKING_BUDGET_MEDIUM: u32 = 2048;
/// Manual-track bucket for the highest efforts (LiteLLM-compatible constant).
pub(crate) const THINKING_BUDGET_HIGH: u32 = 4096;

/// Chat wire `reasoning_effort` value for a session effort, or `None` when
/// the effort has no portable expression on the OpenAI-compatible surface.
///
/// Only the universally-accepted vocabulary is emitted verbatim. Tiers that
/// specific model families gate (`minimal`, `xhigh`, `max`) fall back to the
/// nearest universal tier (OpenRouter's downgrade rule); disable-style and
/// model-defined values have no portable spelling, so the field is omitted
/// and the model default applies (documented lossy edge in ADR-0005).
pub(crate) fn chat_reasoning_effort(effort: &ReasoningEffort) -> Option<&'static str> {
    match effort {
        ReasoningEffort::Low => Some("low"),
        ReasoningEffort::Medium => Some("medium"),
        ReasoningEffort::High => Some("high"),
        ReasoningEffort::Minimal => Some("low"),
        ReasoningEffort::XHigh | ReasoningEffort::Max | ReasoningEffort::Ultra => Some("high"),
        ReasoningEffort::None | ReasoningEffort::Persistent | ReasoningEffort::Custom(_) => None,
    }
}

/// Manual-track `budget_tokens` bucket for a session effort, or `None` when
/// the effort has no manual-track meaning (disable-style / unknown values).
///
/// Buckets follow LiteLLM's `reasoning_effort_from_thinking_budget`
/// constants so gatewayed and direct requests behave comparably and the
/// mapping round-trips through that reverse translation.
pub(crate) fn budget_tokens_for_effort(effort: &ReasoningEffort) -> Option<u32> {
    match effort {
        ReasoningEffort::Minimal | ReasoningEffort::Low => Some(THINKING_BUDGET_LOW),
        ReasoningEffort::Medium => Some(THINKING_BUDGET_MEDIUM),
        ReasoningEffort::High
        | ReasoningEffort::XHigh
        | ReasoningEffort::Max
        | ReasoningEffort::Ultra => Some(THINKING_BUDGET_HIGH),
        ReasoningEffort::None | ReasoningEffort::Persistent | ReasoningEffort::Custom(_) => None,
    }
}

/// Adaptive-track `output_config.effort` value for a session effort, or
/// `None` when the effort has no adaptive expression (the model default
/// applies — documented lossy edge for disable-style values).
pub(crate) fn adaptive_effort_value(effort: &ReasoningEffort) -> Option<&'static str> {
    match effort {
        ReasoningEffort::Minimal | ReasoningEffort::Low => Some("low"),
        ReasoningEffort::Medium => Some("medium"),
        ReasoningEffort::High => Some("high"),
        // Per-model tiers degrade to the nearest universally-supported tier
        // until a capability table can steer them (ADR-0005 open question).
        ReasoningEffort::XHigh | ReasoningEffort::Max | ReasoningEffort::Ultra => Some("high"),
        ReasoningEffort::None | ReasoningEffort::Persistent | ReasoningEffort::Custom(_) => None,
    }
}

/// Clamps a thinking budget into the Anthropic-accepted range
/// `[1024, max_tokens - 1]`. Returns `None` when no valid budget exists
/// (the floor cannot fit under `max_tokens`); callers then omit the
/// `thinking` parameter instead of sending a value the API rejects.
pub(crate) fn clamp_thinking_budget(budget: u32, max_tokens: u32) -> Option<u32> {
    let upper = max_tokens.checked_sub(1)?;
    if upper < THINKING_BUDGET_LOW {
        return None;
    }
    Some(budget.clamp(THINKING_BUDGET_LOW, upper))
}

/// The Anthropic `thinking` parameter plus the optional `output_config`
/// companion for a turn driven by a session effort.
pub(crate) struct AnthropicThinking {
    pub(crate) thinking: Value,
    pub(crate) output_config: Option<Value>,
}

/// Translates a session effort into Anthropic wire parameters.
///
/// Adaptive mode (provider opt-in for Claude 4.6+ deployments) speaks
/// `thinking: { type: "adaptive" }` plus `output_config.effort`; manual
/// mode buckets the effort into `budget_tokens`, clamped into the
/// API-accepted range. Efforts with no expression on the selected track
/// yield `None` (no reasoning parameter; model default applies).
pub(crate) fn anthropic_thinking_from_effort(
    effort: &ReasoningEffort,
    adaptive: bool,
    max_tokens: u32,
) -> Option<AnthropicThinking> {
    if adaptive {
        let value = adaptive_effort_value(effort)?;
        return Some(AnthropicThinking {
            thinking: json!({ "type": "adaptive" }),
            output_config: Some(json!({ "effort": value })),
        });
    }
    let budget = budget_tokens_for_effort(effort)?;
    let clamped = clamp_thinking_budget(budget, max_tokens)?;
    Some(AnthropicThinking {
        thinking: json!({ "type": "enabled", "budget_tokens": clamped }),
        output_config: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::openai_models::ReasoningEffort;

    #[test]
    fn chat_reasoning_effort_uses_the_universal_vocabulary() {
        assert_eq!(chat_reasoning_effort(&ReasoningEffort::Low), Some("low"));
        assert_eq!(chat_reasoning_effort(&ReasoningEffort::Medium), Some("medium"));
        assert_eq!(chat_reasoning_effort(&ReasoningEffort::High), Some("high"));
        // Non-universal tiers degrade to the nearest universal tier.
        assert_eq!(chat_reasoning_effort(&ReasoningEffort::Minimal), Some("low"));
        assert_eq!(chat_reasoning_effort(&ReasoningEffort::XHigh), Some("high"));
        assert_eq!(chat_reasoning_effort(&ReasoningEffort::Max), Some("high"));
        // Disable-style and unknown values omit the field entirely.
        assert_eq!(chat_reasoning_effort(&ReasoningEffort::None), None);
        assert_eq!(chat_reasoning_effort(&ReasoningEffort::Persistent), None);
        assert_eq!(
            chat_reasoning_effort(&ReasoningEffort::Custom("weird".to_string())),
            None
        );
    }

    #[test]
    fn manual_buckets_follow_the_litellm_constants() {
        assert_eq!(budget_tokens_for_effort(&ReasoningEffort::Minimal), Some(1_024));
        assert_eq!(budget_tokens_for_effort(&ReasoningEffort::Low), Some(1_024));
        assert_eq!(budget_tokens_for_effort(&ReasoningEffort::Medium), Some(2_048));
        assert_eq!(budget_tokens_for_effort(&ReasoningEffort::High), Some(4_096));
        assert_eq!(budget_tokens_for_effort(&ReasoningEffort::XHigh), Some(4_096));
        assert_eq!(budget_tokens_for_effort(&ReasoningEffort::Max), Some(4_096));
        assert_eq!(budget_tokens_for_effort(&ReasoningEffort::None), None);
        assert_eq!(budget_tokens_for_effort(&ReasoningEffort::Custom("off".to_string())), None);
    }

    #[test]
    fn adaptive_effort_maps_directly_with_nearest_tier_fallback() {
        assert_eq!(adaptive_effort_value(&ReasoningEffort::Minimal), Some("low"));
        assert_eq!(adaptive_effort_value(&ReasoningEffort::Low), Some("low"));
        assert_eq!(adaptive_effort_value(&ReasoningEffort::Medium), Some("medium"));
        assert_eq!(adaptive_effort_value(&ReasoningEffort::High), Some("high"));
        assert_eq!(adaptive_effort_value(&ReasoningEffort::Max), Some("high"));
        assert_eq!(adaptive_effort_value(&ReasoningEffort::None), None);
        assert_eq!(adaptive_effort_value(&ReasoningEffort::Custom("tier9".to_string())), None);
    }

    #[test]
    fn clamp_thinking_budget_keeps_budget_under_max_tokens() {
        assert_eq!(clamp_thinking_budget(4_096, 8_192), Some(4_096));
        // Doc contract: budget_tokens must be < max_tokens.
        assert_eq!(clamp_thinking_budget(100_000, 4_096), Some(4_095));
        // Below the floor clamps up rather than sending a rejected value.
        assert_eq!(clamp_thinking_budget(512, 8_192), Some(1_024));
        // No valid budget when the floor cannot fit; never panic.
        assert_eq!(clamp_thinking_budget(1_024, 1_024), None);
        assert_eq!(clamp_thinking_budget(1_024, 0), None);
    }

    #[test]
    fn anthropic_thinking_from_effort_speaks_both_tracks() {
        let manual = anthropic_thinking_from_effort(&ReasoningEffort::High, false, 8_192).unwrap();
        assert_eq!(
            manual.thinking,
            json!({ "type": "enabled", "budget_tokens": 4_096 })
        );
        assert!(manual.output_config.is_none());

        let adaptive =
            anthropic_thinking_from_effort(&ReasoningEffort::Medium, true, 8_192).unwrap();
        assert_eq!(adaptive.thinking, json!({ "type": "adaptive" }));
        assert_eq!(adaptive.output_config, Some(json!({ "effort": "medium" })));

        // Disable-style efforts send nothing on either track.
        assert!(anthropic_thinking_from_effort(&ReasoningEffort::None, false, 8_192).is_none());
        assert!(anthropic_thinking_from_effort(&ReasoningEffort::None, true, 8_192).is_none());
    }

    #[test]
    fn effort_translation_is_deterministic() {
        let a = anthropic_thinking_from_effort(&ReasoningEffort::Medium, true, 8_192).unwrap();
        let b = anthropic_thinking_from_effort(&ReasoningEffort::Medium, true, 8_192).unwrap();
        assert_eq!(a.thinking, b.thinking);
        assert_eq!(a.output_config, b.output_config);
    }
}
