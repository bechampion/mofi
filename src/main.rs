mod apps;
mod clipboard;
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
        _ => run_daemon(),
    }
}

// ── --client ──────────────────────────────────────────────────────────────────

fn client_main() {
    let mut stream = match UnixStream::connect(SOCK_FILE) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("rofi-mac: daemon not running (socket not found at {})", SOCK_FILE);
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
            eprintln!("rofi-mac: no PID file at {}", PID_FILE);
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
        eprintln!("rofi-mac: failed to decrypt {}", entry);
        std::process::exit(1);
    }
}

// ── Daemon ────────────────────────────────────────────────────────────────────

fn run_daemon() -> eframe::Result<()> {
    let pid = std::process::id();
    fs::write(PID_FILE, pid.to_string()).ok();
    let _ = unsafe { libc::atexit(cleanup_files) };

    let toggle = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGUSR1, Arc::clone(&toggle))
        .expect("Failed to register SIGUSR1 handler");

    let _ = fs::remove_file(SOCK_FILE);
    let listener = UnixListener::bind(SOCK_FILE).expect("Failed to bind Unix socket");

    let pending_entry: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
    let pending_entry_sock = Arc::clone(&pending_entry);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let pending = Arc::clone(&pending_entry_sock);
            std::thread::spawn(move || handle_client(stream, pending));
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
            Box::new(ui::RofiApp::new(cc, toggle, pending_entry))
        }),
    )
}

fn handle_client(
    mut stream: UnixStream,
    pending_entry: Arc<Mutex<Option<Option<String>>>>,
) {
    // Read the ready line (we don't need its content anymore).
    let mut buf = String::new();
    BufReader::new(&stream).read_line(&mut buf).ok();

    // Wait for the UI to set pending_entry (pass entry name or None on dismiss).
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

extern "C" fn cleanup_files() {
    let _ = fs::remove_file(PID_FILE);
    let _ = fs::remove_file(SOCK_FILE);
}
