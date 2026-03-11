mod apps;
mod clipboard;
mod config;
mod files;
mod frecency;
mod launcher;
mod pass;
mod ui;

#[cfg(target_os = "linux")]
mod layer_window;

#[cfg(target_os = "linux")]
mod tray;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const PID_FILE: &str = "/tmp/mofi.pid";
const SOCK_FILE: &str = "/tmp/mofi.sock";
const LOCK_FILE: &str = "/tmp/mofi.lock";

/// Send SIGUSR1 to our own process (called from handle_client threads so that
/// shared state is fully written *before* the UI thread wakes on the signal).
fn send_sigusr1_to_self() {
    let pid = std::process::id() as i32;
    unsafe { libc::kill(pid, libc::SIGUSR1) };
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("--daemon");

    match mode {
        "--client" => {
            #[cfg(target_os = "linux")]
            linux_client_main("apps");
            #[cfg(not(target_os = "linux"))]
            client_main();
        }
        "--password" => {
            #[cfg(target_os = "linux")]
            linux_client_main("pass");
            #[cfg(not(target_os = "linux"))]
            show_tab_main("pass");
        }
        "--clipboard" => {
            #[cfg(target_os = "linux")]
            linux_client_main("clip");
            #[cfg(not(target_os = "linux"))]
            show_tab_main("clip");
        }
        "--files" => {
            #[cfg(target_os = "linux")]
            linux_client_main("files");
            #[cfg(not(target_os = "linux"))]
            show_tab_main("files");
        }
        "--pass" => {
            show_tab_main("pass");
        }
        "--clip" => {
            show_tab_main("clip");
        }
        "--input" => {
            input_client_main();
        }
        "--themes" => {
            themes_client_main();
        }
        "--install" => {
            install_main();
        }
        "--restart" => {
            restart_main();
        }
        _ => run_daemon(),
    }
}

fn connect_with_retry() -> Option<UnixStream> {
    // Retry for up to ~2 seconds in case the daemon is still starting up.
    for _ in 0..20 {
        match UnixStream::connect(SOCK_FILE) {
            Ok(s) => return Some(s),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    None
}

// ── --pass / --clip (show on a specific tab, wait for selection) ──────────────

fn show_tab_main(tab: &str) {
    let mut stream = match connect_with_retry() {
        Some(s) => s,
        None => {
            eprintln!(
                "mofi: daemon not running (socket not found at {})",
                SOCK_FILE
            );
            std::process::exit(1);
        }
    };

    // Send "tab:<name>" — daemon opens on that tab and waits for a selection,
    // then writes the selected entry name back over the socket.
    // The daemon's handle_client sends SIGUSR1 to itself after setting pending_mode.
    let msg = format!("tab:{}\n", tab);
    stream.write_all(msg.as_bytes()).ok();

    // Wait for the daemon to send back the selected entry name (or empty = cancel).
    let mut response = String::new();
    BufReader::new(&stream).read_line(&mut response).ok();
    let entry = response.trim().to_string();

    if entry.is_empty() {
        return;
    }

    // Only pass entries need decryption; clip entries are already in clipboard.
    if tab == "pass" {
        if pass::copy_password_client(&entry) {
            println!("Copied {}", entry);
        } else {
            eprintln!("mofi: failed to decrypt {}", entry);
            std::process::exit(1);
        }
    }
}

// ── --client daemonless (Linux one-shot, fallback only) ──────────────────────

#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn client_oneshot(initial_mode: ui::Mode) {
    let app = ui::RofiApp::new_oneshot_with_mode(initial_mode);
    let app = layer_window::run_oneshot(app);

    // Inspect the result and act on it.
    match app.oneshot_result {
        Some(Some(entry)) => {
            // Pass entry selected — decrypt and copy.
            if pass::copy_password_client(&entry) {
                println!("Copied {}", entry);
                std::process::exit(0);
            } else {
                eprintln!("mofi: failed to decrypt {}", entry);
                std::process::exit(1);
            }
        }
        Some(None) => {
            // Cancelled or non-pass selection (app/clip handled inline).
            std::process::exit(0);
        }
        None => {
            // Should not happen — means the window closed without setting a result.
            std::process::exit(1);
        }
    }
}

// ── --client / --password / --clipboard via daemon (Linux) ───────────────────
//
// Connects to the running daemon over the Unix socket, asking it to show on a
// specific tab.  If the daemon is not running, spawns it in the background and
// retries.  On success, decrypts and copies the password if a pass entry was
// selected.

#[cfg(target_os = "linux")]
fn linux_client_main(tab: &str) {
    let stream = match connect_or_start_daemon() {
        Some(s) => s,
        None => {
            eprintln!("mofi: could not connect to daemon after auto-start");
            std::process::exit(1);
        }
    };
    // Delegate to the shared show_tab_main logic.
    run_tab_client(stream, tab);
}

/// Try to connect to the daemon socket.  If not running, spawn `mofi --daemon`
/// detached and retry with a longer timeout (~5 s).
#[cfg(target_os = "linux")]
fn connect_or_start_daemon() -> Option<UnixStream> {
    // Fast path: daemon already running.
    if let Ok(s) = UnixStream::connect(SOCK_FILE) {
        return Some(s);
    }

    // Spawn the daemon detached (double-fork via nohup equivalent).
    let bin = std::env::current_exe().expect("cannot resolve binary path");
    std::process::Command::new(&bin)
        .arg("--daemon")
        // Detach from our session so it survives after we exit.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // Retry for up to 5 seconds.
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(s) = UnixStream::connect(SOCK_FILE) {
            return Some(s);
        }
    }
    None
}

