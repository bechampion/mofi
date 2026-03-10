use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// A single entry in the clipboard history.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ClipboardEntry {
    pub text: String,
    /// Unix timestamp of when this was first captured.
    pub captured_at: u64,
    /// If this entry is an image, the path to the saved PNG file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_path: Option<String>,
    /// Pre-rendered thumbnail for image entries (small PNG).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_path: Option<String>,
}

impl ClipboardEntry {
    /// True if this entry represents an image rather than text.
    pub fn is_image(&self) -> bool {
        self.image_path.is_some()
    }

    /// Single-line truncated preview for display.
    pub fn preview(&self) -> String {
        if self.is_image() {
            return self.text.clone(); // e.g. "Screenshot 1920x1080"
        }
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
        if let Some(path) = &self.image_path {
            // Show file size.
            if let Ok(meta) = std::fs::metadata(path) {
                let kb = meta.len() / 1024;
                return format!("image · {} KB", kb);
            }
            return "image".to_string();
        }
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
const MAX_IMAGES: usize = 20;

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

/// Directory where clipboard images are saved.
fn images_dir() -> std::path::PathBuf {
    let mut p = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir);
    p.push("rofi-mac");
    p.push("clipboard_images");
    let _ = std::fs::create_dir_all(&p);
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

/// Push a new text entry into history (deduplicates, trims to MAX_HISTORY, saves).
fn push_entry(history: &mut Vec<ClipboardEntry>, text: String) {
    // Strip leading/trailing blank lines and whitespace.
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }

    // Remove any existing duplicate (text entries only).
    history.retain(|e| e.is_image() || e.text != text);

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    history.insert(
        0,
        ClipboardEntry {
            text,
            captured_at: ts,
            image_path: None,
            thumbnail_path: None,
        },
    );
    history.truncate(MAX_HISTORY);
    save_history(history);
}

/// Resize a PNG to a small thumbnail (max THUMB_H pixels tall).
/// Returns Some(path) on success, None on failure.
#[cfg(target_os = "linux")]
const THUMB_H: u32 = 80;

#[cfg(target_os = "linux")]
fn generate_thumbnail(png_bytes: &[u8], out_path: &std::path::Path) -> Option<String> {
    use image::GenericImageView;
    let img = image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png).ok()?;
    let (w, h) = img.dimensions();
    if h <= THUMB_H {
        // Already small enough — just copy.
        std::fs::write(out_path, png_bytes).ok()?;
    } else {
        let new_h = THUMB_H;
        let new_w = (w as f64 * new_h as f64 / h as f64).round() as u32;
        let thumb = img.resize_exact(new_w, new_h, image::imageops::FilterType::Triangle);
        thumb.save(out_path).ok()?;
    }
    Some(out_path.to_string_lossy().to_string())
}

