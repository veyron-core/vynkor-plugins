//! `ai` plugin — provider-agnostic chat completion for other plugins, routed
//! through `network`'s `http_request` action rather than opening its own
//! sockets (see ROADMAP.md, "Decision: reuse `network`, don't reinvent").
//!
//! v0.3 adds a SQLite store (`VYN_DATA_DIR/ai.db`) holding declared +
//! auto-discovered models, agent profiles, and per-call token usage.
//!
//! Doesn't use the SDK's `Plugin::run`/`serve` loop: `Plugin::on_message`
//! only gets `&mut self`, not `&mut VynkorClient`, and there is no way to
//! get a second client for the outbound `send_action` call into `network`
//! — the kernel rejects a second connection under the same `plugin_id`
//! (`vynkor/src/plugins/registry.rs`) and rejects any traffic from an
//! unregistered connection (`vynkor/src/ipc/protocol.rs`). So this plugin
//! drives its own loop, near-identical to the SDK's `serve()`, but calls
//! the `chat_completion` handler with the loop's own `&mut VynkorClient` in
//! hand. Sequential, one request at a time — same model `network` and
//! `ping-pong-rs` already use.

use std::path::PathBuf;
use std::sync::Arc;

use ai_plugin::{config, db, discovery, handler};
use vynkor_sdk::proto::{envelope, ActionResponse, ActionStatus, Envelope, PluginManifest, Pong};
use vynkor_sdk::{VynkorClient, VynkorError};

const PLUGIN_ID: &str = "ai";
const PLUGIN_VERSION: &str = "0.1.1";

/// Startup model-refresh retries: the first attempts typically race the
/// `network` plugin's registration (`ActionNotFound`).
const STARTUP_REFRESH_ATTEMPTS: u32 = 5;

fn manifest() -> PluginManifest {
    PluginManifest {
        // `network`: ai invokes `network`'s gated `http_request` action, and
        // `secrets`: ai resolves provider keys from the secrets vault first
        // (`secret_get`, gated by PERMISSION_SECRETS). T-19 requires callers
        // of a gated action to hold its permission too (matches plugin.json
        // `permissions`; Manifest v2 per-action model).
        permissions: vec!["PERMISSION_NETWORK".into(), "PERMISSION_SECRETS".into()],
        actions: vec![
            "chat_completion".to_string(),
            "embedding".to_string(),
            "list_models".to_string(),
            "list_agents".to_string(),
            "refresh_models".to_string(),
            "usage_stats".to_string(),
        ],
        ..Default::default()
    }
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn handle_action_request(
    client: &mut VynkorClient,
    req: vynkor_sdk::proto::ActionRequest,
    db: &db::AiDb,
    cfg: &config::AiConfig,
) -> Envelope {
    let outcome = match req.action.as_str() {
        "chat_completion" => handler::handle_chat_completion(client, &req.params_json, db).await,
        "embedding" => handler::handle_embedding(client, &req.params_json, db).await,
        "list_models" => handler::handle_list_models(db),
        "list_agents" => handler::handle_list_agents(db),
        "usage_stats" => handler::handle_usage_stats(db),
        "refresh_models" => handler::handle_refresh_models(client, db, &cfg.discovery).await,
        other => {
            return Envelope {
                payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                    action_id: req.action_id,
                    status: ActionStatus::ActionNotFound as i32,
                    data_json: Vec::new(),
                    error: format!("unknown action: {other}"),
                })),
                ..Default::default()
            };
        }
    };
    let reply = match outcome {
        Ok(data_json) => ActionResponse {
            action_id: req.action_id,
            status: ActionStatus::ActionOk as i32,
            data_json,
            error: String::new(),
        },
        Err(error) => ActionResponse {
            action_id: req.action_id,
            status: ActionStatus::ActionError as i32,
            data_json: Vec::new(),
            error,
        },
    };
    Envelope {
        payload: Some(envelope::Payload::ActionResponse(reply)),
        ..Default::default()
    }
}

