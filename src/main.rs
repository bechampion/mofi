mod apps;
mod clipboard;
mod config;
mod launcher;
mod pass;
mod ui;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use eframe::egui;
use signal_hook::consts::SIGUSR1;

const PID_FILE: &str = "/tmp/mofi.pid";
const SOCK_FILE: &str = "/tmp/mofi.sock";

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("--daemon");

    match mode {
        "--client" => {
            client_main();
            Ok(())
        }
        "--input" => {
            input_client_main();
            Ok(())
        }
        "--themes" => {
            themes_client_main();
            Ok(())
        }
        "--install" => {
            install_main();
            Ok(())
        }
        "--restart" => {
            restart_main();
            Ok(())
        }
        _ => run_daemon(),
    }
}

// ── --client (existing toggle / pass flow) ────────────────────────────────────

fn client_main() {
    let mut stream = match UnixStream::connect(SOCK_FILE) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("mofi: daemon not running (socket not found at {})", SOCK_FILE);
            std::process::exit(1);
        }
    };

    stream.write_all(b"ready\n").ok();

    match fs::read_to_string(PID_FILE) {
        Ok(contents) => {
            let pid: i32 = contents.trim().parse().expect("Invalid PID");
            unsafe { libc::kill(pid, libc::SIGUSR1) };
        }
        Err(_) => {
            eprintln!("mofi: no PID file at {}", PID_FILE);
            std::process::exit(1);
        }
    }

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

    let mut stream = match UnixStream::connect(SOCK_FILE) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("mofi: daemon not running (socket not found at {})", SOCK_FILE);
            std::process::exit(1);
        }
    };

    // Encode as: "input\t<item1>\t<item2>\t...\n"
    // Items that contain tabs have them replaced with spaces (display only anyway).
    let encoded: Vec<String> = lines.iter().map(|l| l.replace('\t', " ")).collect();
    let message = format!("input\t{}\n", encoded.join("\t"));
    stream.write_all(message.as_bytes()).ok();

    // Signal the daemon to show the window.
    match fs::read_to_string(PID_FILE) {
        Ok(contents) => {
            let pid: i32 = contents.trim().parse().expect("Invalid PID");
            unsafe { libc::kill(pid, libc::SIGUSR1) };
        }
        Err(_) => {
            eprintln!("mofi: no PID file at {}", PID_FILE);
            std::process::exit(1);
        }
    }

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
    let mut stream = match std::os::unix::net::UnixStream::connect(SOCK_FILE) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("mofi: daemon not running");
            std::process::exit(1);
        }
    };

    // Send via the themes protocol so the daemon knows to enable live preview.
    let encoded: Vec<String> = lines.iter().map(|l| l.replace('\t', " ")).collect();
    let message = format!("themes\t{}\n", encoded.join("\t"));
    stream.write_all(message.as_bytes()).ok();

    // Signal the daemon to show the window.
    match fs::read_to_string(PID_FILE) {
        Ok(contents) => {
            let pid: i32 = contents.trim().parse().expect("Invalid PID");
            unsafe { libc::kill(pid, libc::SIGUSR1) };
        }
        Err(_) => {
            eprintln!("mofi: no PID file at {}", PID_FILE);
            std::process::exit(1);
        }
    }

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
    use std::path::PathBuf;

    // Resolve the absolute path to this binary.
    let bin = std::env::current_exe()
        .expect("cannot resolve current binary path")
        .canonicalize()
        .expect("cannot canonicalize binary path");
    let bin_str = bin.to_string_lossy();

    println!("mofi --install");
    println!("  binary: {}", bin_str);

    // ── 1. launchd plist ──────────────────────────────────────────────────────
    let launch_agents = PathBuf::from(
        shellexpand::tilde("~/Library/LaunchAgents").as_ref()
    );
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

    fs::write(&plist_path, &plist)
        .unwrap_or_else(|e| { eprintln!("mofi: failed to write plist: {}", e); std::process::exit(1); });
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

    let hotkey_line = format!("cmd - space : {} --client", bin_str);
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
            .unwrap_or_else(|e| { eprintln!("mofi: cannot open ~/.skhdrc: {}", e); std::process::exit(1); });
        use std::io::Write as _;
        file.write_all(block.as_bytes())
            .unwrap_or_else(|e| { eprintln!("mofi: cannot write ~/.skhdrc: {}", e); std::process::exit(1); });
        println!("  [skhd]  appended hotkey → ~/.skhdrc");

        // Reload skhd if it is running.
        let reload = std::process::Command::new("skhd").arg("--reload").status();
        match reload {
            Ok(s) if s.success() => println!("  [skhd]  reloaded"),
            Ok(_) | Err(_)       => println!("  [skhd]  skhd not running — start it with: skhd --start-service"),
        }
    }

    // ── 3. mofi config dir ───────────────────────────────────────────────────
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

    println!();
    println!("Done. mofi is installed and running.");
    println!("  Open with: Cmd+Space");
    println!("  Pick a theme: mofi --themes");
}

