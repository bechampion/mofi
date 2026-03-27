use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Content captured from the system clipboard.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ClipboardContent {
    Text(String),
    Image {
        /// PNG-encoded bytes used when restoring this entry back to clipboard.
        png: Vec<u8>,
        width: u32,
        height: u32,
    },
}

/// A single entry in the clipboard history.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ClipboardEntry {
    pub content: ClipboardContent,
    /// Unix timestamp of when this was first captured.
    pub captured_at: u64,
}

impl ClipboardEntry {
    /// Single-line truncated preview for display.
    pub fn preview(&self) -> String {
        match &self.content {
            ClipboardContent::Text(text) => {
                let line = text.lines().next().unwrap_or("").trim();
                if line.chars().count() > 80 {
                    let s: String = line.chars().take(77).collect();
                    format!("{}…", s)
                } else {
                    line.to_string()
                }
            }
            ClipboardContent::Image { width, height, .. } => {
                format!("Screenshot {}x{}", width, height)
            }
        }
    }

    /// Human-readable subtitle.
    pub fn subtitle(&self) -> String {
        match &self.content {
            ClipboardContent::Text(text) => {
                let lines = text.lines().count();
                let chars = text.chars().count();
                if lines > 1 {
                    format!("{} lines · {} chars", lines, chars)
                } else {
                    format!("{} chars", chars)
                }
            }
            ClipboardContent::Image { png, width, height } => {
                let kib = (png.len() as f64 / 1024.0).round() as u64;
                format!("PNG · {}x{} · {} KiB", width, height, kib)
            }
        }
    }

    pub fn text(&self) -> Option<&str> {
        match &self.content {
            ClipboardContent::Text(text) => Some(text.as_str()),
            ClipboardContent::Image { .. } => None,
        }
    }

    pub fn image_png(&self) -> Option<&[u8]> {
        match &self.content {
            ClipboardContent::Image { png, .. } => Some(png.as_slice()),
            ClipboardContent::Text(_) => None,
        }
    }

    pub fn is_image(&self) -> bool {
        matches!(self.content, ClipboardContent::Image { .. })
    }
}

/// Thread-safe shared clipboard history.
pub type ClipboardHistory = Arc<Mutex<Vec<ClipboardEntry>>>;

const MAX_HISTORY: usize = 100;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

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
fn push_entry(history: &mut Vec<ClipboardEntry>, content: ClipboardContent) {
    history.retain(|e| e.content != content);

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    history.insert(0, ClipboardEntry { content, captured_at: ts });
    history.truncate(MAX_HISTORY);
    save_history(history);
}

fn content_from_raw_image(bytes: &[u8], is_png: bool) -> Option<ClipboardContent> {
    let decoded = if is_png {
        image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?
    } else {
        image::load_from_memory_with_format(bytes, image::ImageFormat::Tiff).ok()?
    };

    let (width, height) = (decoded.width(), decoded.height());
    if width == 0 || height == 0 {
        return None;
    }

    let mut png = Vec::new();
    if decoded
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .is_err()
    {
        return None;
    }

    if png.len() > MAX_IMAGE_BYTES {
        return None;
    }

    Some(ClipboardContent::Image { png, width, height })
}

/// Read current clipboard content, but return None if the pasteboard is
/// marked with `org.nspasteboard.ConcealedType` (passwords, secrets).
fn read_current() -> Option<ClipboardContent> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF};
    use objc2_foundation::{NSArray, NSString};

    unsafe {
        let pb = NSPasteboard::generalPasteboard();

        // If the concealment marker is present, skip this entry entirely.
        let concealed_type = NSString::from_str("org.nspasteboard.ConcealedType");
        let available = pb.availableTypeFromArray(&NSArray::from_retained_slice(&[concealed_type]));
        if available.is_some() {
            return None;
        }

        // Prefer plain text if available.
        if let Some(s) = pb.stringForType(NSPasteboardTypeString) {
            let text = s.to_string();
            if !text.trim().is_empty() {
                return Some(ClipboardContent::Text(text));
            }
        }

        // Otherwise try image formats (PNG first, then TIFF).
        if let Some(data) = pb.dataForType(NSPasteboardTypePNG) {
            let bytes = data.to_vec();
            if let Some(content) = content_from_raw_image(&bytes, true) {
                return Some(content);
            }
        }
        if let Some(data) = pb.dataForType(NSPasteboardTypeTIFF) {
            let bytes = data.to_vec();
            if let Some(content) = content_from_raw_image(&bytes, false) {
                return Some(content);
            }
        }

        None
    }
}

/// Spawn a background thread that polls the clipboard every 500 ms.
/// Any new content is prepended to `history` and persisted to disk.
/// Entries marked with org.nspasteboard.ConcealedType are silently skipped.
pub fn start_poller(history: ClipboardHistory) {
    std::thread::spawn(move || {
        let mut last: Option<ClipboardContent> = {
            let lock = history.lock().unwrap();
            lock.first().map(|e| e.content.clone())
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
                continue;
            }
            last_change_count = change_count;

            match read_current() {
                Some(current) if Some(&current) != last.as_ref() => {
                    let mut lock = history.lock().unwrap();
                    push_entry(&mut lock, current.clone());
                    last = Some(current);
                }
                Some(_) => {}
                None => {
                    last = None;
                }
            }
        }
    });
}
