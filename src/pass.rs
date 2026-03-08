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

/// Copy the first line (the password) of a pass entry to the clipboard.
/// This version is for the CLIENT — uses `pass show -c` which handles
/// pinentry-mac GUI prompting and clipboard copy natively.
/// Returns true on success.
pub fn copy_password_client(name: &str) -> bool {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };

    // Build a sane PATH that includes Homebrew so `gpg`, `pass`, `pinentry-mac`
    // are all resolvable even when launched from a sparse launchd/skhd env.
    let path_env = format!(
        "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}/.local/bin",
        home.display()
    );

    let gnupghome = std::env::var("GNUPGHOME")
        .unwrap_or_else(|_| home.join(".gnupg").to_string_lossy().into_owned());

    // `pass show -c <name>` decrypts, copies the first line to clipboard via
    // pbcopy, and triggers pinentry-mac for the GPG passphrase if needed.
    // stderr is inherited so pinentry-mac can connect to the window server.
    let status = std::process::Command::new("pass")
        .args(["show", "-c", name])
        .env("HOME",      home.to_str().unwrap_or("/"))
        .env("PATH",      &path_env)
        .env("GNUPGHOME", &gnupghome)
        // Tell pinentry to use the GUI (not curses/loopback) even without a TTY.
        .env("PINENTRY_USER_DATA", "USE_CURSES:0")
        // Unset GPG_TTY so gpg-agent doesn't try a curses/tty pinentry.
        .env_remove("GPG_TTY")
        .stdin(std::process::Stdio::null())
        // Do NOT suppress stderr — pinentry-mac needs it to reach the display.
        .stderr(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::null())
        .status();

    matches!(status, Ok(s) if s.success())
}

