use std::sync::Arc;

use vector_db_plugin::db::{DbConfig, DbPools};
use vector_db_plugin::handler::Handler;
use vynkor_sdk::concurrent::serve_concurrent;
use vynkor_sdk::{VynkorClient, VynkorError};

fn load_config() -> DbConfig {
    let data_dir = std::env::var("VECTOR_DB_DATA_DIR")
        .unwrap_or_else(|_| panic!("VECTOR_DB_DATA_DIR must be set (see config.example.yaml)"));
    let pool_size = std::env::var("VECTOR_DB_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    let busy_timeout_ms = std::env::var("VECTOR_DB_BUSY_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5000);
    let max_db_bytes = std::env::var("VECTOR_DB_MAX_DB_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256 * 1024 * 1024);
    DbConfig {
        data_dir: data_dir.into(),
        pool_size,
        busy_timeout_ms,
        max_db_bytes,
    }
}

#[tokio::main]
async fn main() -> Result<(), VynkorError> {
    let max_response_bytes = std::env::var("VECTOR_DB_MAX_RESPONSE_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4 * 1024 * 1024);
    let default_dim = std::env::var("VECTOR_DB_DEFAULT_DIM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(384);

    let pools = DbPools::new(load_config());
    let handler = Arc::new(Handler::new(pools, max_response_bytes, default_dim));

    let client = VynkorClient::connect_from_env().await?;
    let token = std::env::var("VYN_JWT_TOKEN").unwrap_or_default();
    serve_concurrent(client, &token, handler).await?;

    println!("[vector-db] shutting down");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::UnixStream;
    use vector_db_plugin::db::{DbConfig, DbPools};
    use vector_db_plugin::handler::Handler;
    use vynkor_sdk::concurrent::run_concurrent_loop;
    use vynkor_sdk::proto::{envelope, ActionRequest, ActionStatus, Envelope, PluginShutdown};
    use vynkor_sdk::VynkorClient;

    #[tokio::test]
    async fn concurrent_upserts_without_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let pools = DbPools::new(DbConfig {
            data_dir: dir.path().to_path_buf(),
            pool_size: 4,
            busy_timeout_ms: 2000,
            max_db_bytes: 0,
        });
        let handler = Arc::new(Handler::new(pools, 4 * 1024 * 1024, 8));

        let (plugin_side, kernel_side) = UnixStream::pair().unwrap();
        let client = VynkorClient::from_stream(plugin_side, None);
        let mut kernel = VynkorClient::from_stream(kernel_side, None);

        let loop_task = tokio::spawn(run_concurrent_loop(client, handler));

        const N: usize = 20;
        for i in 0..N {
            let req = Envelope {
                payload: Some(envelope::Payload::ActionRequest(ActionRequest {
                    action_id: format!("action-{i}"),
                    action: "vec_upsert".into(),
                    params_json: serde_json::to_vec(&serde_json::json!({
                        "collection": "test",
                        "id": format!("doc:{i}"),
                        "text": format!("hello world {i}")
                    }))
                    .unwrap(),
                    timeout_ms: 0,
                    streaming: false,
                    caller_plugin_id: "caller_x".into(),
                })),
                ..Default::default()
            };
            kernel.send("vector-db", req).await.unwrap();
        }

        let mut seen = 0;
        let mut changed_events = 0usize;
        for _ in 0..(2 * N) {
            let env = tokio::time::timeout(Duration::from_secs(5), kernel.recv())
                .await
                .expect("timed out — loop likely deadlocked")
                .unwrap();
            match env.payload {
                Some(envelope::Payload::ActionResponse(resp)) => {
                    assert_eq!(resp.status, ActionStatus::ActionOk as i32, "error: {}", resp.error);
                    seen += 1;
                }
                Some(envelope::Payload::EventPublish(ev)) => {
                    assert_eq!(ev.event_type, "changed");
                    changed_events += 1;
                }
                other => panic!("unexpected payload: {other:?}"),
            }
        }
        assert_eq!(seen, N);
        assert_eq!(changed_events, N);

        let shutdown = Envelope {
            payload: Some(envelope::Payload::PluginShutdown(PluginShutdown {
                reason: "test done".into(),
                grace_seconds: 0,
            })),
            ..Default::default()
        };
        kernel.send("vector-db", shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), loop_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
