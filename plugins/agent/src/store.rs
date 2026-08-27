//! Typed access to the `database` plugin on behalf of `agent`.
//!
//! Same contract as `notes`/`calendar`: kernel-routed actions only via the
//! [`Rpc`] proxy, private namespace stamped by the kernel's per-caller
//! isolation. Every goal is one JSON document under `goal:<id>`; ids come
//! from an atomic `db_incr` counter (`meta:next_id`). No local state —
//! restart-safe by construction.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Rpc;

/// Counter key backing goal ids (atomic via `db_incr`).
pub const NEXT_ID_KEY: &str = "meta:next_id";
/// Key prefix for goal documents: `goal:<id>` → JSON [`GoalDoc`].
pub const KEY_PREFIX: &str = "goal:";

pub const STATUS_RUNNING: &str = "running";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_NEEDS_CONFIRMATION: &str = "needs_confirmation";
pub const STATUS_DECLINED: &str = "declined";
pub const STATUS_MAX_STEPS: &str = "max_steps_reached";
pub const STATUS_ERROR: &str = "error";

/// One message of the model conversation, persisted so a halted goal can be
/// resumed with full context later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    pub content: String,
}

/// The LLM routing plan snapshot taken at goal start — replayed unchanged by
/// `goal_resume` so an approved continuation uses the same provider/model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmPlan {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
    #[serde(default)]
    pub agent_id: String,
    pub max_tokens: u32,
}

/// One line of the human-facing step log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRec {
    pub n: u32,
    /// `tool_ok` | `tool_error` | `unknown_tool` | `halt_confirm` |
    /// `final` | `max_steps` | `error`
    pub kind: String,
    #[serde(default)]
    pub detail: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalDoc {
    pub id: String,
    pub title: String,
    pub goal: String,
    #[serde(default)]
    pub context: String,
    /// One of the `STATUS_*` constants.
    pub status: String,
    #[serde(default)]
    pub final_answer: String,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub steps: Vec<StepRec>,
    #[serde(default)]
    pub transcript: Vec<Turn>,
    #[serde(default)]
    pub pending_tool: String,
    #[serde(default)]
    pub pending_params: Value,
    /// Set when a provider rejected the native `tools` param and the goal
    /// degraded to the text protocol mid-flight; later steps skip it.
    #[serde(default)]
    pub native_tools_disabled: bool,
    pub llm: LlmPlan,
    pub max_steps: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default)]
    pub tool_counts: std::collections::BTreeMap<String, u32>,
    #[serde(default)]
    pub tool_last_ms: std::collections::BTreeMap<String, i64>,
}

impl GoalDoc {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            STATUS_COMPLETED | STATUS_DECLINED | STATUS_MAX_STEPS | STATUS_ERROR
        )
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Typed wrapper over the `database` actions used by agent.
pub struct Db {
    rpc: Rpc,
    timeout_ms: u32,
}

impl Db {
    pub fn new(rpc: Rpc, timeout_ms: u32) -> Self {
        Self { rpc, timeout_ms }
    }

    async fn call(&self, action: &str, params: Value) -> Result<Value, String> {
        self.rpc.call(action, params, self.timeout_ms).await
    }

    /// Next monotonic goal id (atomic counter in our own namespace).
    pub async fn next_id(&self) -> Result<u64, String> {
        let v = self.call("db_incr", serde_json::json!({"key": NEXT_ID_KEY})).await?;
        v.get("value")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("database.db_incr returned unexpected payload: {v}"))
    }

    pub async fn put(&self, doc: &GoalDoc) -> Result<(), String> {
        let key = format!("{KEY_PREFIX}{}", doc.id);
        let v = self.call("db_set", serde_json::json!({"key": key, "value": doc})).await?;
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(format!("database.db_set returned unexpected payload: {v}"));
        }
        Ok(())
    }

    /// Missing goals read as `None`; a present-but-corrupt document is an
    /// error (loudness over silent data loss on single-doc reads).
    pub async fn get(&self, id: &str) -> Result<Option<GoalDoc>, String> {
        let v =
            self.call("db_get", serde_json::json!({"key": format!("{KEY_PREFIX}{id}")})).await?;
        if v.get("found").and_then(Value::as_bool) != Some(true) {
            return Ok(None);
        }
        let value = v.get("value").cloned().unwrap_or(Value::Null);
        let doc: GoalDoc = serde_json::from_value(value)
            .map_err(|e| format!("stored goal \"{id}\" is corrupt: {e}"))?;
        Ok(Some(doc))
    }

    /// Newest first. A corrupt entry fails the whole listing loudly rather
    /// than silently vanishing.
    pub async fn list(&self, limit: usize) -> Result<Vec<GoalDoc>, String> {
        let v = self.call("db_keys", serde_json::json!({"prefix": KEY_PREFIX})).await?;
        let mut keys: Vec<String> = v
            .get("keys")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter().filter_map(|k| k.as_str()).map(str::to_string).collect::<Vec<_>>()
            })
            .ok_or_else(|| format!("database.db_keys returned unexpected payload: {v}"))?;
        keys.sort_by_key(|k| std::cmp::Reverse(key_num(k)));
        keys.truncate(limit);

        let batch = self.call("db_batch_get", serde_json::json!({"keys": keys})).await?;
        let values = batch
            .get("values")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("database.db_batch_get returned unexpected payload: {batch}"))?;

        let mut docs = Vec::new();
        for key in &keys {
            let value = values.get(key).cloned().unwrap_or(Value::Null);
            let doc: GoalDoc = serde_json::from_value(value)
                .map_err(|e| format!("stored goal \"{key}\" is corrupt: {e}"))?;
            docs.push(doc);
        }
        Ok(docs)
    }
}

fn key_num(key: &str) -> u64 {
    key.strip_prefix(KEY_PREFIX).and_then(|s| s.parse().ok()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_num_sorts_numerically() {
        assert_eq!(key_num("goal:12"), 12);
        assert_eq!(key_num("goal:x"), 0);
        let mut keys = vec!["goal:10", "goal:9", "goal:2"];
        keys.sort_by_key(|k| std::cmp::Reverse(key_num(k)));
        assert_eq!(keys, vec!["goal:10", "goal:9", "goal:2"]);
    }

    #[test]
    fn terminal_statuses_are_exact() {
        let mut doc = sample(STATUS_RUNNING);
        assert!(!doc.is_terminal());
        for s in [STATUS_COMPLETED, STATUS_DECLINED, STATUS_MAX_STEPS, STATUS_ERROR] {
            doc.status = s.to_string();
            assert!(doc.is_terminal(), "{s}");
        }
        assert!(!sample(STATUS_NEEDS_CONFIRMATION).is_terminal());
    }

    fn sample(status: &str) -> GoalDoc {
        GoalDoc {
            id: "1".into(),
            title: "t".into(),
            goal: "g".into(),
            context: String::new(),
            status: status.into(),
            final_answer: String::new(),
            error: String::new(),
            steps: Vec::new(),
            transcript: Vec::new(),
            pending_tool: String::new(),
            pending_params: Value::Null,
            native_tools_disabled: false,
            llm: LlmPlan {
                provider: "openai".into(),
                base_url: String::new(),
                model: "m".into(),
                api_key_env: "K".into(),
                agent_id: String::new(),
                max_tokens: 1024,
            },
            max_steps: 6,
            created_at_ms: 0,
            updated_at_ms: 0,
            tool_counts: Default::default(),
            tool_last_ms: Default::default(),
        }
    }
}
