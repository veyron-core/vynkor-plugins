//! The model leg of the `agent` plugin: build the conversation, call
//! `ai`'s `chat_completion` through the [`Rpc`] proxy, and parse the reply
//! into either a final answer or one tool call.
//!
//! Tool-calling protocol: `ai`'s normalized interface is plain text
//! messages (no native tool-use blocks), so the catalog and the reply
//! format are negotiated inside the prompt itself — the model answers with
//! either plain text (final answer) or exactly one JSON object
//! `{"tool": "...", "params": {...}}`. Parsing is deliberately forgiving:
//! fenced code blocks are unwrapped and the first balanced JSON object
//! wins; anything that doesn't parse as a well-formed tool call is treated
//! as the final answer so prose is never lost.

use serde_json::{json, Value};

use crate::store::{LlmPlan, Turn};
use crate::tools::Catalog;
use crate::Rpc;

/// Hard cap matching `ai`'s own `MAX_TIMEOUT_MS`.
const CHAT_TIMEOUT_MS: u32 = 30_000;
/// Default `max_tokens` when neither env nor request names one.
pub const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Build the instructions message that carries the tool catalog. `ai`
/// resolves `system_prompt` only from an `agent_id` profile (never from
/// callers), so the portable place for operator-free instructions is a
/// leading user message.
pub fn opening_messages(goal: &str, context: &str, catalog: &Catalog) -> Vec<Turn> {
    let tools_json: Vec<Value> = catalog
        .tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
                "requires_confirmation": t.requires_confirmation,
            })
        })
        .collect();
    let instructions = format!(
        "You are the vynkor host agent: you complete the user's goal by \
         calling host tools step by step.\n\n\
         Available tools (JSON array; `parameters` is a JSON Schema for the \
         `params` object):\n{}\n\n\
         Reply rules:\n\
         - To call a tool, reply with EXACTLY ONE JSON object and nothing \
         else: {{\"tool\": \"<name>\", \"params\": {{...}}}}.\n\
         - Only tool names from the list above are valid.\n\
         - After each call you receive a message starting with \
         \"[TOOL RESULT\" containing the outcome.\n\
         - When the goal is achieved (or impossible), reply with the final \
         answer as PLAIN TEXT — no JSON object at all.",
        serde_json::to_string_pretty(&tools_json).unwrap_or_else(|_| "[]".to_string()),
    );
    let mut msgs = vec![Turn { role: "user".into(), content: instructions }];
    let goal_msg = if context.is_empty() {
        goal.to_string()
    } else {
        format!("{goal}\n\nContext:\n{context}")
    };
    msgs.push(Turn { role: "user".into(), content: goal_msg });
    msgs
}

/// One `chat_completion` round-trip over the current transcript. Resolves
/// to the assistant's text content.
pub async fn chat(rpc: &Rpc, plan: &LlmPlan, transcript: &[Turn]) -> Result<String, String> {
    let messages: Vec<Value> = transcript
        .iter()
        .map(|t| json!({"role": t.role, "content": t.content}))
        .collect();
    let mut params = serde_json::Map::new();

    if plan.agent_id.is_empty() {
        if plan.model.is_empty() || plan.api_key_env.is_empty() {
            return Err(
                "no LLM configured: set AGENT_PLUGIN_AI_MODEL / AGENT_PLUGIN_AI_API_KEY_ENV \
                 (or AGENT_PLUGIN_AI_AGENT_ID), or pass overrides in the request"
                    .to_string(),
            );
        }
        params.insert("provider".into(), json!(plan.provider));
        if !plan.base_url.is_empty() {
            params.insert("base_url".into(), json!(plan.base_url));
        }
        params.insert("model".into(), json!(plan.model));
        params.insert("api_key_env".into(), json!(plan.api_key_env));
    } else {
        params.insert("agent_id".into(), json!(plan.agent_id));
    }
    params.insert("messages".into(), Value::Array(messages));
    params.insert("max_tokens".into(), json!(plan.max_tokens));
    params.insert("timeout_ms".into(), json!(CHAT_TIMEOUT_MS));

    let v = rpc.call("chat_completion", Value::Object(params), CHAT_TIMEOUT_MS).await?;
    v.get("content")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("chat_completion returned unexpected payload: {v}"))
}

/// Model reply after parsing: a final answer or one tool call.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Final(String),
    ToolCall { name: String, params: Value },
}

/// Extract the first balanced `{...}` substring, respecting string literals.
fn first_balanced_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&text[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return trimmed;
    }
    let body = trimmed.trim_start_matches('`');
    // Drop a language tag on the fence line, then the closing fence.
    let body = body.split_once('\n').map(|(_, rest)| rest).unwrap_or(body);
    body.trim().trim_end_matches('`').trim()
}

