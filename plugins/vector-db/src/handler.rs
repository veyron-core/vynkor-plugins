use serde_json::{json, Value};
use sqlx::Row;

use crate::db::{sanitize_caller_id, sanitize_collection, sanitize_id, DbPools};
use crate::embed::{cosine_similarity, fake_embed, validate_and_normalize_vector};
use crate::request::{parse_request, VectorRequest};

pub struct Handler {
    pools: DbPools,
    max_response_bytes: usize,
    default_dim: usize,
}

impl Handler {
    pub fn new(pools: DbPools, max_response_bytes: usize, default_dim: usize) -> Self {
        Self {
            pools,
            max_response_bytes,
            default_dim,
        }
    }

    pub async fn handle(
        &self,
        caller_plugin_id: &str,
        action: &str,
        params_json: &[u8],
    ) -> Result<Value, String> {
        self.handle_inner(caller_plugin_id, action, params_json).await
    }

    async fn handle_inner(
        &self,
        caller_plugin_id: &str,
        action: &str,
        params_json: &[u8],
    ) -> Result<Value, String> {
        sanitize_caller_id(caller_plugin_id)?;
        let req = parse_request(action, params_json)?;
        let pool = self.pools.pool_for(caller_plugin_id).await?;

        match req {
            VectorRequest::Upsert {
                collection,
                id,
                text,
                vector,
                metadata,
            } => {
                let collection = sanitize_collection(&collection)?.to_string();
                let id = sanitize_id(&id)?.to_string();
                let text_val = text.unwrap_or_default();
                let metadata_str = metadata
                    .map(|v| serde_json::to_string(&v).unwrap_or_else(|_| "{}".to_string()))
                    .unwrap_or_else(|| "{}".to_string());

                // Resolve dimension and vector
                let dim = self.resolve_collection_dim(&pool, &collection).await?;
                let vec = if let Some(v) = vector {
                    validate_and_normalize_vector(v, dim)?
                } else {
                    let d = dim.unwrap_or(self.default_dim);
                    let emb = fake_embed(&text_val, d);
                    emb
                };

                // Check dim consistency if collection exists — handle race by INSERT OR IGNORE
                if let Some(existing_dim) = dim {
                    if vec.len() != existing_dim {
                        return Err(format!(
                            "dimension mismatch: collection '{}' dim {existing_dim}, got {}",
                            collection,
                            vec.len()
                        ));
                    }
                } else {
                    // Create collection with this dim; race-safe (concurrent first-writes)
                    let res = sqlx::query(
                        "insert or ignore into collections (name, dim, created_at) values (?1, ?2, ?3)",
                    )
                    .bind(&collection)
                    .bind(vec.len() as i64)
                    .bind(crate::db::now_ms())
                    .execute(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                    if res.rows_affected() == 0 {
                        // Another task created it concurrently — verify dim matches
                        let existing = self.resolve_collection_dim(&pool, &collection).await?;
                        if let Some(existing_dim) = existing {
                            if vec.len() != existing_dim {
                                return Err(format!(
                                    "dimension mismatch: collection '{}' dim {existing_dim}, got {}",
                                    collection,
                                    vec.len()
                                ));
                            }
                        }
                    }
                }

                let vector_json = serde_json::to_string(&vec).map_err(|e| e.to_string())?;
                if vector_json.len() > 1024 * 1024 {
                    return Err("vector too large".to_string());
                }

                sqlx::query(
                    "insert into vectors (collection, id, text, vector, metadata, updated_at) values (?1, ?2, ?3, ?4, ?5, ?6) \
                     on conflict(collection, id) do update set text=excluded.text, vector=excluded.vector, metadata=excluded.metadata, updated_at=excluded.updated_at"
                )
                .bind(&collection)
                .bind(&id)
                .bind(&text_val)
                .bind(&vector_json)
                .bind(&metadata_str)
                .bind(crate::db::now_ms())
                .execute(&pool)
                .await
                .map_err(|e| e.to_string())?;

                Ok(json!({"ok": true, "id": id, "dim": vec.len()}))
            }
            VectorRequest::Query {
                collection,
                text,
                vector,
                top_k,
                include_vector,
                filter,
            } => {
                let collection = sanitize_collection(&collection)?.to_string();
                let pool_dim = self.resolve_collection_dim(&pool, &collection).await?;
                let dim = match pool_dim {
                    Some(d) => d,
                    None => return Ok(json!({"results": []})),
                };

                let query_vec = if let Some(v) = vector {
                    validate_and_normalize_vector(v, Some(dim))?
                } else {
                    let t = text.unwrap_or_default();
                    fake_embed(&t, dim)
                };

                // Load all vectors in collection
                let rows = sqlx::query(
                    "select id, text, vector, metadata from vectors where collection = ?1",
                )
                .bind(&collection)
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?;

                let mut scored: Vec<(String, f32, String, String)> = Vec::new();
                for row in rows {
                    let vid: String = row.get("id");
                    let vtext: String = row.get("text");
                    let vvec_json: String = row.get("vector");
                    let vmeta: String = row.get("metadata");

                    // Filter by metadata if provided (exact match on top-level keys)
                    if let Some(ref filt) = filter {
                        if let Some(fobj) = filt.as_object() {
                            if !fobj.is_empty() {
                                let meta_val: Value =
                                    serde_json::from_str(&vmeta).unwrap_or(json!({}));
                                let mut matches = true;
                                for (k, fv) in fobj {
                                    if meta_val.get(k) != Some(fv) {
                                        matches = false;
                                        break;
                                    }
                                }
                                if !matches {
                                    continue;
                                }
                            }
                        }
                    }

                    let stored: Vec<f32> =
                        serde_json::from_str(&vvec_json).map_err(|e| format!("corrupt vector for {vid}: {e}"))?;
                    let score = cosine_similarity(&query_vec, &stored);
                    scored.push((vid, score, vtext, vmeta));
                }

                scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                scored.truncate(top_k);

                // Estimate response size
                let mut results = Vec::new();
                let mut bytes = 0usize;
                for (vid, score, vtext, vmeta) in scored {
                    let meta_val: Value = serde_json::from_str(&vmeta).unwrap_or(json!({}));
                    let mut obj = json!({
                        "id": vid,
                        "score": score,
                        "text": vtext,
                        "metadata": meta_val
                    });
                    if include_vector {
                        // re-fetch vector for include
                        let vvec_json: String = sqlx::query_scalar(
                            "select vector from vectors where collection = ?1 and id = ?2",
                        )
                        .bind(&collection)
                        .bind(&vid)
                        .fetch_one(&pool)
                        .await
                        .map_err(|e| e.to_string())?;
                        let vec_val: Value = serde_json::from_str(&vvec_json).unwrap_or(json!([]));
                        obj["vector"] = vec_val;
                    }
                    bytes += serde_json::to_vec(&obj).unwrap().len() + 1;
                    if bytes > self.max_response_bytes {
                        return Err(format!(
                            "query result exceeds max_response_bytes (> {})",
                            self.max_response_bytes
                        ));
                    }
                    results.push(obj);
                }

                Ok(json!({"results": results}))
            }
            VectorRequest::Get { collection, id } => {
                let collection = sanitize_collection(&collection)?.to_string();
                let id = sanitize_id(&id)?.to_string();
                let row = sqlx::query(
                    "select text, vector, metadata from vectors where collection = ?1 and id = ?2",
                )
                .bind(&collection)
                .bind(&id)
                .fetch_optional(&pool)
                .await
                .map_err(|e| e.to_string())?;

                match row {
                    Some(r) => {
                        let text: String = r.get("text");
                        let vector_json: String = r.get("vector");
                        let metadata_str: String = r.get("metadata");
                        let vector: Value =
                            serde_json::from_str(&vector_json).unwrap_or(json!([]));
                        let metadata: Value =
                            serde_json::from_str(&metadata_str).unwrap_or(json!({}));
                        let dim = vector.as_array().map(|a| a.len()).unwrap_or(0);
                        Ok(json!({
                            "found": true,
                            "id": id,
                            "text": text,
                            "vector": vector,
                            "metadata": metadata,
                            "dim": dim
                        }))
                    }
                    None => Ok(json!({"found": false})),
                }
            }
            VectorRequest::Delete { collection, id } => {
                let collection = sanitize_collection(&collection)?.to_string();
                let id = sanitize_id(&id)?.to_string();
                let res = sqlx::query("delete from vectors where collection = ?1 and id = ?2")
                    .bind(&collection)
                    .bind(&id)
                    .execute(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({"deleted": res.rows_affected() > 0}))
            }
            VectorRequest::List { prefix } => {
                let rows: Vec<(String,)> = if prefix.is_empty() {
                    sqlx::query_as("select name from collections order by name")
                        .fetch_all(&pool)
                        .await
                        .map_err(|e| e.to_string())?
                } else {
                    let pattern = format!("{prefix}%");
                    sqlx::query_as(
                        "select name from collections where name like ?1 escape '\\' order by name",
                    )
                    .bind(pattern)
                    .fetch_all(&pool)
                    .await
                    .map_err(|e| e.to_string())?
                };
                let collections: Vec<String> = rows.into_iter().map(|(n,)| n).collect();
                Ok(json!({"collections": collections}))
            }
            VectorRequest::Stats { collection } => {
                let collection = sanitize_collection(&collection)?.to_string();
                let dim: Option<(i64,)> =
                    sqlx::query_as("select dim from collections where name = ?1")
                        .bind(&collection)
                        .fetch_optional(&pool)
                        .await
                        .map_err(|e| e.to_string())?;
                match dim {
                    Some((d,)) => {
                        let count: (i64,) = sqlx::query_as(
                            "select count(*) from vectors where collection = ?1",
                        )
                        .bind(&collection)
                        .fetch_one(&pool)
                        .await
                        .map_err(|e| e.to_string())?;
                        Ok(json!({"count": count.0, "dim": d}))
                    }
                    None => Ok(json!({"count": 0, "dim": 0})),
                }
            }
        }
    }

    async fn resolve_collection_dim(
        &self,
        pool: &sqlx::SqlitePool,
        collection: &str,
    ) -> Result<Option<usize>, String> {
        let row: Option<(i64,)> =
            sqlx::query_as("select dim from collections where name = ?1")
                .bind(collection)
                .fetch_optional(pool)
                .await
                .map_err(|e| e.to_string())?;
        Ok(row.map(|(d,)| d as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DbConfig, DbPools};
    use serde_json::json;

    fn make_handler(dir: &std::path::Path) -> Handler {
        let pools = DbPools::new(DbConfig {
            data_dir: dir.to_path_buf(),
            pool_size: 2,
            busy_timeout_ms: 2000,
            max_db_bytes: 0,
        });
        Handler::new(pools, 4 * 1024 * 1024, 8)
    }

    #[tokio::test]
    async fn upsert_and_query_text() {
        let dir = tempfile::tempdir().unwrap();
        let h = make_handler(dir.path());
        let caller = "agent";
        h.handle(
            caller,
            "vec_upsert",
            br#"{"collection":"mem","id":"1","text":"hello world"}"#,
        )
        .await
        .unwrap();
        h.handle(
            caller,
            "vec_upsert",
            br#"{"collection":"mem","id":"2","text":"goodbye world"}"#,
        )
        .await
        .unwrap();
        let res = h
            .handle(caller, "vec_query", br#"{"collection":"mem","text":"hello world","top_k":1}"#)
            .await
            .unwrap();
        assert_eq!(res["results"][0]["id"], "1");
        assert!(res["results"][0]["score"].as_f64().unwrap() > 0.9);
    }

    #[tokio::test]
    async fn upsert_with_vector_and_query_by_vector() {
        let dir = tempfile::tempdir().unwrap();
        let h = make_handler(dir.path());
        let caller = "agent";
        h.handle(
            caller,
            "vec_upsert",
            br#"{"collection":"c","id":"a","vector":[1,0,0,0,0,0,0,0]}"#,
        )
        .await
        .unwrap();
        h.handle(
            caller,
            "vec_upsert",
            br#"{"collection":"c","id":"b","vector":[0,1,0,0,0,0,0,0]}"#,
        )
        .await
        .unwrap();
        let res = h
            .handle(
                caller,
                "vec_query",
                br#"{"collection":"c","vector":[1,0,0,0,0,0,0,0],"top_k":2}"#,
            )
            .await
            .unwrap();
        assert_eq!(res["results"][0]["id"], "a");
        assert_eq!(res["results"][1]["id"], "b");
        assert!(res["results"][0]["score"].as_f64().unwrap() > res["results"][1]["score"].as_f64().unwrap());
    }

    #[tokio::test]
    async fn dimension_mismatch_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let h = make_handler(dir.path());
        let caller = "agent";
        h.handle(
            caller,
            "vec_upsert",
            br#"{"collection":"c","id":"1","vector":[1,0,0,0,0,0,0,0]}"#,
        )
        .await
        .unwrap();
        let err = h
            .handle(
                caller,
                "vec_upsert",
                br#"{"collection":"c","id":"2","vector":[1,0,0]}"#,
            )
            .await
            .unwrap_err();
        assert!(err.contains("dimension mismatch"), "err was: {err}");
    }

    #[tokio::test]
    async fn per_caller_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let h = make_handler(dir.path());
        h.handle(
            "caller_a",
            "vec_upsert",
            br#"{"collection":"c","id":"1","text":"hello"}"#,
        )
        .await
        .unwrap();
        let res = h
            .handle("caller_b", "vec_query", br#"{"collection":"c","text":"hello","top_k":5}"#)
            .await
            .unwrap();
        assert_eq!(res["results"].as_array().unwrap().len(), 0);
        let res_a = h
            .handle("caller_a", "vec_query", br#"{"collection":"c","text":"hello","top_k":5}"#)
            .await
            .unwrap();
        assert_eq!(res_a["results"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn get_delete_list_stats() {
        let dir = tempfile::tempdir().unwrap();
        let h = make_handler(dir.path());
        let caller = "agent";
        h.handle(caller, "vec_upsert", br#"{"collection":"mem","id":"1","text":"hi"}"#)
            .await
            .unwrap();
        let get = h
            .handle(caller, "vec_get", br#"{"collection":"mem","id":"1"}"#)
            .await
            .unwrap();
        assert_eq!(get["found"], true);
        assert_eq!(get["id"], "1");

        let stats = h
            .handle(caller, "vec_stats", br#"{"collection":"mem"}"#)
            .await
            .unwrap();
        assert_eq!(stats["count"], 1);

        let list = h.handle(caller, "vec_list", b"{}").await.unwrap();
        assert!(list["collections"].as_array().unwrap().contains(&json!("mem")));

        let del = h
            .handle(caller, "vec_delete", br#"{"collection":"mem","id":"1"}"#)
            .await
            .unwrap();
        assert_eq!(del["deleted"], true);
        let del2 = h
            .handle(caller, "vec_delete", br#"{"collection":"mem","id":"1"}"#)
            .await
            .unwrap();
        assert_eq!(del2["deleted"], false);

        let get2 = h
            .handle(caller, "vec_get", br#"{"collection":"mem","id":"1"}"#)
            .await
            .unwrap();
        assert_eq!(get2["found"], false);
    }

    #[tokio::test]
    async fn filter_and_topk() {
        let dir = tempfile::tempdir().unwrap();
        let h = make_handler(dir.path());
        let caller = "agent";
        h.handle(
            caller,
            "vec_upsert",
            br#"{"collection":"c","id":"1","text":"hello","metadata":{"tag":"a"}}"#,
        )
        .await
        .unwrap();
        h.handle(
            caller,
            "vec_upsert",
            br#"{"collection":"c","id":"2","text":"hello","metadata":{"tag":"b"}}"#,
        )
        .await
        .unwrap();
        let res = h
            .handle(
                caller,
                "vec_query",
                br#"{"collection":"c","text":"hello","top_k":5,"filter":{"tag":"a"}}"#,
            )
            .await
            .unwrap();
        assert_eq!(res["results"].as_array().unwrap().len(), 1);
        assert_eq!(res["results"][0]["id"], "1");
    }

    #[tokio::test]
    async fn handles_concurrent_caller_isolation_under_load() {
        let dir = tempfile::tempdir().unwrap();
        let h = std::sync::Arc::new(make_handler(dir.path()));
        let mut tasks = Vec::new();
        for caller_n in 0..4 {
            let h2 = h.clone();
            tasks.push(tokio::spawn(async move {
                let caller = format!("caller_{caller_n}");
                for i in 0..10 {
                    let params = serde_json::json!({
                        "collection": "c",
                        "id": format!("doc:{i}"),
                        "text": format!("text {caller_n} {i}")
                    });
                    let bytes = serde_json::to_vec(&params).unwrap();
                    h2.handle(&caller, "vec_upsert", &bytes).await.unwrap();
                }
                let q = serde_json::json!({"collection":"c","text":"text","top_k":20});
                let res = h2
                    .handle(&caller, "vec_query", &serde_json::to_vec(&q).unwrap())
                    .await
                    .unwrap();
                assert_eq!(res["results"].as_array().unwrap().len(), 10);
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
    }
}
