//! Shared reasoning-effort ladders.
//!
//! Keep these in provider-core so provider runtimes and UI clients expose the
//! same ordered values. `swarm` and `swarm-deep` are Jcode UI sentinels rather
//! than wire-level provider values, but they belong in the selectable ladder.

/// OpenAI Responses API effort levels, followed by Jcode's swarm modes.
pub const OPENAI_SELECTABLE_EFFORTS: &[&str] = &[
    "none",
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
    "swarm",
    "swarm-deep",
];

/// OpenRouter's unified reasoning effort levels.
///
/// OpenRouter currently treats `max` as an alias for `xhigh`, so it is not a
/// separate rung in this ladder.
pub const OPENROUTER_SELECTABLE_EFFORTS: &[&str] = &[
    "none",
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "swarm",
    "swarm-deep",
];

/// Portable effort levels for direct OpenAI-compatible GPT endpoints.
///
/// Azure OpenAI Chat Completions rejects both `minimal` and `max`, while native
/// OpenAI, compatible Responses, and OpenRouter expose broader vocabularies.
/// Keep the compatible Chat picker to the verified intersection. A runtime may
/// still pass through an explicitly configured provider-specific value.
pub const OPENAI_COMPATIBLE_SELECTABLE_EFFORTS: &[&str] = &[
    "none",
    "low",
    "medium",
    "high",
    "xhigh",
    "swarm",
    "swarm-deep",
];

/// Portable effort levels for OpenAI-compatible Responses API endpoints.
///
/// Azure Responses accepts literal `max` for current GPT-5 reasoning models but
/// rejects `minimal`, so this differs from both native OpenAI and compatible
/// Chat Completions. Explicit provider-specific values can still be requested.
pub const OPENAI_COMPATIBLE_RESPONSES_SELECTABLE_EFFORTS: &[&str] = &[
    "none",
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
    "swarm",
    "swarm-deep",
];

/// Direct DeepSeek effort levels, followed by Jcode's swarm modes.
pub const DEEPSEEK_SELECTABLE_EFFORTS: &[&str] = &[
    "none",
    "low",
    "medium",
    "high",
    "max",
    "swarm",
    "swarm-deep",
];

/// Convert a provider-advertised OpenAI/OpenRouter effort into the canonical
/// static value used by the provider trait.
pub fn canonical_reasoning_effort(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some("none"),
        "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

/// Infer the selectable effort ladder when only provider/model identity is
/// available, such as in a remote TUI session.
pub fn inferred_reasoning_efforts(
    provider_name: Option<&str>,
    model_name: Option<&str>,
) -> Vec<&'static str> {
    let provider = provider_name.unwrap_or_default().to_ascii_lowercase();
    let model = model_name.unwrap_or_default().to_ascii_lowercase();
    let is_openai_model = model.starts_with("gpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.starts_with("o5");

    if provider.contains("reasoning effort disabled") {
        return Vec::new();
    }

    if provider.contains("responses") && is_openai_model {
        return OPENAI_COMPATIBLE_RESPONSES_SELECTABLE_EFFORTS.to_vec();
    }

    if provider.contains("openrouter") {
        return OPENROUTER_SELECTABLE_EFFORTS.to_vec();
    }

    if provider.contains("deepseek") || model.contains("deepseek") {
        return DEEPSEEK_SELECTABLE_EFFORTS.to_vec();
    }

    if provider.contains("openai-compatible") {
        return if is_openai_model {
            // Azure and other compatible GPT gateways commonly expose the
            // portable none|low|medium|high|xhigh vocabulary. Keep broader
            // native values available only through explicit configuration.
            OPENAI_COMPATIBLE_SELECTABLE_EFFORTS.to_vec()
        } else {
            Vec::new()
        };
    }

    let is_anthropic = provider.contains("anthropic")
        || provider.contains("claude")
        || model.starts_with("claude-");
    if is_anthropic {
        let caps = crate::anthropic_reasoning_caps(&model);
        if !caps.supports_reasoning_effort() {
            return Vec::new();
        }
        let mut efforts = vec!["none", "low", "medium", "high"];
        if caps.xhigh_effort {
            efforts.push("xhigh");
        }
        if caps.max_effort {
            efforts.push("max");
        }
        efforts.extend(["swarm", "swarm-deep"]);
        return efforts;
    }

    let is_openai = provider.contains("openai") || provider.contains("codex") || is_openai_model;
    if is_openai {
        return OPENAI_SELECTABLE_EFFORTS.to_vec();
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ladders_preserve_native_max_and_portable_compatible_semantics() {
        assert_eq!(
            inferred_reasoning_efforts(Some("openai"), Some("gpt-5.4")),
            OPENAI_SELECTABLE_EFFORTS
        );
        assert!(OPENAI_SELECTABLE_EFFORTS.contains(&"max"));
        assert!(OPENAI_SELECTABLE_EFFORTS.contains(&"minimal"));
        assert!(OPENROUTER_SELECTABLE_EFFORTS.contains(&"minimal"));
        assert!(!OPENROUTER_SELECTABLE_EFFORTS.contains(&"max"));
        assert!(!OPENAI_COMPATIBLE_SELECTABLE_EFFORTS.contains(&"minimal"));
        assert!(!OPENAI_COMPATIBLE_SELECTABLE_EFFORTS.contains(&"max"));
        assert!(DEEPSEEK_SELECTABLE_EFFORTS.contains(&"max"));
        assert_eq!(
            inferred_reasoning_efforts(Some("openai-compatible:custom"), Some("gpt-5.6")),
            OPENAI_COMPATIBLE_SELECTABLE_EFFORTS,
            "compatible runtimes should advertise only the portable vocabulary"
        );
        assert_eq!(
            inferred_reasoning_efforts(
                Some("openai-compatible:custom responses api"),
                Some("gpt-5.6")
            ),
            OPENAI_COMPATIBLE_RESPONSES_SELECTABLE_EFFORTS,
            "Responses-compatible runtimes should advertise the portable Responses efforts"
        );
        assert!(!OPENAI_COMPATIBLE_RESPONSES_SELECTABLE_EFFORTS.contains(&"minimal"));
        assert!(OPENAI_COMPATIBLE_RESPONSES_SELECTABLE_EFFORTS.contains(&"max"));
        assert!(
            inferred_reasoning_efforts(
                Some("azure-credit-chat (Chat Completions API; reasoning effort disabled)"),
                Some("gpt-5.6-sol")
            )
            .is_empty(),
            "an explicitly effort-disabled profile must not expose remote effort controls"
        );
    }

    #[test]
    fn anthropic_ladder_comes_from_model_capabilities() {
        assert_eq!(
            inferred_reasoning_efforts(Some("anthropic"), Some("claude-sonnet-4-6")),
            vec![
                "none",
                "low",
                "medium",
                "high",
                "max",
                "swarm",
                "swarm-deep"
            ]
        );
        assert_eq!(
            inferred_reasoning_efforts(Some("anthropic"), Some("claude-opus-4-7")),
            vec![
                "none",
                "low",
                "medium",
                "high",
                "xhigh",
                "max",
                "swarm",
                "swarm-deep"
            ]
        );
    }
}
