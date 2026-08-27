//! vyn ask — terminal client to the `agent` plugin's goal loop.
//!
//! One-shot: `vyn ask "why did CI fail"` → goal_start → prints answer.
//! Stdin: `git diff | vyn ask "write commit message"` — stdin appended to prompt.
//! REPL: `vyn ask` (no args) → readline loop.
//! History: `~/.local/share/vyn/ask_history`

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::Instant;
use vynkor_sdk::proto::{Envelope, PluginManifest};
use vynkor_sdk::VynkorClient;

const PLUGIN_ID: &str = "vyn-ask";

fn history_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME").ok().map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".local/share")
    });
    base.join("vyn/ask_history")
}

fn read_stdin_if_piped() -> Option<String> {
    // If stdin is piped (not a tty), read it. Use is_terminal check.
    let is_tty = libc_is_tty(0);
    if is_tty { return None; }
    let mut buf = String::new();
    let mut stdin = io::stdin();
    if stdin.read_to_string(&mut buf).is_ok() {
        let t = buf.trim().to_string();
        if t.is_empty() { None } else { Some(t) }
    } else { None }
}

#[cfg(unix)]
fn libc_is_tty(fd: i32) -> bool {
    unsafe { libc::isatty(fd) != 0 }
}
#[cfg(not(unix))]
fn libc_is_tty(_fd: i32) -> bool { false }

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let goal_arg = args.get(1).cloned();
    let stdin_extra = read_stdin_if_piped();

    if goal_arg.is_none() && stdin_extra.is_none() {
        // REPL mode
        repl().await;
        return;
    }

    let goal = match (goal_arg, stdin_extra) {
        (Some(g), Some(s)) => format!("{g}\n\n--- stdin ---\n{s}"),
        (Some(g), None) => g,
        (None, Some(s)) => s,
        (None, None) => unreachable!(),
    };

    if let Err(e) = run_goal(&goal).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn repl() {
    println!("vyn ask — REPL to agent (Ctrl-D to exit)");
    println!(" stdin piped content is appended automatically: `git diff | vyn ask \"explain\"`");
    let mut line = String::new();
    loop {
        print!("> ");
        io::stdout().flush().unwrap();
        line.clear();
        if io::stdin().read_line(&mut line).unwrap() == 0 { break; }
        let goal = line.trim().to_string();
        if goal.is_empty() { continue; }
        if goal == "exit" || goal == "quit" { break; }
        let full = goal;
        append_history(&full);
        if let Err(e) = run_goal(&full).await {
            eprintln!("error: {e}");
        }
    }
}

fn append_history(goal: &str) {
    let path = history_path();
    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
    if let Some(ref mut file) = f {
        let _ = writeln!(file, "{}", goal);
    }
}

async fn run_goal(goal: &str) -> Result<(), String> {
    let start = Instant::now();
    let mut client = VynkorClient::connect_from_env().await.map_err(|e| format!("connect: {e}"))?;
    let token = std::env::var("VYN_JWT_TOKEN").unwrap_or_default();
    let ack = client.register_full(PLUGIN_ID, "0.1.0", PluginManifest::default(), &token).await.map_err(|e| format!("register: {e}"))?;
    if !ack.accepted { return Err(format!("registration rejected: {}", ack.reject_reason)); }

    let params = serde_json::json!({"goal": goal});
    let action_id = format!("ask-{}", uuid::Uuid::new_v4().simple());
    client.send("kernel", Envelope {
        payload: Some(vynkor_sdk::proto::envelope::Payload::ActionRequest(vynkor_sdk::proto::ActionRequest {
            action_id: action_id.clone(),
            action: "goal_start".into(),
            params_json: serde_json::to_vec(&params).unwrap(),
            timeout_ms: 120_000,
            streaming: false,
            ..Default::default()
        })),
        ..Default::default()
    }).await.map_err(|e| format!("send: {e}"))?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() { return Err("timed out after 120s".into()); }
        let env = client.recv_timeout(remaining).await.map_err(|e| format!("recv: {e}"))?;
        match env.payload {
            Some(vynkor_sdk::proto::envelope::Payload::ActionResponse(resp)) if resp.action_id == action_id => {
                eprintln!("elapsed: {} ms | status={} error={:?}", start.elapsed().as_millis(), resp.status, resp.error);
                if resp.status == vynkor_sdk::proto::ActionStatus::ActionOk as i32 {
                    let v: serde_json::Value = serde_json::from_slice(&resp.data_json).unwrap_or(serde_json::Value::Null);
                    // agent returns {goal_id, status, answer, steps?}
                    if let Some(answer) = v.get("answer").and_then(|a| a.as_str()) {
                        println!("{}", answer);
                    } else if let Some(text) = v.get("output").and_then(|a| a.as_str()) {
                        println!("{}", text);
                    } else {
                        println!("{}", serde_json::to_string_pretty(&v).unwrap());
                    }
                    return Ok(());
                } else {
                    return Err(resp.error);
                }
            }
            Some(vynkor_sdk::proto::envelope::Payload::Error(e)) => return Err(format!("kernel error: {e:?}")),
            _ => continue,
        }
    }
}

mod libc {
    #[cfg(unix)]
    extern "C" { pub fn isatty(fd: i32) -> i32; }
}
