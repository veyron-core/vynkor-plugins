use std::path::PathBuf;

pub const DESKTOP_DIRS_ENV: &str = "LAUNCHER_DESKTOP_DIRS";
pub const STEAM_ROOTS_ENV: &str = "LAUNCHER_STEAM_ROOTS";
pub const ALLOWED_IDS_ENV: &str = "LAUNCHER_ALLOWED_IDS";
pub const TIMEOUT_MS_ENV: &str = "LAUNCHER_TIMEOUT_MS";
pub const STEAM_BINARY_ENV: &str = "LAUNCHER_STEAM_BINARY";
pub const DESKTOP_LAUNCHER_ENV: &str = "LAUNCHER_DESKTOP_LAUNCHER";
pub const CACHE_TTL_MS_ENV: &str = "LAUNCHER_CACHE_TTL_MS";
pub const TERMINAL_ENV: &str = "LAUNCHER_TERMINAL";
pub const TMUX_SESSION_ENV: &str = "LAUNCHER_TMUX_SESSION";

pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_STEAM_BINARY: &str = "steam";
pub const DEFAULT_CACHE_TTL_MS: u64 = 60_000;
pub const DEFAULT_TMUX_SESSION: &str = "vynkor";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopLauncher {
    Auto,
    GtkLaunch,
    Gio,
    XdgOpen,
    Exec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalKind {
    Auto,
    None,
    Tmux,
    Kitty,
    Alacritty,
    Ghostty,
}

impl TerminalKind {
    pub fn parse(s: Option<&str>) -> Result<Self, String> {
        match s.map(|v| v.trim().to_lowercase()).unwrap_or_else(|| "auto".to_string()).as_str() {
            "" | "auto" => Ok(Self::Auto),
            "none" => Ok(Self::None),
            "tmux" => Ok(Self::Tmux),
            "kitty" => Ok(Self::Kitty),
            "alacritty" => Ok(Self::Alacritty),
            "ghostty" => Ok(Self::Ghostty),
            other => Err(format!(
                "ERR_LAUNCH_BAD_PARAMS: invalid {TERMINAL_ENV} '{other}' (expected auto/none/tmux/kitty/alacritty/ghostty)"
            )),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Tmux => "tmux",
            Self::Kitty => "kitty",
            Self::Alacritty => "alacritty",
            Self::Ghostty => "ghostty",
        }
    }
    pub fn detect_auto() -> Self {
        if crate::runner::binary_in_path("tmux") {
            return Self::Tmux;
        }
        if crate::runner::binary_in_path("ghostty") {
            return Self::Ghostty;
        }
        if crate::runner::binary_in_path("kitty") {
            return Self::Kitty;
        }
        if crate::runner::binary_in_path("alacritty") {
            return Self::Alacritty;
        }
        Self::None
    }
    pub fn effective(&self) -> Self {
        if *self == Self::Auto {
            Self::detect_auto()
        } else {
            self.clone()
        }
    }
}

impl DesktopLauncher {
    pub fn parse(s: Option<&str>) -> Result<Self, String> {
        match s.map(|v| v.trim()).unwrap_or("auto") {
            "" | "auto" => Ok(Self::Auto),
            "gtk-launch" => Ok(Self::GtkLaunch),
            "gio" => Ok(Self::Gio),
            "xdg-open" => Ok(Self::XdgOpen),
            "exec" => Ok(Self::Exec),
            other => Err(format!(
                "ERR_LAUNCH_BAD_PARAMS: invalid {DESKTOP_LAUNCHER_ENV} '{other}' (expected auto/gtk-launch/gio/xdg-open/exec)"
            )),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::GtkLaunch => "gtk-launch",
            Self::Gio => "gio",
            Self::XdgOpen => "xdg-open",
            Self::Exec => "exec",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub desktop_dirs: Vec<PathBuf>,
    pub steam_roots: Vec<PathBuf>,
    pub allowed_ids: Option<Vec<String>>,
    pub timeout_ms: u64,
    pub steam_binary: String,
    pub desktop_launcher: DesktopLauncher,
    pub cache_ttl_ms: u64,
    pub terminal: TerminalKind,
    pub tmux_session: String,
}

impl Config {
    pub fn from_env() -> Self {
        let desktop_dirs = parse_dirs(&std::env::var(DESKTOP_DIRS_ENV).ok());
        let steam_roots = parse_steam_roots(&std::env::var(STEAM_ROOTS_ENV).ok());
        let allowed_ids = parse_allowed_ids(&std::env::var(ALLOWED_IDS_ENV).ok());
        let timeout_ms = std::env::var(TIMEOUT_MS_ENV)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        let steam_binary = std::env::var(STEAM_BINARY_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_STEAM_BINARY.to_string());
        let desktop_launcher =
            DesktopLauncher::parse(std::env::var(DESKTOP_LAUNCHER_ENV).ok().as_deref())
                .unwrap_or(DesktopLauncher::Auto);
        let cache_ttl_ms = std::env::var(CACHE_TTL_MS_ENV)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_CACHE_TTL_MS);
        let terminal = TerminalKind::parse(std::env::var(TERMINAL_ENV).ok().as_deref())
            .unwrap_or(TerminalKind::Auto);
        let tmux_session = std::env::var(TMUX_SESSION_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_TMUX_SESSION.to_string());
        Self {
            desktop_dirs,
            steam_roots,
            allowed_ids,
            timeout_ms,
            steam_binary,
            desktop_launcher,
            cache_ttl_ms,
            terminal,
            tmux_session,
        }
    }
}

fn parse_dirs(raw: &Option<String>) -> Vec<PathBuf> {
    if let Some(s) = raw {
        if !s.trim().is_empty() {
            return s
                .split(',')
                .filter_map(|p| {
                    let t = p.trim();
                    if t.is_empty() {
                        None
                    } else {
                        Some(PathBuf::from(t))
                    }
                })
                .collect();
        }
    }
    default_desktop_dirs()
}

fn default_desktop_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    // XDG_DATA_DIRS default /usr/local/share:/usr/share
    let xdg = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for p in xdg.split(':') {
        if !p.trim().is_empty() {
            dirs.push(PathBuf::from(p).join("applications"));
        }
    }
    dirs
}

fn parse_steam_roots(raw: &Option<String>) -> Vec<PathBuf> {
    if let Some(s) = raw {
        if !s.trim().is_empty() {
            return s
                .split(',')
                .filter_map(|p| {
                    let t = p.trim();
                    if t.is_empty() {
                        None
                    } else {
                        Some(PathBuf::from(t))
                    }
                })
                .collect();
        }
        // explicit empty string => scan nothing
        return Vec::new();
    }
    default_steam_roots()
}

fn default_steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let h = PathBuf::from(home);
        roots.push(h.join(".steam/steam"));
        roots.push(h.join(".local/share/Steam"));
        roots.push(h.join(".steam/root"));
    }
    roots
}

