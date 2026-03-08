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
/// pinentry GUI prompting and clipboard copy natively.
/// Returns true on success.
pub fn copy_password_client(name: &str) -> bool {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };

    let gnupghome = std::env::var("GNUPGHOME")
        .unwrap_or_else(|_| home.join(".gnupg").to_string_lossy().into_owned());

    #[cfg(target_os = "macos")]
    let path_env = format!(
        "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}/.local/bin",
        home.display()
    );

    #[cfg(target_os = "linux")]
    let path_env = format!(
        "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}/.local/bin:{}/.local/share/mise/shims",
        home.display(),
        home.display(),
    );

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let path_env = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());

    let mut cmd = std::process::Command::new("pass");
    cmd.args(["show", "-c", name])
        .env("HOME", home.to_str().unwrap_or("/"))
        .env("PATH", &path_env)
        .env("GNUPGHOME", &gnupghome)
        .stdin(std::process::Stdio::null())
        // Do NOT suppress stderr — pinentry needs it to reach the display.
        .stderr(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::null());

    #[cfg(target_os = "macos")]
    {
        // Tell pinentry to use the GUI (not curses/loopback) even without a TTY.
        cmd.env("PINENTRY_USER_DATA", "USE_CURSES:0");
        // Unset GPG_TTY so gpg-agent doesn't try a curses/tty pinentry.
        cmd.env_remove("GPG_TTY");
    }

    #[cfg(target_os = "linux")]
    {
        // On Linux the display variable is needed for GUI pinentry.
        // Inherit DISPLAY and WAYLAND_DISPLAY from the caller if set.
        if let Ok(display) = std::env::var("DISPLAY") {
            cmd.env("DISPLAY", display);
        }
        if let Ok(wd) = std::env::var("WAYLAND_DISPLAY") {
            cmd.env("WAYLAND_DISPLAY", wd);
        }
        // For headless / tty sessions, allow loopback pinentry.
        cmd.env("GPG_TTY", std::env::var("GPG_TTY").unwrap_or_default());
    }

    matches!(cmd.status(), Ok(s) if s.success())
}
