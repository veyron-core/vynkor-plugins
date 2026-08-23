use std::path::PathBuf;

use crate::db::DbConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedFallback {
    Fake,
    Error,
}

impl EmbedFallback {
    fn from_env() -> Self {
        match std::env::var("VECTOR_DB_EMBED_FALLBACK")
            .unwrap_or_else(|_| "error".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "fake" => Self::Fake,
            _ => Self::Error,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmbedConfig {
    pub enabled: bool,
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
    pub timeout_ms: u32,
    pub fallback: EmbedFallback,
}

impl EmbedConfig {
    pub fn from_env() -> Self {
        let model = std::env::var("VECTOR_DB_EMBED_MODEL").unwrap_or_default();
        let enabled = !model.trim().is_empty();
        let provider = std::env::var("VECTOR_DB_EMBED_PROVIDER").unwrap_or_else(|_| "openai".to_string());
        let base_url = std::env::var("VECTOR_DB_EMBED_BASE_URL").unwrap_or_default();
        let api_key_env = std::env::var("VECTOR_DB_EMBED_API_KEY_ENV").unwrap_or_default();
        let timeout_ms = std::env::var("VECTOR_DB_EMBED_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10000);
        let fallback = EmbedFallback::from_env();
        Self {
            enabled,
            provider,
            base_url,
            model,
            api_key_env,
            timeout_ms,
            fallback,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub db: DbConfig,
    pub max_response_bytes: usize,
    pub default_dim: usize,
    pub embed: EmbedConfig,
}

impl Config {
    pub fn from_env() -> Self {
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
        let max_response_bytes = std::env::var("VECTOR_DB_MAX_RESPONSE_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4 * 1024 * 1024);
        let default_dim = std::env::var("VECTOR_DB_DEFAULT_DIM")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(384);
        Self {
            db: DbConfig {
                data_dir: PathBuf::from(data_dir),
                pool_size,
                busy_timeout_ms,
                max_db_bytes,
            },
            max_response_bytes,
            default_dim,
            embed: EmbedConfig::from_env(),
        }
    }
}
