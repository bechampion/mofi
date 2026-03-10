use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// A single entry in the clipboard history.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ClipboardEntry {
    pub text: String,
    /// Unix timestamp of when this was first captured.
    pub captured_at: u64,
}

impl ClipboardEntry {
    /// Single-line truncated preview for display.
    pub fn preview(&self) -> String {
        let line = self.text.lines().next().unwrap_or("").trim();
        if line.chars().count() > 80 {
            let s: String = line.chars().take(77).collect();
            format!("{}…", s)
        } else {
            line.to_string()
        }
    }

    /// Human-readable subtitle: line count or char count.
    pub fn subtitle(&self) -> String {
        let lines = self.text.lines().count();
        let chars = self.text.chars().count();
        if lines > 1 {
            format!("{} lines · {} chars", lines, chars)
        } else {
            format!("{} chars", chars)
        }
    }
}

/// Thread-safe shared clipboard history.
pub type ClipboardHistory = Arc<Mutex<Vec<ClipboardEntry>>>;

const MAX_HISTORY: usize = 100;

/// Path where history is persisted between launches.
fn history_path() -> std::path::PathBuf {
    let mut p = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir);
    p.push("rofi-mac");
    let _ = std::fs::create_dir_all(&p);
    p.push("clipboard_history.json");
    p
}

/// Load history from disk. Returns an empty vec on any error.
pub fn load_history() -> Vec<ClipboardEntry> {
    let path = history_path();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Save history to disk (silently ignores errors).
fn save_history(history: &[ClipboardEntry]) {
    if let Ok(json) = serde_json::to_vec_pretty(history) {
        let _ = std::fs::write(history_path(), json);
    }
}

/// Push a new entry into history (deduplicates, trims to MAX_HISTORY, saves).
fn push_entry(history: &mut Vec<ClipboardEntry>, text: String) {
    // Strip leading/trailing blank lines and whitespace.
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }

    // Remove any existing duplicate
    history.retain(|e| e.text != text);

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    history.insert(
        0,
        ClipboardEntry {
            text,
            captured_at: ts,
        },
    );
    history.truncate(MAX_HISTORY);
    save_history(history);
}

// ── Platform clipboard read ───────────────────────────────────────────────────

/// Read current clipboard content.
pub fn read_current() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("pbpaste").output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).to_string();
        if s.trim().is_empty() {
            None
        } else {
            Some(s)
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Try wl-paste (Wayland) first, fall back to xclip (X11).
        if let Some(s) = run_paste_cmd("wl-paste", &["--no-newline"]) {
            return Some(s);
        }
        if let Some(s) = run_paste_cmd("xclip", &["-selection", "clipboard", "-o"]) {
            return Some(s);
        }
        // arboard as last resort (links libxcb / wayland at compile time).
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if let Ok(text) = cb.get_text() {
                if !text.trim().is_empty() {
                    return Some(text);
                }
            }
        }
        None
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn run_paste_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

// ── Platform clipboard write ──────────────────────────────────────────────────

/// Write text to the system clipboard.
pub fn write_clipboard(text: &str) {
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        if let Ok(mut child) = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Try wl-copy (Wayland) first, then xclip (X11).
        let written = try_write_clipboard_cmd("wl-copy", &[], text)
            || try_write_clipboard_cmd("xclip", &["-selection", "clipboard"], text);
        if !written {
            // arboard fallback.
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_text(text);
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = text;
    }
}

#[cfg(target_os = "linux")]
fn try_write_clipboard_cmd(cmd: &str, args: &[&str], text: &str) -> bool {
    use std::io::Write;
    let child = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .spawn();
    if let Ok(mut child) = child {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        return child.wait().map(|s| s.success()).unwrap_or(false);
    }
    false
}

/// Spawn a background thread that polls the clipboard every 500 ms.
/// Any new content is prepended to `history` and persisted to disk.
pub fn start_poller(history: ClipboardHistory) {
    std::thread::spawn(move || {
        let mut last: Option<String> = {
            let lock = history.lock().unwrap();
            lock.first().map(|e| e.text.clone())
        };

        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));

            if let Some(current) = read_current() {
                if Some(&current) != last.as_ref() {
                    let mut lock = history.lock().unwrap();
                    push_entry(&mut lock, current.clone());
                    last = Some(current);
                }
            }
        }
    });
}