async fn serve(
    mut client: VynkorClient,
    db: Arc<db::AiDb>,
    cfg: config::AiConfig,
) -> Result<(), VynkorError> {
    let jwt_token = std::env::var("VYN_JWT_TOKEN").unwrap_or_default();
    let ack = client
        .register_full(PLUGIN_ID, PLUGIN_VERSION, manifest(), &jwt_token)
        .await?;
    if !ack.accepted {
        return Err(VynkorError::PermissionDenied(format!(
            "registration rejected: {}",
            ack.reject_reason
        )));
    }
    println!("[{PLUGIN_ID}] registered with kernel");

    if let Err(e) = config::seed(&db, &cfg) {
        eprintln!("[{PLUGIN_ID}] failed to seed config: {e}");
    }

    if !cfg.discovery.is_empty() {
        for _ in 0..STARTUP_REFRESH_ATTEMPTS {
            match handler::handle_refresh_models(&mut client, &db, &cfg.discovery).await {
                Ok(data) => match serde_json::from_slice::<discovery::Discovered>(&data) {
                    Ok(d) => {
                        println!(
                            "[{PLUGIN_ID}] models refreshed: {} new, {} updated",
                            d.discovered, d.updated
                        );
                        for e in &d.errors {
                            eprintln!("[{PLUGIN_ID}] discovery error: {e}");
                        }
                        if d.errors.is_empty() {
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("[{PLUGIN_ID}] failed to decode refresh result: {e}");
                        break;
                    }
                },
                Err(e) => eprintln!("[{PLUGIN_ID}] initial model refresh failed: {e}"),
            }
            // The startup refresh races the `network` plugin's registration
            // (ActionNotFound) — back off and retry rather than dying on it.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    // Tie the default model to the default agent's model when no model
    // default is configured explicitly (fresh installs land on the
    // operator's chosen default agent instead of the first alphabetical id).
    if db.default_model().map(|m| m.is_none()).unwrap_or(true) {
        if let Ok(Some(agent)) = db.default_agent() {
            if db
                .get_model(&agent.model_id)
                .map(|m| m.is_some())
                .unwrap_or(false)
            {
                let _ = db.set_model_default(&agent.model_id);
            }
        }
    }

    loop {
        let env = match client.recv().await {
            Ok(env) => env,
            Err(_) => break, // disconnect / EOF
        };
        match env.payload {
            Some(envelope::Payload::Ping(ping)) => {
                let pong = Envelope {
                    payload: Some(envelope::Payload::Pong(Pong {
                        original_timestamp: ping.timestamp,
                        server_timestamp: unix_millis(),
                    })),
                    ..Default::default()
                };
                let _ = client.send("kernel", pong).await;
            }
            Some(envelope::Payload::PluginShutdown(_)) => break,
            Some(envelope::Payload::Event(event)) => {
                // ai declares no event subscriptions; ack defensively so the
                // kernel doesn't retry anything unexpectedly delivered.
                let _ = client.ack_event(&event.event_id).await;
            }
            Some(envelope::Payload::ActionRequest(req)) => {
                let resp = handle_action_request(&mut client, req, &db, &cfg).await;
                let _ = client.send("kernel", resp).await;
            }
            other => {
                println!("[{PLUGIN_ID}] unhandled message: {other:?}");
            }
        }
    }

    println!("[{PLUGIN_ID}] shutting down");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), VynkorError> {
    let socket_path = std::env::var("VYN_SOCKET_PATH")
        .unwrap_or_else(|_| vynkor_wire::socket::default_socket_path());
    let secret = std::env::var("VYN_JWT_SECRET")
        .ok()
        .filter(|s| !s.is_empty());
    let client = match secret {
        Some(s) => VynkorClient::connect_with_secret(&socket_path, s.as_bytes()).await?,
        None => VynkorClient::connect(&socket_path).await?,
    };

    let data_dir = std::env::var_os("VYN_DATA_DIR").map(PathBuf::from);
    let db = match db::AiDb::open(data_dir.as_deref()) {
        Ok(db) => Arc::new(db),
        Err(e) => {
            eprintln!("[{PLUGIN_ID}] cannot open database: {e}");
            std::process::exit(1);
        }
    };
    let cfg = config::from_env();

    serve(client, db, cfg).await
}
