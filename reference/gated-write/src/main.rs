//! `gated-write` — reference high-risk plugin demonstrating the D-09
//! confirmation gate from the Rust SDK.
//!
//! The risky operation (writing a file into a configured data dir) is split
//! into two actions:
//!
//! - `request_write` — any registered caller may invoke; the action spec is
//!   marked `requires_confirmation` and the params are stored as pending;
//!   **nothing is written**.
//! - `confirm_write` — only callers on the confirm allowlist may invoke; it
//!   executes the write with the params stored at request time.
//!
//! The kernel stays dumb on purpose (a kernel gate would violate the
//! dumb-core rule): enforcement is entirely inside this plugin, keyed on the
//! kernel-stamped `caller_plugin_id` (the kernel overwrites it from the real
//! registered sender — it cannot be spoofed).
//!
//! Config (env):
//! - `GATED_WRITE_DATA_DIR` (required) — directory confirmed writes land in.
//! - `GATED_WRITE_CONFIRM_CALLERS` (default `device.*`) — comma-separated
//!   plugin ids (or `prefix.*` globs) allowed to confirm.
//!
//! This is the sequential [`Plugin`] pattern — `network` demonstrates the
//! same gate with the concurrent loop (`ConcurrentHandler`).

use std::path::PathBuf;

use gated_write_plugin::handler;
use vynkor_sdk::confirmation_gate::ConfirmationGate;
use vynkor_sdk::proto::{envelope, ActionRisk, Envelope, PluginManifest};
use vynkor_sdk::{Plugin, VynkorError};

const DATA_DIR_ENV: &str = "GATED_WRITE_DATA_DIR";
const CONFIRM_CALLERS_ENV: &str = "GATED_WRITE_CONFIRM_CALLERS";
const DEFAULT_CONFIRM_CALLERS: &str = "device.*";

/// JSON Schema of `request_write` params, served to the AI (D-08) as the
/// `request_write` tool spec.
const WRITE_PARAMS_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "path": { "type": "string", "description": "Path relative to the data dir (no '..', not absolute)." },
    "content": { "type": "string", "description": "Text to write." },
    "mode": { "type": "string", "enum": ["append", "overwrite"], "default": "overwrite" }
  },
  "required": ["path"]
}"#;

struct GatedWritePlugin {
    gate: ConfirmationGate,
    data_dir: PathBuf,
}

impl GatedWritePlugin {
    fn new() -> Self {
        let data_dir = std::env::var(DATA_DIR_ENV)
            .unwrap_or_else(|_| panic!("{DATA_DIR_ENV} must be set (see config.example.yaml)"));
        let data_dir = PathBuf::from(&data_dir);
        std::fs::create_dir_all(&data_dir)
            .unwrap_or_else(|e| panic!("failed to create {DATA_DIR_ENV} ({data_dir:?}): {e}"));

        let callers =
            std::env::var(CONFIRM_CALLERS_ENV).unwrap_or_else(|_| DEFAULT_CONFIRM_CALLERS.into());
        let confirm_callers: Vec<&str> = callers
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();

        let gate = ConfirmationGate::new(
            "write",
            "Write a file into the gated-write data dir",
            WRITE_PARAMS_SCHEMA,
            ActionRisk::High,
            &confirm_callers,
        )
        .expect("static gate config is valid");
        Self { gate, data_dir }
    }

    /// The confirmed half: parse + write + encode the result. Runs only
    /// after `ConfirmationGate::route` has checked the caller and resolved a
    /// pending request.
    fn do_write(data_dir: &std::path::Path, params: Vec<u8>) -> Result<Vec<u8>, String> {
        let write = handler::parse_write_params(&params)?;
        let (path, bytes) = handler::execute_write(data_dir, &write)?;
        serde_json::to_vec(&serde_json::json!({
            "path": path.display().to_string(),
            "bytes_written": bytes,
            "mode": if write.append { "append" } else { "overwrite" },
        }))
        .map_err(|e| format!("failed to encode result: {e}"))
    }
}

