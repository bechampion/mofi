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
    // Remove any existing duplicate
    history.retain(|e| e.text != text);

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    history.insert(0, ClipboardEntry { text, captured_at: ts });
    history.truncate(MAX_HISTORY);
    save_history(history);
}

/// Read current clipboard content, but return None if the pasteboard is
/// marked with `org.nspasteboard.ConcealedType` (passwords, secrets).
fn read_current() -> Option<String> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;

    unsafe {
        let pb = NSPasteboard::generalPasteboard();

        // If the concealment marker is present, skip this entry entirely.
        let concealed_type = NSString::from_str("org.nspasteboard.ConcealedType");
        let available = pb.availableTypeFromArray(
            &objc2_foundation::NSArray::from_retained_slice(&[concealed_type])
        );
        if available.is_some() {
            return None;
        }

        // Read plain text normally.
        let s = pb.stringForType(NSPasteboardTypeString)?;
        let text = s.to_string();
        if text.trim().is_empty() { None } else { Some(text) }
    }
}

/// Spawn a background thread that polls the clipboard every 500 ms.
/// Any new content is prepended to `history` and persisted to disk.
/// Entries marked with org.nspasteboard.ConcealedType are silently skipped.
pub fn start_poller(history: ClipboardHistory) {
    std::thread::spawn(move || {
        let mut last: Option<String> = {
            let lock = history.lock().unwrap();
            lock.first().map(|e| e.text.clone())
        };
        // Track the raw pasteboard change count so we can detect concealed
        // changes (where read_current returns None) and advance `last` past them.
        let mut last_change_count: isize = {
            use objc2_app_kit::NSPasteboard;
            NSPasteboard::generalPasteboard().changeCount()
        };

        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));

            let change_count: isize = {
                use objc2_app_kit::NSPasteboard;
                NSPasteboard::generalPasteboard().changeCount()
            };

            if change_count == last_change_count {
                continue; // nothing changed
            }
            last_change_count = change_count;

            match read_current() {
                Some(current) if Some(&current) != last.as_ref() => {
                    let mut lock = history.lock().unwrap();
                    push_entry(&mut lock, current.clone());
                    last = Some(current);
                }
                Some(_) => {} // same content, no-op
                None => {
                    // Concealed entry (or empty) — don't record, but clear `last`
                    // so the next real copy is always treated as new.
                    last = None;
                }
            }
        }
    });
}
