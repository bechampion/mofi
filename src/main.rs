mod apps;
mod clipboard;
mod config;
mod files;
mod frecency;
mod launcher;
mod pass;
mod ui;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
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
        "--pass" => {
            show_tab_main("pass");
            Ok(())
        }
        "--clip" => {
            show_tab_main("clip");
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

// ── --pass / --clip (show on a specific tab, wait for selection) ──────────────

fn show_tab_main(tab: &str) {
    let mut stream = match UnixStream::connect(SOCK_FILE) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("mofi: daemon not running (socket not found at {})", SOCK_FILE);
            std::process::exit(1);
        }
    };

    // Send "tab:<name>" — daemon opens on that tab and waits for a selection,
    // then writes the selected entry name back over the socket.
    let msg = format!("tab:{}\n", tab);
    stream.write_all(msg.as_bytes()).ok();

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

/// Render a square PNG icon — Kanagawa dark background, crystalBlue border,
/// bold "M" in Maple Mono NF Bold — and return the raw PNG bytes.
fn render_menubar_icon() -> Vec<u8> {
    // Very dark violet bg, border same color as bg, fujiWhite M
    render_icon_colors([45, 28, 70, 255], [220, 215, 186, 255], 'M', None, 34.0)
}

/// Render the icon with explicit bg/fg/border RGBA colours, glyph, optional font path override,
/// and px scale. Border color is always lightBlue (173,205,247).
fn render_icon_colors(bg: [u8; 4], fg: [u8; 4], glyph: char, font_path_override: Option<&std::path::Path>, scale_px: f32) -> Vec<u8> {
    use ab_glyph::{Font, FontRef, PxScale, ScaleFont};

    // ── Canvas: 22×22 pt @ 2× retina → 44×44 physical pixels ────────────────
    const SIZE: usize = 44;

    let mut buf = vec![0u8; SIZE * SIZE * 4];

    // Fill background
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&bg);
    }

    // 2-px border — lighter than the dark bg so it's visible as a frame
    let border: [u8; 4] = [120, 80, 180, 255];
    for t in 0..2usize {
        for i in 0..SIZE {
            let set = |buf: &mut Vec<u8>, x: usize, y: usize| {
                let off = (y * SIZE + x) * 4;
                buf[off..off + 4].copy_from_slice(&border);
            };
            set(&mut buf, i, t);
            set(&mut buf, i, SIZE - 1 - t);
            set(&mut buf, t, i);
            set(&mut buf, SIZE - 1 - t, i);
        }
    }

    // ── Load font ─────────────────────────────────────────────────────────────
    let home = dirs::home_dir().unwrap();
    let default_font = home.join("Library/Fonts/MapleMono-NF-SemiBold.ttf");
    let font_path = font_path_override.unwrap_or(&default_font);
    let font_bytes = match std::fs::read(font_path) {
        Ok(b) => b,
        Err(_) => return encode_png(&buf, SIZE),
    };
    let font = match FontRef::try_from_slice(&font_bytes) {
        Ok(f) => f,
        Err(_) => return encode_png(&buf, SIZE),
    };

    // ── Render glyph centred in the square ────────────────────────────────────
    let scale  = PxScale::from(scale_px);
    let scaled = font.as_scaled(scale);

    let glyph_id = font.glyph_id(glyph);

    // Centre horizontally and vertically
    let glyph_w  = scaled.h_advance(glyph_id);
    let ascent   = scaled.ascent();
    let descent  = scaled.descent();
    let glyph_h  = ascent - descent;

    let origin_x = ((SIZE as f32 - glyph_w) / 2.0).round();
    let origin_y = ((SIZE as f32 - glyph_h) / 2.0 + ascent).round();

    let glyph = glyph_id.with_scale_and_position(scale, ab_glyph::point(origin_x, origin_y));

    if let Some(outlined) = font.outline_glyph(glyph) {
        let bounds = outlined.px_bounds();
        outlined.draw(|gx, gy, cov| {
            let px = bounds.min.x as i32 + gx as i32;
            let py = bounds.min.y as i32 + gy as i32;
            if px < 0 || py < 0 || px >= SIZE as i32 || py >= SIZE as i32 {
                return;
            }
            let off = (py as usize * SIZE + px as usize) * 4;
            let alpha = (cov * 255.0).round() as u32;
            let inv   = 255 - alpha;
            for i in 0..3 {
                buf[off + i] = ((fg[i] as u32 * alpha + buf[off + i] as u32 * inv) / 255) as u8;
            }
            buf[off + 3] = 255;
        });
    }

    encode_png(&buf, SIZE)
}

