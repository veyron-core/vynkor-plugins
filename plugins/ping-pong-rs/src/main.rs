use vynkor_sdk::proto::{envelope, ActionResponse, ActionStatus, Envelope, PluginManifest};
use vynkor_sdk::VynkorError;
use vynkor_sdk::{Plugin, VynkorClient};

/// Minimal reference plugin: replies "pong" to a "ping" action, rejects anything else.
struct PingPongPlugin;

impl Plugin for PingPongPlugin {
    fn id(&self) -> &str {
        "ping-pong"
    }

    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            actions: vec!["ping".to_string()],
            ..Default::default()
        }
    }

    fn version(&self) -> &str {
        "0.2.0"
    }

    async fn on_init(&mut self, _client: &mut VynkorClient) -> Result<(), VynkorError> {
        Ok(())
    }

    async fn on_message(&mut self, envelope: Envelope) -> Result<Option<Envelope>, VynkorError> {
        match envelope.payload {
            Some(envelope::Payload::ActionRequest(req)) if req.action == "ping" => {
                let response = Envelope {
                    payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                        action_id: req.action_id,
                        status: ActionStatus::ActionOk as i32,
                        data_json: br#"{"reply":"pong"}"#.to_vec(),
                        error: String::new(),
                    })),
                    ..Default::default()
                };
                Ok(Some(response))
            }
            Some(envelope::Payload::ActionRequest(req)) => {
                let response = Envelope {
                    payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                        action_id: req.action_id,
                        status: ActionStatus::ActionNotFound as i32,
                        data_json: Vec::new(),
                        error: format!("ping-pong only handles 'ping', got '{}'", req.action),
                    })),
                    ..Default::default()
                };
                Ok(Some(response))
            }
            _ => Ok(None),
        }
    }

    async fn on_shutdown(&mut self) -> Result<(), VynkorError> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), VynkorError> {
    PingPongPlugin.run().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ping_request(action: &str) -> Envelope {
        Envelope {
            payload: Some(envelope::Payload::ActionRequest(
                vynkor_sdk::proto::ActionRequest {
                    action_id: "test-1".to_string(),
                    action: action.to_string(),
                    ..Default::default()
                },
            )),
            ..Default::default()
        }
    }

    /// Regression guard for live-audit defect #3: a pre-M9 binary answered
    /// with `status=0` because ACTION_OK used to be 0; since M9 zero means
    /// ACTION_UNKNOWN and must never be sent as a success status.
    #[tokio::test]
    async fn pong_replies_action_ok_not_zero() {
        let reply = PingPongPlugin
            .on_message(ping_request("ping"))
            .await
            .unwrap()
            .expect("ping must be answered");
        let Some(envelope::Payload::ActionResponse(resp)) = reply.payload else {
            panic!("expected ActionResponse payload");
        };
        assert_eq!(resp.status, ActionStatus::ActionOk as i32);
        assert_ne!(resp.status, 0, "ACTION_OK is non-zero since M9");
        assert_eq!(std::str::from_utf8(&resp.data_json).unwrap(), r#"{"reply":"pong"}"#);
    }

    #[tokio::test]
    async fn unknown_action_replies_not_found() {
        let reply = PingPongPlugin
            .on_message(ping_request("nope"))
            .await
            .unwrap()
            .expect("any ActionRequest must be answered");
        let Some(envelope::Payload::ActionResponse(resp)) = reply.payload else {
            panic!("expected ActionResponse payload");
        };
        assert_eq!(resp.status, ActionStatus::ActionNotFound as i32);
    }
}
