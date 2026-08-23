use launcher_plugin::config::{Config, DesktopLauncher};
use launcher_plugin::runner::RealLauncher;
use launcher_plugin::{LauncherPlugin, PLUGIN_ID};
use std::sync::Arc;
use vynkor_sdk::proto::{envelope, ActionRequest, ActionStatus, Envelope, PluginRegisterAck};
use vynkor_sdk::{Plugin, VynkorClient};

async fn spawn_plugin(kernel_side: tokio::net::UnixStream, cfg: Config) {
    let mut plugin = LauncherPlugin::new(cfg, Arc::new(RealLauncher));
    let client = VynkorClient::from_stream(kernel_side, None);
    tokio::spawn(async move { plugin.serve(client, "").await });
}

async fn handshake(client: &mut VynkorClient) {
    let reg = client.recv().await.expect("register frame");
    assert!(matches!(
        reg.payload,
        Some(envelope::Payload::PluginRegister(_))
    ));
    let ack = Envelope {
        payload: Some(envelope::Payload::PluginRegisterAck(PluginRegisterAck {
            accepted: true,
            ..Default::default()
        })),
        ..Default::default()
    };
    client.send(PLUGIN_ID, ack).await.expect("ack");
}

async fn call_action(
    client: &mut VynkorClient,
    action_id: &str,
    action: &str,
    params_json: &[u8],
) -> vynkor_sdk::proto::ActionResponse {
    let req = Envelope {
        payload: Some(envelope::Payload::ActionRequest(ActionRequest {
            action_id: action_id.to_string(),
            action: action.to_string(),
            params_json: params_json.to_vec(),
            ..Default::default()
        })),
        ..Default::default()
    };
    client.send(PLUGIN_ID, req).await.expect("send");
    loop {
        let env = client.recv().await.expect("reply");
        match env.payload {
            Some(envelope::Payload::ActionResponse(resp)) => return resp,
            Some(_) => continue,
            None => panic!("empty envelope"),
        }
    }
}

fn test_config_with_desktop(dir: &std::path::Path) -> Config {
    Config {
        desktop_dirs: vec![dir.to_path_buf()],
        steam_roots: vec![],
        allowed_ids: None,
        timeout_ms: 5000,
        steam_binary: "steam".to_string(),
        desktop_launcher: DesktopLauncher::Exec,
        cache_ttl_ms: 60_000,
        terminal: launcher_plugin::config::TerminalKind::None,
        tmux_session: "vynkor".to_string(),
    }
}

#[tokio::test]
async fn registration_then_launch_list_roundtrip() {
    let td = tempfile::tempdir().unwrap();
    std::fs::write(
        td.path().join("alpha.desktop"),
        "[Desktop Entry]\nName=Alpha\nExec=/bin/true\nType=Application\n",
    )
    .unwrap();
    let cfg = test_config_with_desktop(td.path());
    let (plugin_side, kernel_side) = tokio::net::UnixStream::pair().unwrap();
    spawn_plugin(plugin_side, cfg).await;
    let mut kernel = VynkorClient::from_stream(kernel_side, None);
    handshake(&mut kernel).await;
    let resp = call_action(
        &mut kernel,
        "t1",
        "launch_list",
        br#"{"provider":"desktop"}"#,
    )
    .await;
    assert_eq!(resp.status, ActionStatus::ActionOk as i32);
    let v: serde_json::Value = serde_json::from_slice(&resp.data_json).unwrap();
    assert_eq!(v["apps"].as_array().unwrap().len(), 1);
    assert_eq!(v["apps"][0]["id"], "alpha");
}

#[tokio::test]
async fn launch_dry_run_over_wire() {
    let td = tempfile::tempdir().unwrap();
    std::fs::write(
        td.path().join("myapp.desktop"),
        "[Desktop Entry]\nName=MyApp\nExec=/bin/true %U\nType=Application\n",
    )
    .unwrap();
    let cfg = test_config_with_desktop(td.path());
    let (plugin_side, kernel_side) = tokio::net::UnixStream::pair().unwrap();
    spawn_plugin(plugin_side, cfg).await;
    let mut kernel = VynkorClient::from_stream(kernel_side, None);
    handshake(&mut kernel).await;
    let resp = call_action(
        &mut kernel,
        "t2",
        "launch",
        br#"{"app_id":"myapp","dry_run":true}"#,
    )
    .await;
    assert_eq!(resp.status, ActionStatus::ActionOk as i32);
    let v: serde_json::Value = serde_json::from_slice(&resp.data_json).unwrap();
    assert_eq!(v["launched"], false);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["provider"], "desktop");
}

#[tokio::test]
async fn launch_not_found_over_wire() {
    let cfg = Config {
        desktop_dirs: vec![],
        steam_roots: vec![],
        allowed_ids: None,
        timeout_ms: 5000,
        steam_binary: "steam".to_string(),
        desktop_launcher: DesktopLauncher::Exec,
        cache_ttl_ms: 60_000,
        terminal: launcher_plugin::config::TerminalKind::None,
        tmux_session: "vynkor".to_string(),
    };
    let (plugin_side, kernel_side) = tokio::net::UnixStream::pair().unwrap();
    spawn_plugin(plugin_side, cfg).await;
    let mut kernel = VynkorClient::from_stream(kernel_side, None);
    handshake(&mut kernel).await;
    let resp = call_action(&mut kernel, "t3", "launch", br#"{"app_id":"nope"}"#).await;
    assert_eq!(resp.status, ActionStatus::ActionNotFound as i32);
    assert!(
        resp.error.contains("ERR_LAUNCH_NOT_FOUND"),
        "{}",
        resp.error
    );
}

#[tokio::test]
async fn bad_params_over_wire() {
    let td = tempfile::tempdir().unwrap();
    let cfg = test_config_with_desktop(td.path());
    let (plugin_side, kernel_side) = tokio::net::UnixStream::pair().unwrap();
    spawn_plugin(plugin_side, cfg).await;
    let mut kernel = VynkorClient::from_stream(kernel_side, None);
    handshake(&mut kernel).await;
    let resp = call_action(&mut kernel, "t4", "launch", br#"{"app_id":""}"#).await;
    assert_eq!(resp.status, ActionStatus::ActionError as i32);
    assert!(
        resp.error.contains("ERR_LAUNCH_BAD_PARAMS"),
        "{}",
        resp.error
    );
}

#[tokio::test]
async fn providers_over_wire() {
    let cfg = Config {
        desktop_dirs: vec![],
        steam_roots: vec![],
        allowed_ids: None,
        timeout_ms: 5000,
        steam_binary: "steam".to_string(),
        desktop_launcher: DesktopLauncher::Auto,
        cache_ttl_ms: 60_000,
        terminal: launcher_plugin::config::TerminalKind::None,
        tmux_session: "vynkor".to_string(),
    };
    let (plugin_side, kernel_side) = tokio::net::UnixStream::pair().unwrap();
    spawn_plugin(plugin_side, cfg).await;
    let mut kernel = VynkorClient::from_stream(kernel_side, None);
    handshake(&mut kernel).await;
    let resp = call_action(&mut kernel, "t5", "launch_providers", b"{}").await;
    assert_eq!(resp.status, ActionStatus::ActionOk as i32);
    let v: serde_json::Value = serde_json::from_slice(&resp.data_json).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 6);
}
