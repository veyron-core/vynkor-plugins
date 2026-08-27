use serde_json::Value;
use crate::Rpc;
use crate::sampler::Sample;

pub const NEXT_ID_KEY: &str = "meta:next_id";
pub const KEY_PREFIX: &str = "metric:";

pub struct Db {
    rpc: Rpc,
    timeout_ms: u32,
}

impl Db {
    pub fn new(rpc: Rpc, timeout_ms: u32) -> Self { Self { rpc, timeout_ms } }
    async fn call(&self, action: &str, params: Value) -> Result<Value, String> {
        self.rpc.call(action, params, self.timeout_ms).await
    }
    pub async fn next_id(&self) -> Result<u64, String> {
        let v = self.call("db_incr", serde_json::json!({"key": NEXT_ID_KEY})).await?;
        v.get("value").and_then(Value::as_u64).ok_or_else(|| format!("db_incr bad: {v}"))
    }
    pub async fn put(&self, sample: &Sample) -> Result<(), String> {
        let key = format!("{KEY_PREFIX}{}", sample.id);
        let v = self.call("db_set", serde_json::json!({"key": key, "value": sample})).await?;
        if v.get("ok").and_then(Value::as_bool) != Some(true) { return Err(format!("db_set bad: {v}")); }
        Ok(())
    }
    pub async fn list(&self) -> Result<Vec<Sample>, String> {
        let v = self.call("db_keys", serde_json::json!({"prefix": KEY_PREFIX})).await?;
        let keys: Vec<String> = v.get("keys").and_then(Value::as_array).ok_or_else(|| format!("db_keys bad: {v}"))?.iter().filter_map(Value::as_str).map(|s| s.to_string()).collect();
        if keys.is_empty() { return Ok(Vec::new()); }
        let v = self.call("db_batch_get", serde_json::json!({"keys": keys})).await?;
        let values = v.get("values").and_then(Value::as_object).ok_or_else(|| format!("db_batch_get bad: {v}"))?;
        let mut out = Vec::new();
        for (k, val) in values {
            if val.is_null() { continue; }
            match serde_json::from_value::<Sample>(val.clone()) {
                Ok(s) => out.push(s),
                Err(e) => eprintln!("[metrics] skip corrupt {k}: {e}"),
            }
        }
        Ok(out)
    }
    pub async fn delete(&self, id: &str) -> Result<bool, String> {
        let v = self.call("db_delete", serde_json::json!({"key": format!("{KEY_PREFIX}{id}")})).await?;
        v.get("deleted").and_then(Value::as_bool).ok_or_else(|| format!("db_delete bad: {v}"))
    }
    pub async fn trim(&self, max_samples: usize) -> Result<usize, String> {
        if max_samples == 0 { return Ok(0); }
        let mut all = self.list().await?;
        if all.len() <= max_samples { return Ok(0); }
        all.sort_by_key(|s| s.timestamp_ms);
        let to_remove = all.len() - max_samples;
        let mut removed = 0;
        for s in all.iter().take(to_remove) {
            if self.delete(&s.id).await? { removed += 1; }
        }
        Ok(removed)
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)).unwrap_or(0)
}
