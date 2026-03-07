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
/// This version is for the CLIENT — uses `pass show` which correctly sets up
/// the GPG_AGENT_INFO / SSH_AUTH_SOCK environment so pinentry-mac can pop up
/// its GUI dialog even without a TTY.
/// Returns true on success.
pub fn copy_password_client(name: &str) -> bool {
    // `pass show <name>` decrypts and prints the full entry to stdout.
    // We capture stdout, take the first line (the password), and pipe to pbcopy.
    let gpg_agent_socket = gpgconf_agent_socket();

    let mut cmd = std::process::Command::new("pass");
    cmd.arg("show").arg(name);
    cmd.stdin(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    // Ensure the gpg-agent socket is reachable even if the environment is sparse.
    if let Some(sock) = gpg_agent_socket {
        cmd.env("GPG_AGENT_INFO", format!("{}:0:1", sock));
    }

    let output = match cmd.output() {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let password = text.lines().next().unwrap_or("").to_string();
    if password.is_empty() {
        return false;
    }

    use std::io::Write;
    if let Ok(mut child) = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(password.as_bytes());
        }
        let _ = child.wait();
        return true;
    }
    false
}

/// Ask `gpgconf` for the agent socket path (avoids hard-coding ~/.gnupg).
fn gpgconf_agent_socket() -> Option<String> {
    let out = std::process::Command::new("gpgconf")
        .args(["--list-dirs", "agent-socket"])
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

