use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct LaunchRequest {
    pub app_id: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct LaunchListRequest {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub include_hidden: Option<bool>,
}

pub fn parse_launch(params: &[u8]) -> Result<(String, String, Vec<String>, bool), String> {
    if params.is_empty() {
        return Err("ERR_LAUNCH_BAD_PARAMS: missing params_json".to_string());
    }
    let req: LaunchRequest = serde_json::from_slice(params)
        .map_err(|e| format!("ERR_LAUNCH_BAD_PARAMS: invalid params_json: {e}"))?;
    let app_id = req.app_id.trim().to_string();
    if app_id.is_empty() {
        return Err("ERR_LAUNCH_BAD_PARAMS: `app_id` must be non-empty".to_string());
    }
    if app_id.len() > 256 {
        return Err("ERR_LAUNCH_BAD_PARAMS: `app_id` too long (max 256)".to_string());
    }
    let provider = req.provider.unwrap_or_else(|| "auto".to_string());
    let prov = provider.trim().to_lowercase();
    if !matches!(
        prov.as_str(),
        "auto" | "desktop" | "steam" | "tmux" | "kitty" | "alacritty" | "ghostty"
    ) {
        return Err(format!(
            "ERR_LAUNCH_BAD_PARAMS: invalid `provider` '{provider}' (expected auto/desktop/steam/tmux/kitty/alacritty/ghostty)"
        ));
    }
    let args = req.args.unwrap_or_default();
    if args.len() > 32 {
        return Err("ERR_LAUNCH_BAD_PARAMS: `args` too many (max 32)".to_string());
    }
    for a in &args {
        if a.len() > 1024 {
            return Err("ERR_LAUNCH_BAD_PARAMS: arg too long (max 1024)".to_string());
        }
        if a.contains('\0') {
            return Err("ERR_LAUNCH_BAD_PARAMS: arg contains null byte".to_string());
        }
    }
    let dry_run = req.dry_run.unwrap_or(false);
    Ok((app_id, prov, args, dry_run))
}

pub fn parse_launch_list(params: &[u8]) -> Result<(String, Option<String>, u32, bool), String> {
    let req: LaunchListRequest = if params.is_empty() || params == b"{}" || params == b"null" {
        LaunchListRequest {
            provider: None,
            query: None,
            limit: None,
            include_hidden: None,
        }
    } else {
        serde_json::from_slice(params)
            .map_err(|e| format!("ERR_LAUNCH_BAD_PARAMS: invalid params_json: {e}"))?
    };
    let provider = req.provider.unwrap_or_else(|| "auto".to_string());
    let prov = provider.trim().to_lowercase();
    if !matches!(
        prov.as_str(),
        "auto" | "desktop" | "steam" | "tmux" | "kitty" | "alacritty" | "ghostty"
    ) {
        return Err(format!(
            "ERR_LAUNCH_BAD_PARAMS: invalid `provider` '{provider}'"
        ));
    }
    let query = req
        .query
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(ref q) = query {
        if q.len() > 256 {
            return Err("ERR_LAUNCH_BAD_PARAMS: `query` too long".to_string());
        }
    }
    let limit = req.limit.unwrap_or(100);
    if limit == 0 || limit > 500 {
        return Err("ERR_LAUNCH_BAD_PARAMS: `limit` must be 1..500".to_string());
    }
    let include_hidden = req.include_hidden.unwrap_or(false);
    Ok((prov, query, limit, include_hidden))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_launch_minimal() {
        let (id, prov, args, dry) = parse_launch(br#"{"app_id":"firefox"}"#).unwrap();
        assert_eq!(id, "firefox");
        assert_eq!(prov, "auto");
        assert!(args.is_empty());
        assert!(!dry);
    }
    #[test]
    fn parse_launch_with_provider_and_dry() {
        let (_, prov, _, dry) =
            parse_launch(br#"{"app_id":"123","provider":"steam","dry_run":true}"#).unwrap();
        assert_eq!(prov, "steam");
        assert!(dry);
    }
    #[test]
    fn parse_launch_rejects_empty() {
        assert!(parse_launch(br#"{"app_id":""}"#).is_err());
        assert!(parse_launch(br#"{"app_id":"  "}"#).is_err());
    }
    #[test]
    fn parse_launch_rejects_bad_provider() {
        assert!(parse_launch(br#"{"app_id":"x","provider":"bad"}"#).is_err());
    }
    #[test]
    fn parse_list_defaults() {
        let (prov, q, limit, hidden) = parse_launch_list(b"{}").unwrap();
        assert_eq!(prov, "auto");
        assert!(q.is_none());
        assert_eq!(limit, 100);
        assert!(!hidden);
    }
    #[test]
    fn parse_list_with_query() {
        let (_, q, limit, _) = parse_launch_list(br#"{"query":"fox","limit":10}"#).unwrap();
        assert_eq!(q.unwrap(), "fox");
        assert_eq!(limit, 10);
    }
}
