//! Parse + validate the JSON body of a `chat_completion` `ActionRequest`.

/// Hard ceiling on `timeout_ms`; matches `network`'s own cap so a
/// `chat_completion` call can't outlive the HTTP request it wraps.
pub const MAX_TIMEOUT_MS: u64 = 30_000;

/// Default `max_tokens` when the caller omits it.
pub const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Hard ceiling on `max_tokens`. Clamped, never rejected.
pub const MAX_MAX_TOKENS: u32 = 8192;

pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

/// Operator-supplied allowlist of env var names a caller's `api_key_env`
/// may name. Comma-separated, exact (case-sensitive) match. Default-deny:
/// unset or empty means no `api_key_env` value is accepted — a caller
/// could otherwise name *any* environment variable in the `ai` process
/// (an unrelated secret, not just a provider key) and have its value sent
/// straight into an outbound request header to a caller-controlled
/// `base_url`, exfiltrating it.
pub const ALLOWED_KEY_ENVS_ENV: &str = "AI_PLUGIN_ALLOWED_KEY_ENVS";

/// Parse [`ALLOWED_KEY_ENVS_ENV`]'s raw value into the set of permitted
/// `api_key_env` names.
pub fn parse_allowed_key_envs(raw: &str) -> std::collections::HashSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// True if `name` is permitted as an `api_key_env` value, per the
/// operator's [`ALLOWED_KEY_ENVS_ENV`] allowlist.
pub fn is_allowed_key_env(name: &str, allowed: &std::collections::HashSet<String>) -> bool {
    allowed.contains(name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatCompletionParams {
    pub provider: Provider,
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    pub timeout_ms: u64,
    /// Named agent whose model + system prompt are resolved by the handler
    /// from the database. When set, `model`/`provider`/`base_url`/
    /// `api_key_env` may be empty (they are filled in by the handler).
    pub agent_id: Option<String>,
    /// System prompt resolved from the agent profile (never caller-supplied).
    pub system_prompt: Option<String>,
}

/// Parse and validate `params_json` for the `chat_completion` action.
/// Returns a human-readable error message on any validation failure —
/// caller maps that straight into `ActionResponse.error`.
pub fn parse_request(params_json: &[u8]) -> Result<ChatCompletionParams, String> {
    #[derive(serde::Deserialize)]
    struct RawMessage {
        role: String,
        content: String,
    }

    #[derive(serde::Deserialize)]
    struct Raw {
        provider: Option<String>,
        base_url: Option<String>,
        model: Option<String>,
        api_key_env: Option<String>,
        messages: Option<Vec<RawMessage>>,
        max_tokens: Option<u32>,
        timeout_ms: Option<u64>,
        agent_id: Option<String>,
    }

    let raw: Raw = serde_json::from_slice(params_json).map_err(|e| format!("invalid JSON: {e}"))?;

    let agent_id = raw.agent_id.filter(|s| !s.is_empty());

    let provider = match raw.provider {
        Some(p) => match p.as_str() {
            "anthropic" => Provider::Anthropic,
            "openai" => Provider::OpenAi,
            other => return Err(format!("unsupported provider: {other}")),
        },
        // Resolved from the database when an agent_id names the model.
        None if agent_id.is_some() => Provider::OpenAi,
        None => return Err("missing required field: provider".to_string()),
    };

    let base_url = match (raw.base_url, provider) {
        (Some(u), _) if !u.is_empty() => u,
        (_, Provider::Anthropic) => DEFAULT_ANTHROPIC_BASE_URL.to_string(),
        (_, Provider::OpenAi) if agent_id.is_some() => String::new(),
        (_, Provider::OpenAi) => return Err("missing required field: base_url".to_string()),
    };

    let model = raw.model.unwrap_or_default();
    if model.is_empty() && agent_id.is_none() {
        return Err("missing required field: model".to_string());
    }

    let api_key_env = raw.api_key_env.unwrap_or_default();
    if api_key_env.is_empty() && agent_id.is_none() {
        return Err("missing required field: api_key_env".to_string());
    }

    let raw_messages = raw.messages.ok_or("missing required field: messages")?;
    if raw_messages.is_empty() {
        return Err("messages must not be empty".to_string());
    }
    let messages = raw_messages
        .into_iter()
        .map(|m| Message {
            role: m.role,
            content: m.content,
        })
        .collect();

    let max_tokens = raw
        .max_tokens
        .unwrap_or(DEFAULT_MAX_TOKENS)
        .min(MAX_MAX_TOKENS);
    let timeout_ms = raw.timeout_ms.unwrap_or(MAX_TIMEOUT_MS).min(MAX_TIMEOUT_MS);

    Ok(ChatCompletionParams {
        provider,
        base_url,
        model,
        api_key_env,
        messages,
        max_tokens,
        timeout_ms,
        agent_id,
        system_prompt: None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingParams {
    pub provider: Provider,
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
    pub input: String,
    pub timeout_ms: u64,
    pub agent_id: Option<String>,
}

pub fn parse_embedding_request(params_json: &[u8]) -> Result<EmbeddingParams, String> {
    #[derive(serde::Deserialize)]
    struct Raw {
        provider: Option<String>,
        base_url: Option<String>,
        model: Option<String>,
        api_key_env: Option<String>,
        input: Option<String>,
        timeout_ms: Option<u64>,
        agent_id: Option<String>,
    }
    let raw: Raw =
        serde_json::from_slice(params_json).map_err(|e| format!("invalid JSON: {e}"))?;
    let agent_id = raw.agent_id.filter(|s| !s.is_empty());
    let provider = match raw.provider {
        Some(p) => match p.as_str() {
            "openai" => Provider::OpenAi,
            "anthropic" => return Err("anthropic does not support embeddings".to_string()),
            other => return Err(format!("unsupported provider: {other}")),
        },
        None if agent_id.is_some() => Provider::OpenAi,
        None => return Err("missing required field: provider".to_string()),
    };
    let base_url = match (raw.base_url, provider) {
        (Some(u), _) if !u.is_empty() => u,
        (_, _) if agent_id.is_some() => String::new(),
        (_, Provider::OpenAi) => return Err("missing required field: base_url".to_string()),
        (_, _) => return Err("missing required field: base_url".to_string()),
    };
    let model = raw.model.unwrap_or_default();
    if model.is_empty() && agent_id.is_none() {
        return Err("missing required field: model".to_string());
    }
    let api_key_env = raw.api_key_env.unwrap_or_default();
    if api_key_env.is_empty() && agent_id.is_none() {
        return Err("missing required field: api_key_env".to_string());
    }
    let input = raw.input.ok_or("missing required field: input")?;
    if input.trim().is_empty() {
        return Err("input must not be empty".to_string());
    }
    if input.len() > 10000 {
        return Err("input too long (max 10000)".to_string());
    }
    let timeout_ms = raw.timeout_ms.unwrap_or(MAX_TIMEOUT_MS).min(MAX_TIMEOUT_MS);
    Ok(EmbeddingParams {
        provider,
        base_url,
        model,
        api_key_env,
        input,
        timeout_ms,
        agent_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_anthropic_json() -> serde_json::Value {
        serde_json::json!({
            "provider": "anthropic",
            "model": "claude-sonnet-5",
            "api_key_env": "ANTHROPIC_API_KEY",
            "messages": [{"role": "user", "content": "hi"}],
        })
    }

    #[test]
    fn accepts_minimal_anthropic_request() {
        let body = valid_anthropic_json().to_string();
        let params = parse_request(body.as_bytes()).unwrap();
        assert_eq!(params.provider, Provider::Anthropic);
        assert_eq!(params.base_url, DEFAULT_ANTHROPIC_BASE_URL);
        assert_eq!(params.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(params.messages.len(), 1);
    }

    #[test]
    fn rejects_missing_provider() {
        let mut body = valid_anthropic_json();
        body.as_object_mut().unwrap().remove("provider");
        let err = parse_request(body.to_string().as_bytes()).unwrap_err();
        assert!(err.contains("provider"), "error was: {err}");
    }

    #[test]
    fn agent_id_allows_omitting_provider_and_model() {
        let body = serde_json::json!({
            "agent_id": "code",
            "messages": [{"role": "user", "content": "hi"}],
        });
        let params = parse_request(body.to_string().as_bytes()).unwrap();
        assert_eq!(params.agent_id.as_deref(), Some("code"));
        assert!(params.model.is_empty());
        assert!(params.base_url.is_empty());
        assert!(params.api_key_env.is_empty());
    }

    #[test]
    fn rejects_unsupported_provider() {
        let mut body = valid_anthropic_json();
        body["provider"] = "gemini".into();
        let err = parse_request(body.to_string().as_bytes()).unwrap_err();
        assert!(err.contains("unsupported provider"), "error was: {err}");
    }

    #[test]
    fn openai_requires_base_url() {
        let mut body = valid_anthropic_json();
        body["provider"] = "openai".into();
        let err = parse_request(body.to_string().as_bytes()).unwrap_err();
        assert!(err.contains("base_url"), "error was: {err}");
    }

    #[test]
    fn openai_accepts_explicit_base_url() {
        let mut body = valid_anthropic_json();
        body["provider"] = "openai".into();
        body["base_url"] = "http://localhost:11434/v1".into();
        let params = parse_request(body.to_string().as_bytes()).unwrap();
        assert_eq!(params.provider, Provider::OpenAi);
        assert_eq!(params.base_url, "http://localhost:11434/v1");
    }

    #[test]
    fn rejects_missing_messages() {
        let mut body = valid_anthropic_json();
        body.as_object_mut().unwrap().remove("messages");
        let err = parse_request(body.to_string().as_bytes()).unwrap_err();
        assert!(err.contains("messages"), "error was: {err}");
    }

    #[test]
    fn rejects_empty_messages() {
        let mut body = valid_anthropic_json();
        body["messages"] = serde_json::json!([]);
        let err = parse_request(body.to_string().as_bytes()).unwrap_err();
        assert!(err.contains("messages"), "error was: {err}");
    }

    #[test]
    fn clamps_max_tokens_above_cap() {
        let mut body = valid_anthropic_json();
        body["max_tokens"] = 999_999.into();
        let params = parse_request(body.to_string().as_bytes()).unwrap();
        assert_eq!(params.max_tokens, MAX_MAX_TOKENS);
    }

    #[test]
    fn rejects_missing_api_key_env() {
        let mut body = valid_anthropic_json();
        body.as_object_mut().unwrap().remove("api_key_env");
        let err = parse_request(body.to_string().as_bytes()).unwrap_err();
        assert!(err.contains("api_key_env"), "error was: {err}");
    }

    #[test]
    fn allowed_key_envs_empty_by_default() {
        assert!(parse_allowed_key_envs("").is_empty());
    }

    #[test]
    fn allowed_key_envs_parses_comma_list() {
        let allowed = parse_allowed_key_envs("ANTHROPIC_API_KEY, OPENAI_API_KEY ,,");
        assert!(is_allowed_key_env("ANTHROPIC_API_KEY", &allowed));
        assert!(is_allowed_key_env("OPENAI_API_KEY", &allowed));
        assert_eq!(allowed.len(), 2);
    }

    #[test]
    fn is_allowed_key_env_rejects_unlisted_name() {
        let allowed = parse_allowed_key_envs("ANTHROPIC_API_KEY");
        assert!(!is_allowed_key_env("AWS_SECRET_ACCESS_KEY", &allowed));
    }

    #[test]
    fn is_allowed_key_env_is_case_sensitive() {
        let allowed = parse_allowed_key_envs("ANTHROPIC_API_KEY");
        assert!(!is_allowed_key_env("anthropic_api_key", &allowed));
    }

    #[test]
    fn is_allowed_key_env_rejects_everything_when_empty() {
        let allowed = parse_allowed_key_envs("");
        assert!(!is_allowed_key_env("ANTHROPIC_API_KEY", &allowed));
    }
}