impl Plugin for GatedWritePlugin {
    fn id(&self) -> &str {
        "gated-write"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn manifest(&self) -> PluginManifest {
        let (actions, action_specs) = self.gate.manifest_entries();
        PluginManifest {
            permissions: vec!["PERMISSION_FILES_WRITE".into()],
            actions,
            action_specs,
            ..Default::default()
        }
    }

    async fn on_message(&mut self, env: Envelope) -> Result<Option<Envelope>, VynkorError> {
        match env.payload {
            Some(envelope::Payload::ActionRequest(req)) => {
                // The executor runs with the params stored at request time —
                // the confirming caller cannot swap in different content.
                let data_dir = self.data_dir.clone();
                let envelopes = self
                    .gate
                    .route(req, move |params| {
                        let data_dir = data_dir.clone();
                        async move { Self::do_write(&data_dir, params) }
                    })
                    .await;
                Ok(envelopes.into_iter().next())
            }
            other => {
                println!("[gated-write] unhandled message: {other:?}");
                Ok(None)
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), VynkorError> {
    let mut plugin = GatedWritePlugin::new();
    plugin.run().await?;
    println!("[gated-write] shutting down");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vynkor_sdk::proto::{ActionRequest, ActionResponse, ActionStatus};

    fn temp_data_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gated-write-plugin-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_plugin(data_dir: PathBuf) -> GatedWritePlugin {
        let gate = ConfirmationGate::new(
            "write",
            "Write a file",
            WRITE_PARAMS_SCHEMA,
            ActionRisk::High,
            &["device.phone"],
        )
        .unwrap();
        GatedWritePlugin { gate, data_dir }
    }

    fn action_request(action: &str, action_id: &str, caller: &str, params_json: &[u8]) -> Envelope {
        Envelope {
            payload: Some(envelope::Payload::ActionRequest(ActionRequest {
                action_id: action_id.to_string(),
                action: action.to_string(),
                params_json: params_json.to_vec(),
                timeout_ms: 0,
                streaming: false,
                caller_plugin_id: caller.to_string(),
            })),
            ..Default::default()
        }
    }

    async fn call(plugin: &mut GatedWritePlugin, env: Envelope) -> ActionResponse {
        let reply = plugin
            .on_message(env)
            .await
            .unwrap()
            .expect("expected a reply");
        match reply.payload {
            Some(envelope::Payload::ActionResponse(resp)) => resp,
            other => panic!("expected ActionResponse, got {other:?}"),
        }
    }

    fn pending_id(resp: &ActionResponse) -> String {
        let v: serde_json::Value = serde_json::from_slice(&resp.data_json).unwrap();
        v["pending_id"].as_str().unwrap().to_string()
    }

    /// The full D-09 acceptance flow: the AI can request but cannot confirm;
    /// the user's device confirms and the write executes with the request-time
    /// params.
    #[tokio::test]
    async fn ai_cannot_confirm_but_user_device_can() {
        let dir = temp_data_dir("flow");
        let mut plugin = test_plugin(dir.clone());

        // AI requests the write.
        let resp = call(
            &mut plugin,
            action_request(
                "request_write",
                "r1",
                "ai",
                br#"{"path": "notes.txt", "content": "secret plan"}"#,
            ),
        )
        .await;
        assert_eq!(resp.status, ActionStatus::ActionOk as i32);
        let pid = pending_id(&resp);
        assert!(!dir.join("notes.txt").exists(), "request must not write");

        // The AI knows the real pending id — still denied.
        let confirm_params = format!(r#"{{"pending_id": "{pid}"}}"#);
        let resp = call(
            &mut plugin,
            action_request("confirm_write", "c1", "ai", confirm_params.as_bytes()),
        )
        .await;
        assert_eq!(resp.status, ActionStatus::ActionError as i32);
        assert!(
            resp.error.contains("permission denied"),
            "error was: {}",
            resp.error
        );
        assert!(
            !dir.join("notes.txt").exists(),
            "denied confirm must not write"
        );

        // The user's device confirms — the write executes with the content
        // stored at request time.
        let resp = call(
            &mut plugin,
            action_request(
                "confirm_write",
                "c2",
                "device.phone",
                confirm_params.as_bytes(),
            ),
        )
        .await;
        assert_eq!(resp.status, ActionStatus::ActionOk as i32);
        let v: serde_json::Value = serde_json::from_slice(&resp.data_json).unwrap();
        assert_eq!(v["bytes_written"], 11);
        assert_eq!(
            std::fs::read_to_string(dir.join("notes.txt")).unwrap(),
            "secret plan"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A confirmed write for a path that escaped the data dir is refused by
    /// the executor (the write half, not the gate).
    #[tokio::test]
    async fn path_traversal_is_refused_at_confirm_time() {
        let dir = temp_data_dir("traversal");
        let mut plugin = test_plugin(dir.clone());

        let resp = call(
            &mut plugin,
            action_request(
                "request_write",
                "r1",
                "ai",
                br#"{"path": "../evil.txt", "content": "pwned"}"#,
            ),
        )
        .await;
        let pid = pending_id(&resp);

        let confirm_params = format!(r#"{{"pending_id": "{pid}"}}"#);
        let resp = call(
            &mut plugin,
            action_request(
                "confirm_write",
                "c1",
                "device.phone",
                confirm_params.as_bytes(),
            ),
        )
        .await;
        assert_eq!(resp.status, ActionStatus::ActionError as i32);
        assert!(resp.error.contains("'..'"), "error was: {}", resp.error);
        assert!(
            !dir.parent().unwrap().join("evil.txt").exists(),
            "nothing written outside the data dir"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The manifest carries the confirmation metadata: `request_write` is
    /// marked `requires_confirmation` with HIGH risk, `confirm_write` is not.
    #[tokio::test]
    async fn manifest_marks_request_write_as_requires_confirmation() {
        let plugin = test_plugin(temp_data_dir("manifest"));
        let manifest = plugin.manifest();
        assert_eq!(
            manifest.actions,
            vec!["request_write".to_string(), "confirm_write".to_string()]
        );
        assert!(manifest
            .permissions
            .contains(&"PERMISSION_FILES_WRITE".to_string()));

        let request_spec = manifest
            .action_specs
            .iter()
            .find(|s| s.name == "request_write")
            .expect("request_write spec present");
        assert!(request_spec.requires_confirmation);
        assert_eq!(request_spec.risk, ActionRisk::High as i32);
        assert!(request_spec.params_schema.contains("path"));

        let confirm_spec = manifest
            .action_specs
            .iter()
            .find(|s| s.name == "confirm_write")
            .expect("confirm_write spec present");
        assert!(!confirm_spec.requires_confirmation);
    }
}