/// Send `tab:<tab>\n`, wait for a result line (the daemon sends SIGUSR1 to
/// itself after receiving the message), then decrypt if a pass entry was returned.
#[cfg(target_os = "linux")]
fn run_tab_client(mut stream: UnixStream, tab: &str) {
    let msg = format!("tab:{}\n", tab);
    stream.write_all(msg.as_bytes()).ok();

    // The daemon's handle_client thread now sends SIGUSR1 after setting
    // pending_mode, so we do NOT need to kill() here — just wait for the result.

    // Wait for the daemon to send back the selected entry name (or empty = cancel).
    let mut response = String::new();
    BufReader::new(&stream).read_line(&mut response).ok();
    let entry = response.trim().to_string();

    if entry.is_empty() {
        std::process::exit(0);
    }

    // Only pass entries need decryption.
    if tab == "pass" {
        if pass::copy_password_client(&entry) {
            println!("Copied {}", entry);
            std::process::exit(0);
        } else {
            eprintln!("mofi: failed to decrypt {}", entry);
            std::process::exit(1);
        }
    }
    // Apps / clipboard are handled inline by the daemon — nothing more to do.
    std::process::exit(0);
}

// ── --client (existing daemon toggle / pass flow — macOS / fallback) ─────────

#[cfg_attr(target_os = "linux", allow(dead_code))]
fn client_main() {
    let mut stream = match connect_with_retry() {
        Some(s) => s,
        None => {
            eprintln!(
                "mofi: daemon not running (socket not found at {})",
                SOCK_FILE
            );
            std::process::exit(1);
        }
    };

    stream.write_all(b"ready\n").ok();

    // The daemon's handle_client thread sends SIGUSR1 to itself after receiving "ready".

    let mut response = String::new();
    BufReader::new(&stream).read_line(&mut response).ok();
    let entry = response.trim().to_string();

    if entry.is_empty() {
        return;
    }

    if pass::copy_password_client(&entry) {
        println!("Copied {}", entry);
    } else {
        eprintln!("mofi: failed to decrypt {}", entry);
        std::process::exit(1);
    }
}

// ── --input (pipe-select flow) ────────────────────────────────────────────────

fn input_client_main() {
    // Read all lines from stdin.
    let stdin = std::io::stdin();
    let lines: Vec<String> = stdin
        .lock()
        .lines()
        .filter_map(|l| l.ok())
        .filter(|l| !l.is_empty())
        .collect();

    if lines.is_empty() {
        std::process::exit(1);
    }

    let mut stream = match connect_with_retry() {
        Some(s) => s,
        None => {
            eprintln!(
                "mofi: daemon not running (socket not found at {})",
                SOCK_FILE
            );
            std::process::exit(1);
        }
    };

    // Encode as: "input\t<item1>\t<item2>\t...\n"
    // Items that contain tabs have them replaced with spaces (display only anyway).
    let encoded: Vec<String> = lines.iter().map(|l| l.replace('\t', " ")).collect();
    let message = format!("input\t{}\n", encoded.join("\t"));
    stream.write_all(message.as_bytes()).ok();

    // The daemon's handle_client thread sends SIGUSR1 to itself after posting items.

    // Wait for the daemon's response.
    let mut response = String::new();
    BufReader::new(&stream).read_line(&mut response).ok();
    let resp = response.trim();

    if let Some(selected) = resp.strip_prefix("ok:") {
        print!("{}", selected);
        std::process::exit(0);
    } else {
        // "cancel" or anything else → exit 1, no output.
        std::process::exit(1);
    }
}