/// Push a new image entry into history.  Saves the raw PNG bytes to disk
/// and adds an entry with the path.  Prunes old images beyond MAX_IMAGES.
/// Deduplicates by comparing bytes against existing image files — if the
/// clipboard contains the same image again we just bump it to the top.
#[cfg(target_os = "linux")]
fn push_image_entry(history: &mut Vec<ClipboardEntry>, png_bytes: &[u8], label: String) {
    // Check for a byte-identical image already in history.
    for (i, entry) in history.iter().enumerate() {
        if let Some(ref existing_path) = entry.image_path {
            if let Ok(existing_bytes) = std::fs::read(existing_path) {
                if existing_bytes == png_bytes {
                    // Identical image — bump to top instead of adding a duplicate.
                    let mut dup = history.remove(i);
                    dup.captured_at = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    history.insert(0, dup);
                    save_history(history);
                    return;
                }
            }
        }
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let filename = format!("clip_{}.png", ts);
    let thumb_filename = format!("clip_{}_thumb.png", ts);
    let path = images_dir().join(&filename);
    let thumb_path = images_dir().join(&thumb_filename);
    if std::fs::write(&path, png_bytes).is_err() {
        return;
    }
    let path_str = path.to_string_lossy().to_string();

    // Generate thumbnail (max 80px tall, preserving aspect ratio).
    let thumb_path_str = generate_thumbnail(png_bytes, &thumb_path);

    history.insert(
        0,
        ClipboardEntry {
            text: label,
            captured_at: ts,
            image_path: Some(path_str),
            thumbnail_path: thumb_path_str,
        },
    );
    history.truncate(MAX_HISTORY);

    // Prune old image files beyond MAX_IMAGES.
    let image_entries: Vec<(Option<String>, Option<String>)> = history
        .iter()
        .filter(|e| e.image_path.is_some())
        .map(|e| (e.image_path.clone(), e.thumbnail_path.clone()))
        .collect();
    if image_entries.len() > MAX_IMAGES {
        for (img, thumb) in &image_entries[MAX_IMAGES..] {
            if let Some(p) = img {
                let _ = std::fs::remove_file(p);
            }
            if let Some(p) = thumb {
                let _ = std::fs::remove_file(p);
            }
        }
        // Remove the history entries whose files we just deleted.
        let keep_set: std::collections::HashSet<&str> = image_entries[..MAX_IMAGES]
            .iter()
            .filter_map(|(p, _)| p.as_deref())
            .collect();
        history.retain(|e| {
            e.image_path
                .as_deref()
                .map_or(true, |p| keep_set.contains(p))
        });
    }

    save_history(history);
}

// ── Platform clipboard read ───────────────────────────────────────────────────

/// Read current clipboard text content.
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
        // --type text restricts to text MIME types, so screenshots and other
        // binary clipboard content (image/png etc.) are ignored.
        if let Some(s) = run_paste_cmd("wl-paste", &["--no-newline", "--type", "text"]) {
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

/// Check if the clipboard currently holds an image and return the raw PNG bytes
/// plus a human-readable label like "Screenshot 1920×1080".
#[cfg(target_os = "linux")]
fn read_current_image() -> Option<(Vec<u8>, String)> {
    // Check MIME types offered by the clipboard.
    let types_out = std::process::Command::new("wl-paste")
        .args(["--list-types"])
        .output()
        .ok()?;
    let types = String::from_utf8_lossy(&types_out.stdout);
    if !types.lines().any(|t| t.trim() == "image/png") {
        return None;
    }

    // Read the raw PNG bytes.
    let out = std::process::Command::new("wl-paste")
        .args(["--no-newline", "--type", "image/png"])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }

    // Try to read dimensions for the label.
    let label = match image::ImageReader::new(std::io::Cursor::new(&out.stdout))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
    {
        Some((w, h)) => format!("Screenshot {}x{}", w, h),
        None => "Screenshot".to_string(),
    };

    Some((out.stdout, label))
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

/// Write an image (PNG file) to the system clipboard.
#[cfg(target_os = "linux")]
pub fn write_clipboard_image(png_path: &str) {
    use std::io::Write;
    if let Ok(data) = std::fs::read(png_path) {
        let child = std::process::Command::new("wl-copy")
            .args(["--type", "image/png"])
            .stdin(std::process::Stdio::piped())
            .spawn();
        if let Ok(mut child) = child {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(&data);
            }
            let _ = child.wait();
        }
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
        // Track the last image hash to avoid re-saving the same screenshot.
        let mut last_image_len: usize = 0;

        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));

            // Check for image content first (Linux only).
            #[cfg(target_os = "linux")]
            {
                if let Some((png_bytes, label)) = read_current_image() {
                    // Only push if this is a new image (different size = simple heuristic).
                    if png_bytes.len() != last_image_len {
                        last_image_len = png_bytes.len();
                        let mut lock = history.lock().unwrap();
                        push_image_entry(&mut lock, &png_bytes, label);
                        // Reset text tracker so we don't skip the next text copy.
                        last = None;
                    }
                    continue;
                }
            }

            if let Some(current) = read_current() {
                if Some(&current) != last.as_ref() {
                    let mut lock = history.lock().unwrap();
                    push_entry(&mut lock, current.clone());
                    last = Some(current);
                    last_image_len = 0;
                }
            }
        }
    });
}
