pub mod db;
pub mod embed;
pub mod handler;
pub mod request;

use vynkor_sdk::concurrent::response_envelope;
use vynkor_sdk::proto::{envelope, ActionRequest, Envelope, EventPublish, PluginManifest};
use vynkor_sdk::ConcurrentHandler;

use handler::Handler;

impl ConcurrentHandler for Handler {
    fn id(&self) -> &str {
        "vector-db"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            permissions: vec![
                "PERMISSION_STORAGE".into(),
                "PERMISSION_EVENT_PUBLISH".into(),
            ],
            actions: vec![
                "vec_upsert".into(),
                "vec_query".into(),
                "vec_get".into(),
                "vec_delete".into(),
                "vec_list".into(),
                "vec_stats".into(),
            ],
            ..Default::default()
        }
    }

    async fn on_action(&self, req: ActionRequest) -> Vec<Envelope> {
        let mut envelopes = Vec::new();
        match self
            .handle(&req.caller_plugin_id, &req.action, &req.params_json)
            .await
        {
            Ok(result) => {
                envelopes.push(response_envelope(req.action_id, Ok(serde_json::to_vec(&result).unwrap())));
                // best-effort changed event for upsert/delete
                if req.action == "vec_upsert" || req.action == "vec_delete" {
                    let payload = serde_json::json!({
                        "caller": req.caller_plugin_id,
                        "action": req.action,
                    })
                    .to_string()
                    .into_bytes();
                    envelopes.push(Envelope {
                        payload: Some(envelope::Payload::EventPublish(EventPublish {
                            event_type: "changed".into(),
                            payload_json: payload,
                        })),
                        ..Default::default()
                    });
                }
            }
            Err(error) => {
                envelopes.push(response_envelope(req.action_id, Err(error)));
            }
        }
        envelopes
    }
}