// ── --themes (theme picker) ───────────────────────────────────────────────────

fn themes_client_main() {
    use config::{theme_by_name, Config, THEME_NAMES};

    // Build the display list: "* kanagawa" for the current theme, bare names for others.
    let current = Config::load();
    let active = current.active_theme_name().to_string();

    let lines: Vec<String> = THEME_NAMES
        .iter()
        .map(|&name| {
            if name == active {
                format!("* {}", name)
            } else {
                name.to_string()
            }
        })
        .collect();

    // Send via the --input protocol: pipe to ourselves as a subprocess.
    // We talk directly to the daemon socket so we don't need to fork.
    let mut stream = match connect_with_retry() {
        Some(s) => s,
        None => {
            eprintln!("mofi: daemon not running");
            std::process::exit(1);
        }
    };

    // Send via the themes protocol so the daemon knows to enable live preview.
    let encoded: Vec<String> = lines.iter().map(|l| l.replace('\t', " ")).collect();
    let message = format!("themes\t{}\n", encoded.join("\t"));
    stream.write_all(message.as_bytes()).ok();

    // The daemon's handle_client thread sends SIGUSR1 to itself after posting items.

    // Wait for selection.
    let mut response = String::new();
    BufReader::new(&stream).read_line(&mut response).ok();
    let resp = response.trim();

    if let Some(selected) = resp.strip_prefix("ok:") {
        // Strip the "* " prefix if present.
        let name = selected.trim_start_matches("* ");
        // Validate it's a real theme name.
        if !THEME_NAMES.contains(&name) {
            eprintln!("mofi: unknown theme '{}'", name);
            std::process::exit(1);
        }
        // Write to config.
        let mut cfg = Config::load();
        cfg.theme = name.to_string();
        cfg.save();
        println!("Theme set to '{}'", name);
        // Verify the theme exists (just a sanity check — also triggers dead_code lint suppression).
        let _ = theme_by_name(name);
    }
    // Escape → exit silently with 0 (no change).
}

// ── --install ─────────────────────────────────────────────────────────────────

fn install_main() {
    #[cfg(target_os = "macos")]
    install_macos();

    #[cfg(target_os = "linux")]
    install_linux();

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        eprintln!("mofi: --install is not supported on this platform");
        std::process::exit(1);
    }
}

