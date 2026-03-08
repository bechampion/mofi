use std::path::{Path, PathBuf};

/// A single entry in the pass store, e.g. `disney/aws/accesskey`.
#[derive(Clone, Debug)]
pub struct PassEntry {
    /// Display path relative to the store root, e.g. `disney/aws/accesskey`.
    pub name: String,
}

/// Scan `~/.password-store` recursively and return all `.gpg` entries.
pub fn discover_pass_entries() -> Vec<PassEntry> {
    let store = match dirs::home_dir() {
        Some(h) => h.join(".password-store"),
        None => return Vec::new(),
    };

    if !store.exists() {
        return Vec::new();
    }

    let mut entries = Vec::new();
    collect_entries(&store, &store, &mut entries);
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    entries
}

fn collect_entries(root: &Path, dir: &Path, out: &mut Vec<PassEntry>) {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };

    for entry in read.flatten() {
        let path = entry.path();

        // Skip hidden files/dirs (e.g. .git, .gpg-id)
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }

        if path.is_dir() {
            collect_entries(root, &path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("gpg") {
            if let Some(name) = relative_name(root, &path) {
                out.push(PassEntry { name });
            }
        }
    }
}

/// Strip the store root prefix and the `.gpg` extension, return a slash-separated name.
fn relative_name(root: &Path, file: &Path) -> Option<String> {
    let rel: PathBuf = file.strip_prefix(root).ok()?.to_path_buf();
    // Drop the .gpg extension from the last component
    let without_ext = rel.with_extension("");
    without_ext.to_str().map(|s| s.to_string())
}

/// Copy the first line (the password) of a pass entry to the clipboard,
/// marked with `org.nspasteboard.ConcealedType` so clipboard history managers
/// (Raycast, Pasta, Yippy, etc.) skip recording it.
///
/// Strategy: decrypt with `pass show <name>` (stdout), grab line 1 in Rust,
/// then write to NSPasteboard ourselves with the concealment type set.
/// Returns true on success.
pub fn copy_password_client(name: &str) -> bool {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };

    let path_env = format!(
        "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}/.local/bin",
        home.display()
    );

    let gnupghome = std::env::var("GNUPGHOME")
        .unwrap_or_else(|_| home.join(".gnupg").to_string_lossy().into_owned());

    // Decrypt to stdout — pinentry-mac still works because stderr is inherited.
    let output = std::process::Command::new("pass")
        .args(["show", name])
        .env("HOME",      home.to_str().unwrap_or("/"))
        .env("PATH",      &path_env)
        .env("GNUPGHOME", &gnupghome)
        .env("PINENTRY_USER_DATA", "USE_CURSES:0")
        .env_remove("GPG_TTY")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped())
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };

    // First line of the decrypted file is the password.
    let text = match std::str::from_utf8(&output.stdout) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let password = match text.lines().next() {
        Some(l) => l.to_string(),
        None => return false,
    };

    // Write to NSPasteboard with org.nspasteboard.ConcealedType so clipboard
    // history managers see the concealment marker and skip recording.
    write_concealed_to_pasteboard(&password)
}

/// Write `text` to the general pasteboard as plain UTF-8 text, but also set
/// the `org.nspasteboard.ConcealedType` marker so history managers skip it.
fn write_concealed_to_pasteboard(text: &str) -> bool {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;

    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();

        // The concealment marker — no data needed, presence of the type is the signal.
        let concealed_type = NSString::from_str("org.nspasteboard.ConcealedType");
        // The actual password as plain text.
        let string_type = NSPasteboardTypeString;
        let ns_text = NSString::from_str(text);

        // Write both items in one declareTypes call so they land in the same
        // pasteboard change count and are atomic.
        let types = objc2_foundation::NSArray::from_retained_slice(&[
            objc2::rc::Retained::cast_unchecked(concealed_type),
            objc2::rc::Retained::cast_unchecked(string_type.to_owned()),
        ]);
        pb.declareTypes_owner(&types, None);
        pb.setString_forType(&ns_text, string_type);
    }

    true
}

