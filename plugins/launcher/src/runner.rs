use async_trait::async_trait;
use std::time::Duration;
use tokio::process::Command;

#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, bin: &str, args: &[String], timeout_ms: u64) -> Result<String, String>;
}

pub struct RealRunner;

#[async_trait]
impl Runner for RealRunner {
    async fn run(&self, bin: &str, args: &[String], timeout_ms: u64) -> Result<String, String> {
        let mut cmd = Command::new(bin);
        cmd.args(args).kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("ERR_LAUNCH_PROVIDER_MISSING: binary '{bin}' not found on PATH")
            } else {
                format!("ERR_LAUNCH_SPAWN_FAILED: spawn '{bin}' failed: {e}")
            }
        })?;
        let res = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await;
        match res {
            Err(_) => {
                let _ = child.kill().await;
                Err(format!(
                    "ERR_LAUNCH_TIMEOUT: '{bin}' exceeded {timeout_ms}ms"
                ))
            }
            Ok(Err(e)) => Err(format!("ERR_LAUNCH_SPAWN_FAILED: wait '{bin}' failed: {e}")),
            Ok(Ok(status)) => {
                if !status.success() {
                    Err(format!(
                        "ERR_LAUNCH_SPAWN_FAILED: '{bin}' exited with {status}"
                    ))
                } else {
                    Ok(String::new())
                }
            }
        }
    }
}

/// For launching, we detach: spawn and return immediately without waiting for exit.
/// But for dry testing we can keep wait. The real launch uses fire-and-forget spawn.

#[async_trait]
pub trait Launcher: Send + Sync {
    async fn spawn_detached(&self, bin: &str, args: &[String]) -> Result<(), String>;
}

pub struct RealLauncher;

#[async_trait]
impl Launcher for RealLauncher {
    async fn spawn_detached(&self, bin: &str, args: &[String]) -> Result<(), String> {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(false);
        cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("ERR_LAUNCH_PROVIDER_MISSING: binary '{bin}' not found on PATH")
            } else {
                format!("ERR_LAUNCH_SPAWN_FAILED: spawn '{bin}' failed: {e}")
            }
        })?;
        Ok(())
    }
}

#[cfg(test)]
pub struct FakeLauncher {
    pub calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    pub result: Result<(), String>,
}

#[cfg(test)]
impl FakeLauncher {
    pub fn new_ok() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            result: Ok(()),
        }
    }
    pub fn new_err(msg: &str) -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            result: Err(msg.to_string()),
        }
    }
}

#[cfg(test)]
#[async_trait]
impl Launcher for FakeLauncher {
    async fn spawn_detached(&self, bin: &str, args: &[String]) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push((bin.to_string(), args.to_vec()));
        self.result.clone()
    }
}

#[cfg(test)]
pub struct FakeRunner {
    pub calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
}

#[cfg(test)]
#[async_trait]
impl Runner for FakeRunner {
    async fn run(&self, bin: &str, args: &[String], _timeout_ms: u64) -> Result<String, String> {
        self.calls
            .lock()
            .unwrap()
            .push((bin.to_string(), args.to_vec()));
        Ok(String::new())
    }
}

pub fn binary_in_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(name);
                candidate.is_file() && is_executable(&candidate)
            })
        })
        .unwrap_or(false)
}

fn is_executable(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}
