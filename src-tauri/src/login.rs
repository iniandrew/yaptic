//! Launch at login via a per-user LaunchAgent that runs this executable.

use std::{fs, path::PathBuf};

const LABEL: &str = "io.github.iniandrew.yaptic";

fn plist_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

pub fn is_enabled() -> bool {
    plist_path().map(|p| p.is_file()).unwrap_or(false)
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let path = plist_path()?;
    if !enabled {
        return match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("remove {}: {e}", path.display())),
        };
    }

    let exe = std::env::current_exe().map_err(|e| format!("locate app: {e}"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    fs::write(&path, plist(&exe.to_string_lossy()))
        .map_err(|e| format!("write {}: {e}", path.display()))
}

fn plist(exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>ProcessType</key>
  <string>Interactive</string>
</dict>
</plist>
"#,
        xml_escape(exe)
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_executable_path() {
        let xml = plist("/Applications/R&D <x>.app/Contents/MacOS/yaptic");
        assert!(xml.contains("<string>/Applications/R&amp;D &lt;x&gt;.app/Contents/MacOS/yaptic</string>"));
        assert!(xml.contains("<key>RunAtLoad</key>\n  <true/>"));
    }
}
