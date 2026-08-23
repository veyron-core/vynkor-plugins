//! OpenAI-compatible chat completions adapter
//! (`POST {base_url}/chat/completions`) — covers OpenAI, OpenRouter, Ollama,
//! and any other self-hosted gateway that speaks the same wire shape.

use std::collections::HashMap;

use super::{ChatResult, EmbeddingResult, EmbeddingProvider, HttpRequestJson, Provider, Usage};
use crate::request::{ChatCompletionParams, EmbeddingParams};

pub struct OpenAiCompatProvider;

impl Provider for OpenAiCompatProvider {
    fn build_http_request(&self, params: &ChatCompletionParams, api_key: &str) -> HttpRequestJson {
        let url = format!("{}/chat/completions", params.base_url.trim_end_matches('/'));

        let mut messages: Vec<serde_json::Value> = params
            .messages
            .iter()
            .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
            .collect();
        if let Some(system) = params.system_prompt.as_deref().filter(|s| !s.is_empty()) {
            messages.insert(0, serde_json::json!({"role": "system", "content": system}));
        }

        let body = serde_json::json!({
            "model": params.model,
            "max_tokens": params.max_tokens,
            "messages": messages,
        })
        .to_string();

        let mut headers = HashMap::new();
        // Omitted when the resolved key is empty (e.g. a local Ollama
        // instance with no auth) rather than sending `Bearer ` with an
        // empty token.
        if !api_key.is_empty() {
            headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
        }
        headers.insert("content-type".to_string(), "application/json".to_string());

        HttpRequestJson {
            method: "POST",
            url,
            headers,
            body,
            timeout_ms: params.timeout_ms,
        }
    }

    fn parse_response(&self, body: &[u8]) -> Result<ChatResult, String> {
        #[derive(serde::Deserialize)]
        struct ResponseMessage {
            #[serde(default)]
            content: String,
        }
        #[derive(serde::Deserialize)]
        struct Choice {
            message: ResponseMessage,
            #[serde(default)]
            finish_reason: Option<String>,
        }
        #[derive(serde::Deserialize, Default)]
        struct OpenAiUsage {
            #[serde(default)]
            prompt_tokens: u64,
            #[serde(default)]
            completion_tokens: u64,
        }
        #[derive(serde::Deserialize)]
        struct Response {
            choices: Vec<Choice>,
            #[serde(default)]
            usage: OpenAiUsage,
        }

        let resp: Response = serde_json::from_slice(body)
            .map_err(|e| format!("malformed openai-compatible response: {e}"))?;
        let choice = resp
            .choices
            .into_iter()
            .next()
            .ok_or("openai-compatible response has no choices")?;

        Ok(ChatResult {
            content: choice.message.content,
            stop_reason: choice.finish_reason.unwrap_or_default(),
            usage: Usage {
                input_tokens: resp.usage.prompt_tokens,
                output_tokens: resp.usage.completion_tokens,
            },
        })
    }
}

impl EmbeddingProvider for OpenAiCompatProvider {
    fn build_embedding_request(
        &self,
        params: &EmbeddingParams,
        api_key: &str,
    ) -> HttpRequestJson {
        let url = format!("{}/embeddings", params.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": params.model,
            "input": params.input,
        })
        .to_string();
        let mut headers = HashMap::new();
        if !api_key.is_empty() {
            headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
        }
        headers.insert("content-type".to_string(), "application/json".to_string());
        HttpRequestJson {
            method: "POST",
            url,
            headers,
            body,
            timeout_ms: params.timeout_ms,
        }
    }

    fn parse_embedding_response(&self, body: &[u8]) -> Result<EmbeddingResult, String> {
        #[derive(serde::Deserialize)]
        struct EmbeddingData {
            embedding: Vec<f32>,
        }
        #[derive(serde::Deserialize, Default)]
        struct EmbeddingUsage {
            #[serde(default)]
            prompt_tokens: u64,
            #[serde(default)]
            total_tokens: u64,
        }
        #[derive(serde::Deserialize)]
        struct Response {
            data: Vec<EmbeddingData>,
            #[serde(default)]
            model: String,
            #[serde(default)]
            usage: EmbeddingUsage,
        }
        let resp: Response = serde_json::from_slice(body)
            .map_err(|e| format!("malformed openai embedding response: {e}"))?;
        let datum = resp
            .data
            .into_iter()
            .next()
            .ok_or("openai embedding response has no data")?;
        let dim = datum.embedding.len();
        Ok(EmbeddingResult {
            embedding: datum.embedding,
            dim,
            model: resp.model,
            usage: Usage {
                input_tokens: resp.usage.prompt_tokens,
                output_tokens: 0,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{Message, Provider as ReqProvider};

    fn params(base_url: &str) -> ChatCompletionParams {
        ChatCompletionParams {
            provider: ReqProvider::OpenAi,
            base_url: base_url.to_string(),
            model: "gpt-4o".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
            max_tokens: 1024,
            timeout_ms: 30_000,
            agent_id: None,
            system_prompt: None,
        }
    }

    #[test]
    fn builds_request_with_bearer_auth() {
        let req = OpenAiCompatProvider
            .build_http_request(&params("https://api.openai.com/v1"), "sk-secret");
        assert_eq!(req.url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(
            req.headers.get("Authorization").unwrap(),
            "Bearer sk-secret"
        );
    }

    #[test]
    fn omits_auth_header_when_key_empty() {
        let req = OpenAiCompatProvider.build_http_request(&params("http://localhost:11434/v1"), "");
        assert!(!req.headers.contains_key("Authorization"));
    }

    #[test]
    fn strips_trailing_slash_from_base_url() {
        let req =
            OpenAiCompatProvider.build_http_request(&params("https://openrouter.ai/api/v1/"), "k");
        assert_eq!(req.url, "https://openrouter.ai/api/v1/chat/completions");
    }

    #[test]
    fn parses_valid_response() {
        let body = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2}
        })
        .to_string();
        let result = OpenAiCompatProvider
            .parse_response(body.as_bytes())
            .unwrap();
        assert_eq!(result.content, "hello");
        assert_eq!(result.stop_reason, "stop");
        assert_eq!(result.usage.input_tokens, 4);
        assert_eq!(result.usage.output_tokens, 2);
    }

    #[test]
    fn rejects_response_with_no_choices() {
        let body = serde_json::json!({"choices": []}).to_string();
        let err = OpenAiCompatProvider
            .parse_response(body.as_bytes())
            .unwrap_err();
        assert!(err.contains("no choices"), "error was: {err}");
    }

    #[test]
    fn rejects_malformed_json() {
        let err = OpenAiCompatProvider
            .parse_response(b"not json")
            .unwrap_err();
        assert!(err.contains("malformed"), "error was: {err}");
    }
}