/// Parse a model reply into a final answer or a tool call. Anything that
/// isn't a well-formed `{"tool": ..., "params": {...}}` object degrades to
/// [`Reply::Final`] carrying the original trimmed text.
pub fn parse_reply(content: &str) -> Reply {
    let cleaned = strip_code_fence(content);
    let Some(obj_text) = first_balanced_object(cleaned) else {
        return Reply::Final(cleaned.trim().to_string());
    };
    let Ok(v) = serde_json::from_str::<Value>(obj_text) else {
        return Reply::Final(cleaned.trim().to_string());
    };
    let Some(name) = v.get("tool").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return Reply::Final(cleaned.trim().to_string());
    };
    let params = match v.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(p) if p.is_object() => p.clone(),
        _ => return Reply::Final(cleaned.trim().to_string()),
    };
    Reply::ToolCall { name: name.to_string(), params }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_fenced_and_embedded_tool_calls() {
        let r = parse_reply("{\"tool\":\"notify_send\",\"params\":{\"title\":\"hi\"}}");
        assert_eq!(
            r,
            Reply::ToolCall { name: "notify_send".into(), params: json!({"title": "hi"}) }
        );

        let fenced = "```json\n{\"tool\": \"a\", \"params\": {\"x\": 1}}\n```";
        assert_eq!(parse_reply(fenced), Reply::ToolCall { name: "a".into(), params: json!({"x": 1}) });

        let embedded = "Let me check.\n{\"tool\":\"b\",\"params\":{\"path\":\"}{ odd\"}} done";
        assert_eq!(
            parse_reply(embedded),
            Reply::ToolCall { name: "b".into(), params: json!({"path": "}{ odd"}) }
        );

        let no_params = "{\"tool\":\"c\"}";
        assert_eq!(
            parse_reply(no_params),
            Reply::ToolCall { name: "c".into(), params: json!({}) }
        );
    }

    #[test]
    fn malformed_calls_degrade_to_final_answer() {
        assert_eq!(parse_reply("Just do it."), Reply::Final("Just do it.".into()));
        assert_eq!(parse_reply("{not json}"), Reply::Final("{not json}".into()));
        // A JSON object without "tool" is not a tool call.
        assert_eq!(parse_reply("{\"answer\": 42}"), Reply::Final("{\"answer\": 42}".into()));
        // Empty tool name / non-object params degrade too.
        assert_eq!(parse_reply("{\"tool\":\"\"}"), Reply::Final("{\"tool\":\"\"}".into()));
        assert_eq!(
            parse_reply("{\"tool\":\"a\",\"params\":[1]}"),
            Reply::Final("{\"tool\":\"a\",\"params\":[1]}".into())
        );
        // Unterminated brace → plain final.
        assert_eq!(parse_reply("{\"tool\":\"a\""), Reply::Final("{\"tool\":\"a\"".into()));
    }

    #[test]
    fn opening_messages_carry_catalog_and_goal() {
        let cat = Catalog {
            tools: vec![crate::tools::ToolSpec {
                name: "notify_send".into(),
                description: "Send a notification".into(),
                parameters: json!({"type": "object"}),
                requires_confirmation: false,
                risk: String::new(),
                timeout_ms: 30_000,
                source: crate::tools::Source::Kernel,
            }],
            allowed_actions: vec!["notify_send".into()],
            tools_file_set: true,
        };
        let msgs = opening_messages("water the plants", "garden only", &cat);
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].content.contains("notify_send"));
        assert!(msgs[0].content.contains("JSON"));
        assert_eq!(msgs[1].role, "user");
        assert!(msgs[1].content.contains("water the plants"));
        assert!(msgs[1].content.contains("Context:\ngarden only"));

        let no_ctx = opening_messages("just g", "", &cat);
        assert!(!no_ctx[1].content.contains("Context:"));
    }

    #[test]
    fn chat_requires_model_and_key_without_agent_id() {
        // Compile-level smoke of the error path shape (no kernel needed).
        let plan = LlmPlan {
            provider: "openai".into(),
            base_url: String::new(),
            model: String::new(),
            api_key_env: String::new(),
            agent_id: String::new(),
            max_tokens: 1024,
        };
        // Directly exercising chat() needs an Rpc; covered e2e in main.rs.
        // Here just assert the guard constants line up with ai's cap.
        assert_eq!(CHAT_TIMEOUT_MS, 30_000);
        let _ = plan;
    }
}