#[cfg(target_os = "macos")]
fn install_macos() {
    use std::path::PathBuf;

    // Resolve the absolute path to this binary.
    let bin = std::env::current_exe()
        .expect("cannot resolve current binary path")
        .canonicalize()
        .expect("cannot canonicalize binary path");
    let bin_str = bin.to_string_lossy();

    println!("mofi --install (macOS)");
    println!("  binary: {}", bin_str);

    // ── 1. launchd plist ──────────────────────────────────────────────────────
    let launch_agents = PathBuf::from(shellexpand::tilde("~/Library/LaunchAgents").as_ref());
    fs::create_dir_all(&launch_agents).ok();
    let plist_path = launch_agents.join("com.user.mofi.plist");

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.user.mofi</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>--daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/mofi-daemon.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/mofi-daemon.log</string>
</dict>
</plist>
"#,
        bin = bin_str
    );

    let plist_existed = plist_path.exists();
    if plist_existed {
        // Unload the old agent before overwriting.
        std::process::Command::new("launchctl")
            .args(["unload", plist_path.to_str().unwrap()])
            .status()
            .ok();
        println!("  [plist] unloaded existing agent");
    }

    fs::write(&plist_path, &plist).unwrap_or_else(|e| {
        eprintln!("mofi: failed to write plist: {}", e);
        std::process::exit(1);
    });
    println!("  [plist] written → {}", plist_path.display());

    // Load the new agent.
    let status = std::process::Command::new("launchctl")
        .args(["load", plist_path.to_str().unwrap()])
        .status();
    match status {
        Ok(s) if s.success() => println!("  [plist] loaded (daemon is now running)"),
        Ok(s) => eprintln!("  [plist] launchctl load exited {}", s),
        Err(e) => eprintln!("  [plist] launchctl error: {}", e),
    }

    // ── 2. skhd hotkey ───────────────────────────────────────────────────────
    let skhdrc = PathBuf::from(shellexpand::tilde("~/.skhdrc").as_ref());

    let hotkey_line = format!(
        "cmd - space : {} --client\ncmd + shift - p : {} --pass\ncmd + shift - y : {} --clip",
        bin_str, bin_str, bin_str
    );
    let marker = "# mofi";

    let existing = fs::read_to_string(&skhdrc).unwrap_or_default();

    if existing.contains(marker) {
        println!("  [skhd]  mofi entry already present in ~/.skhdrc — skipping");
    } else {
        // Append the hotkey block.
        let block = format!("\n{}\n{}\n", marker, hotkey_line);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&skhdrc)
            .unwrap_or_else(|e| {
                eprintln!("mofi: cannot open ~/.skhdrc: {}", e);
                std::process::exit(1);
            });
        use std::io::Write as _;
        file.write_all(block.as_bytes()).unwrap_or_else(|e| {
            eprintln!("mofi: cannot write ~/.skhdrc: {}", e);
            std::process::exit(1);
        });
        println!("  [skhd]  appended hotkey → ~/.skhdrc");

        // Reload skhd if it is running.
        let reload = std::process::Command::new("skhd").arg("--reload").status();
        match reload {
            Ok(s) if s.success() => println!("  [skhd]  reloaded"),
            Ok(_) | Err(_) => {
                println!("  [skhd]  skhd not running — start it with: skhd --start-service")
            }
        }
    }

    // ── 3. mofi config dir ───────────────────────────────────────────────────
    install_config();

    println!();
    println!("Done. mofi is installed and running.");
    println!("  Open with: Cmd+Space");
    println!("  Pick a theme: mofi --themes");
}

#[cfg(target_os = "linux")]
fn install_linux() {
    use std::path::PathBuf;

    let bin = std::env::current_exe()
        .expect("cannot resolve current binary path")
        .canonicalize()
        .expect("cannot canonicalize binary path");
    let bin_str = bin.to_string_lossy();

    println!("mofi --install (Linux)");
    println!("  binary: {}", bin_str);

    // ── 1. systemd user service ───────────────────────────────────────────────
    let systemd_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(shellexpand::tilde("~/.config").as_ref()))
        .join("systemd/user");
    fs::create_dir_all(&systemd_dir).ok();
    let service_path = systemd_dir.join("mofi.service");

    let service = format!(
        r#"[Unit]
Description=mofi launcher daemon
After=graphical-session.target
PartOf=graphical-session.target

[Service]
ExecStart={bin} --daemon
Restart=on-failure
RestartSec=3s

[Install]
WantedBy=graphical-session.target
"#,
        bin = bin_str
    );

    fs::write(&service_path, &service).unwrap_or_else(|e| {
        eprintln!("mofi: failed to write service file: {}", e);
        std::process::exit(1);
    });
    println!("  [systemd] written → {}", service_path.display());

    // Reload and enable.
    let reload = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    match reload {
        Ok(s) if s.success() => println!("  [systemd] daemon-reload ok"),
        _ => eprintln!("  [systemd] daemon-reload failed (is systemd --user running?)"),
    }

    let enable = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "mofi.service"])
        .status();
    match enable {
        Ok(s) if s.success() => println!("  [systemd] enabled & started mofi.service"),
        Ok(s) => eprintln!("  [systemd] enable/start exited {}", s),
        Err(e) => eprintln!("  [systemd] error: {}", e),
    }

    // ── 2. hotkey hint ────────────────────────────────────────────────────────
    println!();
    println!("  The daemon starts automatically on first use (--client auto-starts it).");
    println!(
        "  To pre-start it (faster first launch): {} --daemon &",
        bin_str
    );
    println!();
    println!("  Hotkey: bind a key in your compositor/WM to run:");
    println!("    {} --client     (launcher)", bin_str);
    println!("    {} --password   (jump to Pass tab)", bin_str);
    println!("    {} --clipboard  (jump to Clipboard tab)", bin_str);
    println!("    {} --files      (jump to Files tab)", bin_str);
    println!("  Examples:");
    println!(
        "    Hyprland  → bind = SUPER, Space, exec, {} --client",
        bin_str
    );
    println!(
        "    Sway      → bindsym Mod4+space exec {} --client",
        bin_str
    );
    println!(
        "    keyd / sxhkd — add a binding to call {} --client",
        bin_str
    );

    // ── 3. mofi config dir ───────────────────────────────────────────────────
    install_config();

    println!();
    println!("Done. mofi is installed.");
    println!("  Pick a theme: mofi --themes");
}