/// Encode a raw RGBA buffer to PNG bytes.
fn encode_png(rgba: &[u8], size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, size as u32, size as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        if let Ok(mut writer) = enc.write_header() {
            writer.write_image_data(rgba).ok();
        }
    }
    out
}

// ── ObjC target for status-bar button click ───────────────────────────────────

objc2::define_class!(
    /// ObjC object that receives the "buttonClicked:" action from the status bar button.
    #[unsafe(super(objc2::runtime::NSObject))]
    #[name = "MofiStatusTarget"]
    struct MofiStatusTarget;

    impl MofiStatusTarget {
        /// Called when the user clicks the menu-bar icon.
        /// Sends `show:about\n` to the daemon socket and then signals SIGUSR1.
        #[unsafe(method(buttonClicked:))]
        fn button_clicked(&self, _sender: *mut objc2::runtime::AnyObject) {
            use std::io::Write as _;

            // 1. Send "show:about\n" to the daemon socket (fire-and-forget).
            if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(SOCK_FILE) {
                stream.write_all(b"show:about\n").ok();
            }

            // 2. Signal the daemon to wake up.
            if let Ok(contents) = std::fs::read_to_string(PID_FILE) {
                if let Ok(pid) = contents.trim().parse::<i32>() {
                    unsafe { libc::kill(pid, libc::SIGUSR1) };
                }
            }
        }
    }
);

impl MofiStatusTarget {
    fn new() -> objc2::rc::Retained<Self> {
        use objc2::AnyThread as _;
        let this = Self::alloc();
        unsafe { objc2::msg_send![this, init] }
    }
}

// ── Icon flash state ─────────────────────────────────────────────────────────
/// Raw pointer to the NSStatusBarButton, set once in setup_status_bar().
/// Accessed only from the main thread (inside update() and setup_status_bar()).
static ICON_BTN: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
/// Set by a background thread after the flash delay; read and cleared by tick_icon_restore().
static ICON_RESTORE: AtomicBool = AtomicBool::new(false);

/// Swap the menu-bar icon to a flash glyph for 500 ms, then restore.
/// `glyph` is rendered from Symbols Nerd Font.
/// Must be called from the main thread.
pub fn flash_icon(glyph: char) {
    use objc2_app_kit::NSImage;
    use objc2_foundation::{NSData, NSSize};

    let ptr = ICON_BTN.load(Ordering::Relaxed);
    if ptr.is_null() { return; }

    // from Symbols Nerd Font.
    let nf_font = dirs::home_dir()
        .unwrap()
        .join("Library/Fonts/SymbolsNerdFont-Regular.ttf");
    let png = render_icon_colors(
        [45, 28, 70, 255],    // same dark bg as normal icon
        [220, 215, 186, 255], // fujiWhite glyph
        glyph,
        Some(&nf_font),
        28.0,
    );

    unsafe {
        let btn = &*(ptr as *const objc2_app_kit::NSStatusBarButton);
        let ns_data = NSData::with_bytes(&png);
        let alloc = <NSImage as objc2::AnyThread>::alloc();
        if let Some(img) = NSImage::initWithData(alloc, &ns_data) {
            img.setSize(NSSize { width: 22.0, height: 22.0 });
            btn.setImage(Some(&img));
        }
    }

    // Restore after 500 ms.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(500));
        ICON_RESTORE.store(true, Ordering::Relaxed);
    });
}

