use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SteamEntry {
    pub appid: String,
    pub name: String,
    pub path: PathBuf,
}

pub fn scan_steam_roots(roots: &[PathBuf]) -> Vec<SteamEntry> {
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        // root may be .../steam or .../Steam; steamapps is directly under it
        let steamapps = root.join("steamapps");
        if !steamapps.is_dir() {
            continue;
        }
        // also scan libraryfolders.vdf for additional libraries
        let additional = parse_libraryfolders(&steamapps.join("libraryfolders.vdf"));
        let mut all_roots = vec![steamapps.clone()];
        for lib in additional {
            let p = PathBuf::from(lib).join("steamapps");
            if p.is_dir() && p != steamapps {
                all_roots.push(p);
            }
        }
        for lib_steamapps in all_roots {
            let Ok(read) = std::fs::read_dir(&lib_steamapps) else {
                continue;
            };
            for entry in read.flatten() {
                let path = entry.path();
                let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if !fname.starts_with("appmanifest_") || !fname.ends_with(".acf") {
                    continue;
                }
                match parse_appmanifest(&path) {
                    Ok(e) => {
                        if seen.contains(&e.appid) {
                            continue;
                        }
                        seen.insert(e.appid.clone());
                        entries.push(e);
                    }
                    Err(_) => continue,
                }
            }
        }
    }
    entries.sort_by(|a, b| a.appid.cmp(&b.appid));
    entries
}

pub fn parse_appmanifest(path: &Path) -> Result<SteamEntry, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read failed: {e}"))?;
    parse_appmanifest_content(&content, path.to_path_buf())
}

pub fn parse_appmanifest_content(content: &str, path: PathBuf) -> Result<SteamEntry, String> {
    let appid = extract_quoted_value(content, "\"appid\"").ok_or("missing appid")?;
    let name =
        extract_quoted_value(content, "\"name\"").unwrap_or_else(|| format!("Steam App {appid}"));
    if appid.trim().is_empty() || !appid.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("invalid appid '{appid}'"));
    }
    Ok(SteamEntry { appid, name, path })
}

fn extract_quoted_value(content: &str, key: &str) -> Option<String> {
    // find key, then next two quoted strings: key "value"
    let idx = content.find(key)?;
    let after = &content[idx + key.len()..];
    // find first quote
    let mut in_quote = false;
    let mut start = None;
    let mut end = None;
    let mut escaped = false;
    let mut chars = after.chars().enumerate();
    for (i, c) in &mut chars {
        if c == '"' && !escaped {
            if !in_quote {
                in_quote = true;
                start = Some(i + 1);
            } else {
                end = Some(i);
                break;
            }
        }
        escaped = c == '\\' && !escaped;
        if c != '\\' {
            escaped = false;
        }
    }
    let s = start?;
    let e = end?;
    let val = &after[s..e];
    Some(val.to_string())
}

pub fn parse_libraryfolders(path: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_libraryfolders_content(&content)
}

pub fn parse_libraryfolders_content(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        let mut vals = Vec::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '"' {
                let mut v = String::new();
                while let Some(nc) = chars.next() {
                    if nc == '"' {
                        break;
                    }
                    if nc == '\\' {
                        if let Some(ec) = chars.next() {
                            v.push(ec);
                        } else {
                            break;
                        }
                    } else {
                        v.push(nc);
                    }
                }
                vals.push(v);
            }
        }
        if vals.is_empty() {
            continue;
        }
        for v in vals {
            if v.starts_with('/') && v.contains('/') {
                out.push(v);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_appmanifest_simple() {
        let content = r#""AppState" { "appid" "12345" "name" "My Game" }"#;
        let e = parse_appmanifest_content(content, PathBuf::from("/tmp/appmanifest_12345.acf"))
            .unwrap();
        assert_eq!(e.appid, "12345");
        assert_eq!(e.name, "My Game");
    }
    #[test]
    fn parse_appmanifest_missing_name_defaults() {
        let content = r#""AppState" { "appid" "999" }"#;
        let e = parse_appmanifest_content(content, PathBuf::from("/tmp/a.acf")).unwrap();
        assert_eq!(e.appid, "999");
        assert!(e.name.contains("999"));
    }
    #[test]
    fn parse_appmanifest_invalid_appid() {
        let content = r#""AppState" { "appid" "abc" "name" "x" }"#;
        assert!(parse_appmanifest_content(content, PathBuf::from("/x")).is_err());
    }
    #[test]
    fn parse_libraryfolders_variants() {
        let content = r#"
            "libraryfolders"
            {
                "0" { "path" "/home/user/.steam/steam" }
                "1" { "path" "/mnt/ssd/SteamLibrary" }
            }
        "#;
        let libs = parse_libraryfolders_content(content);
        assert!(libs.contains(&"/home/user/.steam/steam".to_string()));
        assert!(libs.contains(&"/mnt/ssd/SteamLibrary".to_string()));
    }
    #[test]
    fn scan_tmpdir() {
        let dir = tempfile::tempdir().unwrap();
        let steamapps = dir.path().join("steamapps");
        std::fs::create_dir_all(&steamapps).unwrap();
        std::fs::write(
            steamapps.join("appmanifest_10.acf"),
            r#""AppState" { "appid" "10" "name" "A" }"#,
        )
        .unwrap();
        std::fs::write(
            steamapps.join("appmanifest_20.acf"),
            r#""AppState" { "appid" "20" "name" "B" }"#,
        )
        .unwrap();
        let entries = scan_steam_roots(&[dir.path().to_path_buf()]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].appid, "10");
    }
}
