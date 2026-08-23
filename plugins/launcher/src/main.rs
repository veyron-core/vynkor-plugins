use launcher_plugin::config::Config;
use launcher_plugin::runner::RealLauncher;
use launcher_plugin::{LauncherPlugin, PLUGIN_ID};
use std::sync::Arc;
use vynkor_sdk::{Plugin, VynkorError};

#[tokio::main]
async fn main() -> Result<(), VynkorError> {
    let config = Config::from_env();
    eprintln!(
        "[{PLUGIN_ID}] desktop_dirs={:?} steam_roots={:?} timeout={} cache_ttl={} steam_bin={} launcher={}",
        config.desktop_dirs,
        config.steam_roots,
        config.timeout_ms,
        config.cache_ttl_ms,
        config.steam_binary,
        config.desktop_launcher.as_str()
    );
    let launcher = Arc::new(RealLauncher);
    let mut plugin = LauncherPlugin::new(config, launcher);
    plugin.run().await
}