/// Check if a restore is pending and swap back to the normal icon.
/// Must be called from the main thread (e.g. inside egui update()).
pub fn tick_icon_restore() {
    if !ICON_RESTORE.swap(false, Ordering::Relaxed) { return; }

    use objc2_app_kit::NSImage;
    use objc2_foundation::{NSData, NSSize};

    let ptr = ICON_BTN.load(Ordering::Relaxed);
    if ptr.is_null() { return; }

    let png = render_menubar_icon();

    unsafe {
        let btn = &*(ptr as *const objc2_app_kit::NSStatusBarButton);
        let ns_data = NSData::with_bytes(&png);
        let alloc = <NSImage as objc2::AnyThread>::alloc();
        if let Some(img) = NSImage::initWithData(alloc, &ns_data) {
            img.setSize(NSSize { width: 22.0, height: 22.0 });
            btn.setImage(Some(&img));
        }
    }
}

/// Create a persistent NSStatusItem with a custom rendered icon.
/// Must be called on the main thread. The item is leaked intentionally.
fn setup_status_bar() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSImage, NSStatusBar, NSVariableStatusItemLength};
    use objc2_foundation::{NSData, NSSize};

    let png_bytes = render_menubar_icon();

    unsafe {
        let mtm = MainThreadMarker::new_unchecked();

        let bar  = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);

        // Build NSImage from PNG bytes
        let ns_data = NSData::with_bytes(&png_bytes);
        let alloc = <NSImage as objc2::AnyThread>::alloc();
        if let Some(img) = NSImage::initWithData(alloc, &ns_data) {
            // Tell AppKit this is a 2× (retina) image by setting its logical size
            // to half the pixel size (22×22 pt from 44×44 px).
            img.setSize(NSSize { width: 22.0, height: 22.0 });

            if let Some(btn) = item.button(mtm) {
                btn.setImage(Some(&img));

                // Store the raw button pointer for flash_icon() / tick_icon_restore().
                ICON_BTN.store(
                    objc2::rc::Retained::as_ptr(&btn) as *mut std::ffi::c_void,
                    Ordering::Relaxed,
                );

                // Wire up the click handler.
                let target = MofiStatusTarget::new();
                btn.setTarget(Some(&*(objc2::rc::Retained::as_ptr(&target) as *const objc2::runtime::AnyObject)));
                btn.setAction(Some(objc2::sel!(buttonClicked:)));
                // Leak the target so it lives as long as the button.
                let _ = objc2::rc::Retained::into_raw(target);
            }
        }

        // Leak so the item stays alive forever.
        let _ = objc2::rc::Retained::into_raw(item);
    }
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
            std::thread::spawn(move || handle_client(stream, pending, p_input, p_is_themes, i_result, p_mode));
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
            // Create the status bar item on the main thread inside the eframe constructor.
            setup_status_bar();
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
        return;
    }

    // tab:<tab> — open on a specific tab and wait for the user's selection.
    // Used by --pass and --clip so the client can act on the result.
    if let Some(tab) = line.strip_prefix("tab:") {
        *pending_mode.lock().unwrap() = Some(tab.to_string());
        // Fall through to the pending_entry wait loop below.
    } else if line.starts_with("input\t") || line.starts_with("themes\t") {
        let is_themes = line.starts_with("themes\t");
        let rest = if is_themes { &line["themes\t".len()..] } else { &line["input\t".len()..] };
        let items: Vec<String> = rest.split('\t').map(|s| s.to_string()).collect();

        // Clear any previous result, set the themes flag, then post the items.
        { *input_result.lock().unwrap() = None; }
        { *pending_input_is_themes.lock().unwrap() = is_themes; }
        { *pending_input.lock().unwrap() = Some(items); }

        // Wait for the UI to post a result (selected string or None for cancel).
        let result = loop {
            {
                let mut lock = input_result.lock().unwrap();
                if let Some(val) = lock.take() { break val; }
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
        // Clear any stale value left from a previous session before waiting.
        *pending_entry.lock().unwrap() = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dump_icon() {
        let png = render_menubar_icon();
        std::fs::write("/tmp/mofi-icon-preview.png", &png).unwrap();
        println!("Written {} bytes to /tmp/mofi-icon-preview.png", png.len());
    }
}
