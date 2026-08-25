//! vyn-act <action> [params_json|@file] [timeout_ms] — call a kernel-routed
//! action from the CLI and print the response data_json. Credentials come
//! from VYN_JWT_TOKEN (+ VYN_JWT_SECRET for frame MACs); `@path` reads the
//! params from a file so big payloads (audio_base64) skip ARG_MAX.
use std::time::Instant;
use vynkor_sdk::proto::{Envelope, PluginManifest};
use vynkor_sdk::VynkorClient;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: vyn-act <action> [params_json|@file] [timeout_ms]");
        std::process::exit(2);
    }
    let mut client = VynkorClient::connect_from_env()
        .await
        .expect("connect to kernel socket");
    let token = std::env::var("VYN_JWT_TOKEN").unwrap_or_default();
    let ack = client
        .register_full("vyn-act", "0.1.0", PluginManifest::default(), &token)
        .await
        .expect("register");
    if !ack.accepted {
        eprintln!("registration rejected: {}", ack.reject_reason);
        std::process::exit(1);
    }

    let action = args[1].clone();
    let raw = args.get(2).cloned().unwrap_or_else(|| "{}".into());
    let params = match raw.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path).expect("read params file"),
        None => raw,
    };
    let timeout: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(45_000);

    let started = Instant::now();
    let action_id = "act-cli".to_string();
    client
        .send(
            "kernel",
            Envelope {
                payload: Some(vynkor_sdk::proto::envelope::Payload::ActionRequest(
                    vynkor_sdk::proto::ActionRequest {
                        action_id: action_id.clone(),
                        action: action.clone(),
                        params_json: params.into_bytes(),
                        timeout_ms: timeout,
                        streaming: false,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
        )
        .await
        .expect("send request");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout as u64);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            eprintln!("error: timed out after {} ms", timeout);
            std::process::exit(1);
        }
        let env = match client.recv_timeout(remaining).await {
            Ok(e) => e,
            Err(e) => {
                eprintln!("error: recv: {e}");
                std::process::exit(1);
            }
        };
        match env.payload {
            Some(vynkor_sdk::proto::envelope::Payload::ActionResponse(resp)) => {
                eprintln!(
                    "elapsed: {} ms | status={} error={:?}",
                    started.elapsed().as_millis(),
                    resp.status,
                    resp.error
                );
                println!("{}", String::from_utf8_lossy(&resp.data_json));
                return;
            }
            // Refusals arrive as a bare Error envelope — surface instead of
            // silently looping to the timeout.
            Some(vynkor_sdk::proto::envelope::Payload::Error(err)) => {
                eprintln!("kernel error: {err:?}");
                std::process::exit(1);
            }
            _ => continue,
        }
    }
}