// ── --restart ─────────────────────────────────────────────────────────────────

fn restart_main() {
    use std::path::PathBuf;

    let plist = PathBuf::from(
        shellexpand::tilde("~/Library/LaunchAgents/com.user.mofi.plist").as_ref()
    );

    if !plist.exists() {
        eprintln!("mofi: plist not found at {} — run `mofi --install` first", plist.display());
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
        Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
    }

    print!("  [restart] loading...   ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let load = std::process::Command::new("launchctl")
        .args(["load", plist_str])
        .status();
    match load {
        Ok(s) if s.success() => println!("ok"),
        Ok(s) => { eprintln!("launchctl load exited {}", s); std::process::exit(1); }
        Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
    }

    // Give the daemon a moment to write its PID file, then confirm.
    std::thread::sleep(std::time::Duration::from_millis(400));
    match fs::read_to_string(PID_FILE) {
        Ok(pid) => println!("  [restart] daemon running (PID {})", pid.trim()),
        Err(_)  => println!("  [restart] daemon started (PID file not yet written)"),
    }
}

/// Message sent from the socket thread to the UI thread.
pub enum SocketMsg {
    /// Regular toggle (pass mode): the UI will show and set pending_entry when done.
    Ready,
    /// Input-select mode: show the given items and return the selected one.
    InputItems(Vec<String>),
}

fn run_daemon() -> eframe::Result<()> {
    let pid = std::process::id();
    fs::write(PID_FILE, pid.to_string()).ok();
    let _ = unsafe { libc::atexit(cleanup_files) };

    let toggle = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGUSR1, Arc::clone(&toggle))
        .expect("Failed to register SIGUSR1 handler");

    let _ = fs::remove_file(SOCK_FILE);
    let listener = UnixListener::bind(SOCK_FILE).expect("Failed to bind Unix socket");

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
            std::thread::spawn(move || handle_client(stream, pending, p_input, p_is_themes, i_result));
        }
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 320.0])
            .with_min_inner_size([360.0, 160.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(false)
            .with_active(true)
            .with_visible(false),
        centered: true,
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
            ))
        }),
    )
}

fn handle_client(
    mut stream: UnixStream,
    pending_entry: Arc<Mutex<Option<Option<String>>>>,
    pending_input: Arc<Mutex<Option<Vec<String>>>>,
    pending_input_is_themes: Arc<Mutex<bool>>,
    input_result: Arc<Mutex<Option<Option<String>>>>,
) {
    let mut buf = String::new();
    BufReader::new(&stream).read_line(&mut buf).ok();
    let line = buf.trim_end_matches('\n').to_string();

    if let Some(rest) = line.strip_prefix("input\t").or_else(|| {
        if line.starts_with("themes\t") { Some(&line["themes\t".len()..]) } else { None }
    }) {
        let is_themes = line.starts_with("themes\t");
        let items: Vec<String> = rest.split('\t').map(|s| s.to_string()).collect();

        // Clear any previous result, set the themes flag, then post the items.
        {
            let mut r = input_result.lock().unwrap();
            *r = None;
        }
        {
            let mut flag = pending_input_is_themes.lock().unwrap();
            *flag = is_themes;
        }
        {
            let mut p = pending_input.lock().unwrap();
            *p = Some(items);
        }

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
    } else {
        // Regular "ready" / pass flow.
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

extern "C" fn cleanup_files() {
    let _ = fs::remove_file(PID_FILE);
    let _ = fs::remove_file(SOCK_FILE);
}
