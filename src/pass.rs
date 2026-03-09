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
/// This version is for the CLIENT — uses `pass show` to decrypt, then
/// copies the first line to the clipboard via wl-copy (Wayland) or
/// xclip (X11) on Linux, or pbcopy on macOS.
///
/// On Linux the clipboard is automatically cleared after `CLIP_TIMEOUT`
/// seconds — but only if it still contains the password we set (so we
/// don't nuke something the user copied in the meantime).
///
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
    cmd.args(["show", name])
        .env("HOME", home.to_str().unwrap_or("/"))
        .env("PATH", &path_env)
        .env("GNUPGHOME", &gnupghome)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped());

    #[cfg(target_os = "macos")]
    {
        cmd.env("PINENTRY_USER_DATA", "USE_CURSES:0");
        cmd.env_remove("GPG_TTY");
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(display) = std::env::var("DISPLAY") {
            cmd.env("DISPLAY", display);
        }
        if let Ok(wd) = std::env::var("WAYLAND_DISPLAY") {
            cmd.env("WAYLAND_DISPLAY", wd);
        }
        cmd.env("GPG_TTY", std::env::var("GPG_TTY").unwrap_or_default());
    }

    let output = match cmd.output() {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            eprintln!(
                "mofi: pass show failed (exit {:?}): {}",
                o.status.code(),
                String::from_utf8_lossy(&o.stderr)
            );
            return false;
        }
        Err(e) => {
            eprintln!("mofi: failed to spawn pass: {}", e);
            return false;
        }
    };

    // Take only the first line (the password itself).
    let plaintext = match std::str::from_utf8(&output.stdout) {
        Ok(s) => s.lines().next().unwrap_or("").to_string(),
        Err(_) => return false,
    };

    if plaintext.is_empty() {
        return false;
    }

    // Copy to clipboard using the appropriate tool for the platform.
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        if let Ok(mut child) = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(plaintext.as_bytes());
            }
            return child.wait().map(|s| s.success()).unwrap_or(false);
        }
        false
    }

    #[cfg(target_os = "linux")]
    {
        use std::io::Write;
        // Try wl-copy (Wayland) first, then xclip (X11).
        // Explicitly pass Wayland session vars — the client process may have
        // a stripped environment when launched via a compositor keybind.
        for (prog, args) in &[
            ("wl-copy", vec![] as Vec<&str>),
            ("xclip", vec!["-selection", "clipboard"]),
        ] {
            let mut child_cmd = std::process::Command::new(prog);
            child_cmd.args(args).stdin(std::process::Stdio::piped());
            // Ensure Wayland/X11 session vars are present.
            for var in &["WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "DISPLAY"] {
                if let Ok(val) = std::env::var(var) {
                    child_cmd.env(var, val);
                }
            }
            if let Ok(mut child) = child_cmd.spawn() {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(plaintext.as_bytes());
                }
                let ok = child.wait().map(|s| s.success()).unwrap_or(false);
                if ok {
                    // Schedule clipboard clear after timeout.
                    schedule_clipboard_clear(plaintext);
                    return true;
                }
            }
        }
        false
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    false
}

// ── Clipboard auto-clear for passwords ────────────────────────────────────────

/// How many seconds to keep a password in the clipboard before clearing it.
/// Matches the default `pass -c` behaviour (45 s).
const CLIP_TIMEOUT_SECS: u64 = 45;

/// Spawn a **detached child process** that sleeps for `CLIP_TIMEOUT_SECS`,
/// then clears the clipboard — but only if it still contains the password
/// we set.  A child process is used instead of a thread because the client
/// binary may exit (process::exit) before a thread would complete.
#[cfg(target_os = "linux")]
fn schedule_clipboard_clear(password: String) {
    // Build a shell script that:
    //   1. sleeps N seconds
    //   2. reads clipboard via wl-paste
    //   3. compares to expected value
    //   4. clears only if it matches
    //
    // We pass the expected password via an environment variable so it
    // doesn't appear in the process argv (visible in `ps`).
    let script = format!(
        r#"sleep {secs}
current="$(wl-paste --no-newline 2>/dev/null)"
if [ "$current" = "$MOFI_CLIP_EXPECT" ]; then
  wl-copy --clear 2>/dev/null
fi"#,
        secs = CLIP_TIMEOUT_SECS
    );

    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", &script])
        .env("MOFI_CLIP_EXPECT", &password)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // Pass through session vars.
    for var in &["WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "DISPLAY"] {
        if let Ok(val) = std::env::var(var) {
            cmd.env(var, val);
        }
    }

    // Fire and forget — the child is detached and survives our exit.
    let _ = cmd.spawn();
}