fn install_config() {
    use std::path::PathBuf;
    let config_dir = PathBuf::from(shellexpand::tilde("~/.config/mofi").as_ref());
    fs::create_dir_all(&config_dir).ok();
    let config_file = config_dir.join("config.toml");
    if !config_file.exists() {
        fs::write(&config_file, "theme = \"kanagawa\"\n")
            .unwrap_or_else(|e| eprintln!("mofi: cannot write config: {}", e));
        println!("  [config] created → {}", config_file.display());
    } else {
        println!("  [config] already exists — skipping");
    }
}

// ── --restart ─────────────────────────────────────────────────────────────────

fn restart_main() {
    #[cfg(target_os = "macos")]
    restart_macos();

    #[cfg(target_os = "linux")]
    restart_linux();

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        eprintln!("mofi: --restart is not supported on this platform");
        std::process::exit(1);
    }
}

#[cfg(target_os = "macos")]
fn restart_macos() {
    use std::path::PathBuf;

    let plist =
        PathBuf::from(shellexpand::tilde("~/Library/LaunchAgents/com.user.mofi.plist").as_ref());

    if !plist.exists() {
        eprintln!(
            "mofi: plist not found at {} — run `mofi --install` first",
            plist.display()
        );
        std::process::exit(1);
    }

    let plist_str = plist.to_str().unwrap();

    print!("  [restart] unloading... ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let unload = std::process::Command::new("launchctl")
        .args(["unload", plist_str])
        .status();
    match unload {
        Ok(s) if s.success() => println!("ok"),
        Ok(s) => println!("exited {}", s),
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }

    print!("  [restart] loading...   ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let load = std::process::Command::new("launchctl")
        .args(["load", plist_str])
        .status();
    match load {
        Ok(s) if s.success() => println!("ok"),
        Ok(s) => {
            eprintln!("launchctl load exited {}", s);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }

    // Give the daemon a moment to write its PID file, then confirm.
    std::thread::sleep(std::time::Duration::from_millis(400));
    match fs::read_to_string(PID_FILE) {
        Ok(pid) => println!("  [restart] daemon running (PID {})", pid.trim()),
        Err(_) => println!("  [restart] daemon started (PID file not yet written)"),
    }
}

#[cfg(target_os = "linux")]
fn restart_linux() {
    print!("  [restart] restarting mofi.service... ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let status = std::process::Command::new("systemctl")
        .args(["--user", "restart", "mofi.service"])
        .status();
    match status {
        Ok(s) if s.success() => println!("ok"),
        Ok(s) => {
            eprintln!("systemctl restart exited {}", s);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }

    std::thread::sleep(std::time::Duration::from_millis(400));
    match fs::read_to_string(PID_FILE) {
        Ok(pid) => println!("  [restart] daemon running (PID {})", pid.trim()),
        Err(_) => println!("  [restart] daemon started (PID file not yet written)"),
    }
}

/// Message sent from the socket thread to the UI thread.
pub enum SocketMsg {
    /// Regular toggle (pass mode): the UI will show and set pending_entry when done.
    Ready,
    /// Input-select mode: show the given items and return the selected one.
    InputItems(Vec<String>),
}

fn run_daemon() {
    // ── Single-instance guard ─────────────────────────────────────────────
    // Acquire an exclusive flock on a lock file.  If another daemon already
    // holds the lock, print a message and exit.  The OS releases the lock
    // automatically when our process exits (even on crash / SIGKILL), so
    // there are no stale-lock problems.
    use std::os::unix::io::AsRawFd;
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(LOCK_FILE)
        .expect("failed to open lock file");
    let lock_fd = lock_file.as_raw_fd();
    let lock_ok = unsafe { libc::flock(lock_fd, libc::LOCK_EX | libc::LOCK_NB) };
    if lock_ok != 0 {
        // Another daemon holds the lock.
        let existing_pid = fs::read_to_string(PID_FILE).unwrap_or_default();
        eprintln!(
            "mofi: daemon already running (pid {}). Not starting a second instance.",
            existing_pid.trim()
        );
        std::process::exit(0);
    }
    // Keep `lock_file` alive for the lifetime of the process — dropping it
    // would close the fd and release the flock.
    // (It's moved into `_lock_guard` so it lives until the end of this function.)
    let _lock_guard = lock_file;

    // Bind the socket FIRST so the PID file is only written once the daemon is
    // ready to accept connections.  This eliminates the race where a client
    // reads a valid PID but the socket is not yet listening.
    let _ = fs::remove_file(SOCK_FILE);
    let listener = UnixListener::bind(SOCK_FILE).expect("Failed to bind Unix socket");

    // Write PID now that we are listening.
    let pid = std::process::id();
    fs::write(PID_FILE, pid.to_string()).ok();

    // Clean up on SIGTERM (systemctl stop).  We register a flag and check it
    // in the event loop so cleanup happens in the main thread — no background
    // thread that could race with the next daemon instance's startup.
    let sigterm_fired = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&sigterm_fired))
        .expect("Failed to register SIGTERM handler");

    // Each SIGUSR1 increments the counter; the UI thread drains it one toggle
    // at a time so rapid double-presses are not collapsed into a single event.
    let toggle = Arc::new(AtomicUsize::new(0));
    {
        let t = Arc::clone(&toggle);
        unsafe {
            signal_hook::low_level::register(libc::SIGUSR1, move || {
                t.fetch_add(1, Ordering::Relaxed);
            })
            .expect("Failed to register SIGUSR1 handler");
        }
    }

    // Shared slot for the pass-entry result (existing mechanism).
    let pending_entry: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
    let pending_entry_sock = Arc::clone(&pending_entry);

    // Shared slot for incoming input-select items + result channel.
    let pending_input: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
    let pending_input_sock = Arc::clone(&pending_input);

    // Flag: true when the pending input is a themes invocation (live preview).
    let pending_input_is_themes: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let pending_input_is_themes_sock = Arc::clone(&pending_input_is_themes);

    // Result from input-select: None = not ready, Some(None) = cancelled, Some(Some(s)) = selected.
    let input_result: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
    let input_result_sock = Arc::clone(&input_result);

    // Shared slot for "show on tab" requests (--pass / --clip).
    let pending_mode: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let pending_mode_sock = Arc::clone(&pending_mode);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let pending = Arc::clone(&pending_entry_sock);
            let p_input = Arc::clone(&pending_input_sock);
            let p_is_themes = Arc::clone(&pending_input_is_themes_sock);
            let i_result = Arc::clone(&input_result_sock);
            let p_mode = Arc::clone(&pending_mode_sock);
            std::thread::spawn(move || {
                handle_client(stream, pending, p_input, p_is_themes, i_result, p_mode)
            });
        }
    });

    #[cfg(target_os = "linux")]
    {
        // Spawn the system-tray icon (StatusNotifierItem via D-Bus).
        let tray_handle = tray::spawn_tray();

        let app = ui::RofiApp::new_linux(
            toggle,
            pending_entry,
            pending_input,
            input_result,
            pending_input_is_themes,
            pending_mode,
            tray_handle,
        );
        layer_window::run(app, sigterm_fired);
        // Clean up runtime files when the event loop exits (SIGTERM or normal close).
        let _ = fs::remove_file(SOCK_FILE);
        let _ = fs::remove_file(PID_FILE);
        let _ = fs::remove_file(LOCK_FILE);
    }

    #[cfg(target_os = "macos")]
    {
        use eframe::egui as feframe_egui;
        let options = eframe::NativeOptions {
            viewport: feframe_egui::ViewportBuilder::default()
                .with_inner_size([1.0, 1.0])
                .with_min_inner_size([1.0, 1.0])
                .with_decorations(false)
                .with_transparent(true)
                .with_always_on_top()
                .with_resizable(true)
                .with_app_id("mofi"),
            centered: false,
            vsync: false,
            multisampling: 0,
            depth_buffer: 0,
            hardware_acceleration: eframe::HardwareAcceleration::Preferred,
            ..Default::default()
        };

        eframe::run_native(
            "mofi",
            options,
            Box::new(move |cc| {
                Box::new(ui::RofiApp::new(
                    cc,
                    toggle,
                    pending_entry,
                    pending_input,
                    input_result,
                    pending_input_is_themes,
                    pending_mode,
                ))
            }),
        )
        .unwrap();
    }
}