fn parse_allowed_ids(raw: &Option<String>) -> Option<Vec<String>> {
    let s = raw.as_ref()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let ids: Vec<String> = trimmed
        .split(',')
        .filter_map(|p| {
            let t = p.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        })
        .collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

pub fn is_allowed(id: &str, allowed: &Option<Vec<String>>) -> bool {
    match allowed {
        None => true,
        Some(list) => list.iter().any(|a| a == id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_desktop_launcher_variants() {
        assert_eq!(DesktopLauncher::parse(None).unwrap(), DesktopLauncher::Auto);
        assert_eq!(
            DesktopLauncher::parse(Some("")).unwrap(),
            DesktopLauncher::Auto
        );
        assert_eq!(
            DesktopLauncher::parse(Some("gtk-launch")).unwrap(),
            DesktopLauncher::GtkLaunch
        );
        assert_eq!(
            DesktopLauncher::parse(Some("gio")).unwrap(),
            DesktopLauncher::Gio
        );
        assert!(DesktopLauncher::parse(Some("bad")).is_err());
    }
    #[test]
    fn allowed_ids_parsing() {
        assert_eq!(parse_allowed_ids(&None), None);
        assert_eq!(parse_allowed_ids(&Some("".to_string())), None);
        assert_eq!(parse_allowed_ids(&Some("  ".to_string())), None);
        assert_eq!(
            parse_allowed_ids(&Some("a,b , c".to_string())),
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }
}
