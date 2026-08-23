use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub id: String,
    pub name: String,
    pub exec: Option<String>,
    pub path: PathBuf,
    pub no_display: bool,
    pub hidden: bool,
    pub entry_type: String,
    pub terminal: bool,
    pub working_dir: Option<String>,
}

pub fn scan_desktop_dirs(dirs: &[PathBuf], include_hidden: bool) -> Vec<DesktopEntry> {
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let Ok(read) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if seen.contains(stem) {
                continue;
            }
            match parse_desktop_file(&path) {
                Ok(e) => {
                    if !include_hidden && (e.no_display || e.hidden) {
                        continue;
                    }
                    if e.entry_type != "Application" {
                        continue;
                    }
                    if e.exec.is_none() {
                        continue;
                    }
                    seen.insert(stem.to_string());
                    entries.push(e);
                }
                Err(_) => continue,
            }
        }
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    entries
}

pub fn parse_desktop_file(path: &Path) -> Result<DesktopEntry, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read failed: {e}"))?;
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    parse_desktop_content(&content, id, path.to_path_buf())
}

pub fn parse_desktop_content(
    content: &str,
    id: String,
    path: PathBuf,
) -> Result<DesktopEntry, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    let mut in_desktop = false;
    let mut has_desktop_section = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop = line == "[Desktop Entry]";
            if in_desktop {
                has_desktop_section = true;
            }
            continue;
        }
        if !in_desktop {
            continue;
        }
        if let Some(idx) = line.find('=') {
            let k = line[..idx].trim().to_string();
            let v = line[idx + 1..].trim().to_string();
            map.entry(k).or_insert(v);
        }
    }
    if !has_desktop_section {
        return Err("missing [Desktop Entry]".to_string());
    }
    let name = map.get("Name").cloned().unwrap_or_else(|| id.clone());
    let exec = map.get("Exec").cloned();
    let entry_type = map
        .get("Type")
        .cloned()
        .unwrap_or_else(|| "Application".to_string());
    let no_display = map
        .get("NoDisplay")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false);
    let hidden = map
        .get("Hidden")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false);
    let terminal = map
        .get("Terminal")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false);
    let working_dir = map.get("Path").cloned().filter(|s| !s.trim().is_empty());
    Ok(DesktopEntry {
        id,
        name,
        exec,
        path,
        no_display,
        hidden,
        entry_type,
        terminal,
        working_dir,
    })
}

/// Strip field codes (%f %F %u %U %d %D %n %N %i %c %k %v %m) from Exec.
/// Keeps quoted strings intact, just removes %X tokens and collapses spaces.
pub fn strip_field_codes(exec: &str) -> String {
    let mut out = String::new();
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(nc) = chars.next() {
                // valid field code chars; invalid % stays removed per spec
                let _ = nc;
                // don't push anything
                // also consume trailing space duplicate handling later
            }
        } else {
            out.push(c);
        }
    }
    // collapse multiple spaces and trim
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn exec_argv(exec: &str) -> Vec<String> {
    let stripped = strip_field_codes(exec);
    // simple shell-word split respecting double/single quotes
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for c in stripped.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if !in_single => {
                escaped = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            ' ' | '\t' if !in_single && !in_double => {
                if !cur.is_empty() {
                    args.push(cur.clone());
                    cur.clear();
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_minimal_desktop() {
        let content = "[Desktop Entry]\nName=Firefox\nExec=firefox %u\nType=Application\n";
        let e = parse_desktop_content(
            content,
            "firefox".into(),
            PathBuf::from("/tmp/firefox.desktop"),
        )
        .unwrap();
        assert_eq!(e.name, "Firefox");
        assert_eq!(e.exec.unwrap(), "firefox %u");
        assert_eq!(e.entry_type, "Application");
        assert!(!e.no_display);
    }
    #[test]
    fn parse_hidden_filtered() {
        let content =
            "[Desktop Entry]\nName=Hidden\nExec=hidden\nType=Application\nNoDisplay=true\n";
        let e = parse_desktop_content(content, "hidden".into(), PathBuf::from("/tmp/h.desktop"))
            .unwrap();
        assert!(e.no_display);
    }
    #[test]
    fn strip_field_codes_removes_all() {
        assert_eq!(strip_field_codes("firefox %u"), "firefox");
        assert_eq!(strip_field_codes("app %f --flag %U"), "app --flag");
        assert_eq!(strip_field_codes("/usr/bin/app %F"), "/usr/bin/app");
    }
    #[test]
    fn exec_argv_simple() {
        assert_eq!(exec_argv("firefox %u"), vec!["firefox"]);
        assert_eq!(
            exec_argv("/usr/bin/code --new-window %F"),
            vec!["/usr/bin/code", "--new-window"]
        );
    }
    #[test]
    fn exec_argv_quoted() {
        assert_eq!(
            exec_argv("\"/opt/My App/app\" --flag"),
            vec!["/opt/My App/app", "--flag"]
        );
    }
    #[test]
    fn scan_tmpdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.desktop"),
            "[Desktop Entry]\nName=A\nExec=a\nType=Application\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.desktop"),
            "[Desktop Entry]\nName=B\nExec=b\nType=Application\nNoDisplay=true\n",
        )
        .unwrap();
        let entries = scan_desktop_dirs(&[dir.path().to_path_buf()], false);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "a");
        let all = scan_desktop_dirs(&[dir.path().to_path_buf()], true);
        assert_eq!(all.len(), 2);
    }
}