fn handle_client(
    mut stream: UnixStream,
    pending_entry: Arc<Mutex<Option<Option<String>>>>,
    pending_input: Arc<Mutex<Option<Vec<String>>>>,
    pending_input_is_themes: Arc<Mutex<bool>>,
    input_result: Arc<Mutex<Option<Option<String>>>>,
    pending_mode: Arc<Mutex<Option<String>>>,
) {
    let mut buf = String::new();
    BufReader::new(&stream).read_line(&mut buf).ok();
    let line = buf.trim_end_matches('\n').to_string();

    // show:<tab> — legacy fire-and-forget protocol, kept for compatibility.
    if let Some(tab) = line.strip_prefix("show:") {
        *pending_mode.lock().unwrap() = Some(tab.to_string());
        send_sigusr1_to_self();
        return;
    }

    // tab:<tab> — open on a specific tab and wait for the user's selection.
    // Used by --pass and --clip so the client can act on the result.
    if let Some(tab) = line.strip_prefix("tab:") {
        *pending_mode.lock().unwrap() = Some(tab.to_string());
        // Send SIGUSR1 NOW, after pending_mode is set — this eliminates the
        // race where the UI wakes on SIGUSR1 before pending_mode is written.
        send_sigusr1_to_self();
        // Fall through to the pending_entry wait loop below.
    } else if line.starts_with("input\t") || line.starts_with("themes\t") {
        let is_themes = line.starts_with("themes\t");
        let rest = if is_themes {
            &line["themes\t".len()..]
        } else {
            &line["input\t".len()..]
        };
        let items: Vec<String> = rest.split('\t').map(|s| s.to_string()).collect();

        // Clear any previous result, set the themes flag, then post the items.
        {
            *input_result.lock().unwrap() = None;
        }
        {
            *pending_input_is_themes.lock().unwrap() = is_themes;
        }
        {
            *pending_input.lock().unwrap() = Some(items);
        }
        // Send SIGUSR1 after all shared state is written.
        send_sigusr1_to_self();

        // Wait for the UI to post a result (selected string or None for cancel).
        let result = loop {
            {
                let mut lock = input_result.lock().unwrap();
                if let Some(val) = lock.take() {
                    break val;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };

        let response = match result {
            Some(selected) => format!("ok:{}\n", selected),
            None => "cancel\n".to_string(),
        };
        stream.write_all(response.as_bytes()).ok();
        return;
    }

    // "ready" (--client) or "tab:<x>" (--pass / --clip) — wait for selection.
    {
        // For "ready" (plain --client / macOS): send SIGUSR1 now that state is set.
        // For "tab:<x>" the signal was already sent above.
        if line == "ready" {
            send_sigusr1_to_self();
        }
        // Clear any stale value left from a previous session.
        // Some(None) means hide() fired — could be from this session's hide-before-wait
        // race, OR stale from a previous session.  Either way we clear it: if it was
        // a true race the SIGUSR1 already sent the window visible so we still need to
        // wait for the new selection; if it was stale we obviously must clear it.
        {
            let mut l = pending_entry.lock().unwrap();
            match *l {
                Some(None) => {
                    // Consume the stale cancel signal and continue waiting for this session.
                    *l = None;
                }
                Some(Some(_)) => {
                    // Stale entry name from a prior session — discard it.
                    *l = None;
                }
                None => {} // normal: no stale value
            }
        }
        let mut waited = 0;
        let entry = loop {
            {
                let mut lock = pending_entry.lock().unwrap();
                if let Some(val) = lock.take() {
                    break val;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            waited += 50;
            if waited > 30_000 {
                break None;
            }
        };

        let response = match entry {
            Some(name) => format!("{}\n", name),
            None => "\n".to_string(),
        };
        stream.write_all(response.as_bytes()).ok();
    }
}
