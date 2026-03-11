use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Key, Rounding, Stroke, Vec2,
};

#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy,
    NSRunningApplication, NSWorkspace,
};

use crate::apps::discover_apps;
use crate::clipboard::{load_history, start_poller, ClipboardEntry, ClipboardHistory};
use crate::config::{config_path, theme_by_name, Config, Theme};
use crate::frecency::FrecencyStore;
#[cfg(target_os = "linux")]
use crate::launcher::launch_shell_command;
use crate::launcher::{launch_app, paste_text, LaunchItem, Launcher};
use crate::pass::discover_pass_entries;

// ── macOS helpers ─────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn capture_previous_app() -> Option<Retained<NSRunningApplication>> {
    let workspace = NSWorkspace::sharedWorkspace();
    let apps = workspace.runningApplications();
    let my_pid = std::process::id() as i32;
    for app in apps.iter() {
        if app.isActive() && app.processIdentifier() != my_pid {
            return Some(app.clone());
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn restore_app_focus(app: &NSRunningApplication) {
    app.activateWithOptions(NSApplicationActivationOptions(0));
}

// ── Constants ─────────────────────────────────────────────────────────────────

const ICON_SIZE: f32 = 30.0;
const ROW_HEIGHT: f32 = 48.0;

/// Return the paths to look for the custom fonts, in priority order.
/// On macOS: ~/Library/Fonts  On Linux: ~/.local/share/fonts and system paths.
fn font_search_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        dirs.push(home.join("Library/Fonts"));

        #[cfg(target_os = "linux")]
        {
            dirs.push(home.join(".local/share/fonts"));
            dirs.push(home.join(".fonts"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        dirs.push(std::path::PathBuf::from("/usr/local/share/fonts"));
        dirs.push(std::path::PathBuf::from("/usr/share/fonts"));
        dirs.push(std::path::PathBuf::from("/usr/share/fonts/truetype"));
        dirs.push(std::path::PathBuf::from("/usr/share/fonts/OTF"));
    }
    dirs
}

/// Try to load a font by stem name (e.g. "MapleMono-NF-Regular").
/// First tries the exact filename (with .ttf/.otf), then scans font directories
/// for any file whose name *contains* the stem — handles e.g. "MapleMono-NF-Regular(1).ttf".
fn find_font(stem: &str) -> Option<Vec<u8>> {
    // Try exact filenames first.
    for ext in &["ttf", "otf"] {
        let filename = format!("{}.{}", stem, ext);
        for dir in font_search_dirs() {
            let path = dir.join(&filename);
            if let Ok(bytes) = std::fs::read(&path) {
                return Some(bytes);
            }
        }
    }
    // Fallback: scan directories for any file whose name contains the stem.
    for dir in font_search_dirs() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.contains(stem)
                    && (name_str.ends_with(".ttf") || name_str.ends_with(".otf"))
                {
                    if let Ok(bytes) = std::fs::read(entry.path()) {
                        return Some(bytes);
                    }
                }
            }
        }
    }
    None
}

// ── Kanagawa palette (kept for per-item accent colors) ────────────────────────
mod kana {
    use egui::Color32;
    pub const fn hex(r: u8, g: u8, b: u8) -> Color32 {
        Color32::from_rgb(r, g, b)
    }
    pub const CRYSTAL_BLUE: Color32 = hex(0x7E, 0x9C, 0xD8);
    pub const ONI_VIOLET: Color32 = hex(0x95, 0x7F, 0xB8);
    pub const SPRING_GREEN: Color32 = hex(0x98, 0xBB, 0x6C);
    pub const WAVE_AQUA2: Color32 = hex(0x7A, 0xA8, 0x9F);
    pub const SAKURA_PINK: Color32 = hex(0xD2, 0x7E, 0x99);
    pub const SURIMI_ORANGE: Color32 = hex(0xFF, 0xA0, 0x66);
    pub const CARP_YELLOW: Color32 = hex(0xE6, 0xC3, 0x84);
    pub const WAVE_RED: Color32 = hex(0xE4, 0x68, 0x76);
    pub const SPRING_BLUE: Color32 = hex(0x7F, 0xB4, 0xCA);
    pub const BOAT_YELLOW2: Color32 = hex(0xC0, 0xA3, 0x6E);
}

// ── Nerd Font glyph lookup ────────────────────────────────────────────────────

fn glyph_for_item(item: &LaunchItem) -> &'static str {
    match item {
        LaunchItem::Clip(e) if e.is_image() => "\u{F03E}", // nf-fa-image
        LaunchItem::Clip(e) => glyph_for_clip(&e.text),
        LaunchItem::Pass(e) => glyph_for_pass(&e.name),
        LaunchItem::App(a) => glyph_for_app(&a.name),
    }
}

/// Pick a glyph based on clipboard text content.
fn glyph_for_clip(text: &str) -> &'static str {
    let trimmed = text.trim();
    // URL
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("ftp://")
    {
        return "\u{F0AC}"; // nf-fa-globe
    }
    // Email address
    if !trimmed.contains('\n') && trimmed.contains('@') && trimmed.contains('.') {
        return "\u{F0E0}"; // nf-fa-envelope
    }
    // File path
    if trimmed.starts_with('/') || trimmed.starts_with("~/") {
        return "\u{F15B}"; // nf-fa-file
    }
    // UUID  e.g. 550e8400-e29b-41d4-a716-446655440000
    let is_uuid = {
        let p: Vec<&str> = trimmed.split('-').collect();
        p.len() == 5
            && p[0].len() == 8
            && p[1].len() == 4
            && p[2].len() == 4
            && p[3].len() == 4
            && p[4].len() == 12
            && p.iter().all(|s| s.chars().all(|c| c.is_ascii_hexdigit()))
    };
    if is_uuid {
        return "\u{F0CB2}"; // nf-md-numeric
    }
    // Pure numeric / hex token (short, no spaces)
    if !trimmed.contains('\n')
        && trimmed.len() <= 64
        && trimmed
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == 'x' || c == 'X' || c == '-' || c == '_')
    {
        return "\u{F0CB2}"; // nf-md-numeric
    }
    // Multi-line → text-box stack
    if trimmed.contains('\n') {
        return "\u{F0219}"; // nf-md-text_box_multiple
    }
    // Short snippet → single page
    if trimmed.len() <= 60 {
        return "\u{F0F6}"; // nf-fa-file_text_o  (small note / page)
    }
    // Long single-line text
    "\u{F0219}" // nf-md-text_box
}

/// Pick a padlock glyph based on the pass entry path.
fn glyph_for_pass(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    // Category hints from folder/entry name.
    if lower.contains("ssh") || lower.contains("gpg") || lower.contains("key") {
        "\u{F0306}" // nf-md-key_variant
    } else if lower.contains("bank") || lower.contains("finance") || lower.contains("credit") {
        "\u{F024B}" // nf-md-bank
    } else if lower.contains("email") || lower.contains("mail") || lower.contains("smtp") {
        "\u{F0E0}" // nf-fa-envelope
    } else if lower.contains("wifi") || lower.contains("network") || lower.contains("vpn") {
        "\u{F0A72}" // nf-md-lock_check  (network cred)
    } else if lower.contains("github") || lower.contains("gitlab") || lower.contains("git") {
        "\u{F0A70}" // nf-md-source_repository_multiple → use lock + git feel
    } else if lower.contains("work") || lower.contains("corp") || lower.contains("office") {
        "\u{F0A75}" // nf-md-briefcase_lock
    } else if name.contains('/') {
        "\u{F023}" // nf-fa-lock  (nested entry — standard padlock)
    } else {
        "\u{F09C0}" // nf-md-lock  (top-level entry — solid lock)
    }
}

/// Returns a representative Nerd Font glyph for each built-in theme name.
fn theme_glyph_for(name: &str) -> &'static str {
    match name {
        "kanagawa" => "\u{E6AC}",    // nf-custom-vim  (wave/japanese feel)
        "gruvbox" => "\u{F0043}",    // nf-md-fire     (warm earthy tones)
        "nord" => "\u{F0599}",       // nf-md-snowflake
        "tokyonight" => "\u{F0E7B}", // nf-md-city_variant_outline
        "dracula" => "\u{F0B4B}",    // nf-md-bat
        "solarized" => "\u{F0438}",  // nf-md-weather_sunny
        "monokai" => "\u{F0A71}",    // nf-md-coffee
        "catppuccin" => "\u{F028B}", // nf-md-cat
        "onedark" => "\u{F04A6}",    // nf-md-moon_waning_crescent
        "rosepine" => "\u{F04CB}",   // nf-md-pine_tree
        _ => "\u{F53F}",             // nf-md-palette  (fallback)
    }
}

fn glyph_for_app(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("safari") {
        "\u{E748}"
    } else if n.contains("firefox") {
        "\u{E745}"
    } else if n.contains("chrome") || n.contains("chromium") {
        "\u{E743}"
    } else if n.contains("terminal")
        || n.contains("iterm")
        || n.contains("alacritty")
        || n.contains("warp")
        || n.contains("kitty")
        || n.contains("ghostty")
    {
        "\u{EA85}"
    } else if n.contains("code") || n.contains("vscode") || n.contains("cursor") {
        "\u{E8DA}"
    } else if n.contains("xcode") {
        "\u{E8E8}"
    } else if n.contains("sublime") {
        "\u{E7AA}"
    } else if n.contains("finder") {
        "\u{F0036}"
    } else if n.contains("mail") {
        "\u{F0E0}"
    } else if n.contains("messages") {
        "\u{F27A}"
    } else if n.contains("calendar") {
        "\u{F073}"
    } else if n.contains("music") {
        "\u{F001}"
    } else if n.contains("spotify") {
        "\u{F1BC}"
    } else if n.contains("discord") {
        "\u{F1FF}"
    } else if n.contains("slack") {
        "\u{E8A4}"
    } else if n.contains("docker") {
        "\u{E7B0}"
    } else if n.contains("github desktop") || n.contains("github") {
        "\u{E709}"
    } else if n.contains("figma") {
        "\u{E7DA}"
    } else if n.contains("system preferences") || n.contains("system settings") {
        "\u{EB52}"
    } else if n.contains("app store") {
        "\u{F0BD}"
    } else if n.contains("photos") {
        "\u{F03E}"
    } else if n.contains("notes") {
        "\u{F249}"
    } else if n.contains("maps") {
        "\u{F279}"
    } else if n.contains("calculator") {
        "\u{F1EC}"
    } else if n.contains("disk utility") {
        "\u{F02CA}"
    } else if n.contains("activity monitor") {
        "\u{F0128}"
    } else if n.contains("time machine") {
        "\u{F006F}"
    } else if n.contains("vlc") {
        "\u{F057C}"
    } else if n.contains("steam") {
        "\u{F1B6}"
    } else if n.contains("postman") {
        "\u{E86B}"
    } else if n.contains("1password") || n.contains("onepassword") {
        "\u{F0881}"
    } else if n.contains("dropbox") {
        "\u{E707}"
    } else {
        "\u{F2D0}"
    }
}

fn glyph_color_for_item(item: &LaunchItem, t: &Theme) -> Color32 {
    match item {
        LaunchItem::Clip(_) => t.accent2,
        LaunchItem::Pass(_) => t.toast,
        LaunchItem::App(a) => accent_for_name(&a.name),
    }
}

fn accent_for_name(name: &str) -> Color32 {
    let accents = [
        kana::CRYSTAL_BLUE,
        kana::ONI_VIOLET,
        kana::SPRING_GREEN,
        kana::WAVE_AQUA2,
        kana::SAKURA_PINK,
        kana::SURIMI_ORANGE,
        kana::CARP_YELLOW,
        kana::WAVE_RED,
        kana::SPRING_BLUE,
        kana::BOAT_YELLOW2,
    ];
    let idx = name.bytes().next().unwrap_or(0) as usize % accents.len();
    accents[idx]
}

fn dim_color(c: Color32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        (c.r() as u16 * 2 / 3) as u8,
        (c.g() as u16 * 2 / 3) as u8,
        (c.b() as u16 * 2 / 3) as u8,
        255,
    )
}

// ── Font loading ──────────────────────────────────────────────────────────────

/// Load fonts and return the best available "medium weight" family.
/// If MapleMono-NF-Medium is not installed, falls back to Monospace so
/// we never reference an unregistered FontFamily (which panics in epaint).
fn load_fonts(ctx: &egui::Context) -> FontFamily {
    let mut fonts = FontDefinitions::default();
    if let Some(bytes) = find_font("MapleMono-NF-Regular") {
        fonts
            .font_data
            .insert("MapleMono".to_owned(), FontData::from_owned(bytes));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "MapleMono".to_owned());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "MapleMono".to_owned());
    }
    let medium_family = if let Some(bytes) = find_font("MapleMono-NF-Medium") {
        fonts
            .font_data
            .insert("MapleMonoMedium".to_owned(), FontData::from_owned(bytes));
        fonts.families.insert(
            FontFamily::Name("medium".into()),
            vec!["MapleMonoMedium".to_owned()],
        );
        FontFamily::Name("medium".into())
    } else {
        // Medium font not installed — fall back to the regular NF font or Monospace.
        FontFamily::Monospace
    };
    ctx.set_fonts(fonts);
    medium_family
}

// ── Mode ──────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Mode {
    Apps,
    Clipboard,
    Pass,
    Files,
    About,
    /// Activated by `mofi --input`.
    Input,
    /// Activated by `mofi --themes` — same as Input but with live theme preview.
    Themes,
}

// ── App state ─────────────────────────────────────────────────────────────────

pub struct RofiApp {
    query: String,
    items: Vec<LaunchItem>,
    filtered: Vec<usize>,
    selected: usize,
    launcher: Launcher,
    mode: Mode,
    should_close: bool,
    clip_history: ClipboardHistory,
    /// In oneshot (daemonless) mode, holds the result after the window closes.
    /// Some(Some(name)) = pass entry selected, Some(None) = cancelled/Escape, None = not done.
    pub oneshot_result: Option<Option<String>>,
    /// True when running in daemonless oneshot mode; false in persistent daemon mode.
    /// Controls whether update() signals the event loop to exit after hide().
    oneshot_mode: bool,
    toast: Option<(String, std::time::Instant)>,
    frame_count: u32,
    /// True once egui has reported keyboard focus at least once.
    /// On Wayland the focus `enter` event may arrive after several frames;
    /// we must not trigger auto-hide-on-focus-loss until focus was first seen.
    had_keyboard_focus_ever: bool,
    toggle: Arc<AtomicUsize>,
    visible: bool,
    /// Previous focused app — macOS only (focus restore after hide).
    #[cfg(target_os = "macos")]
    prev_app: Option<Retained<NSRunningApplication>>,
    pending_entry: Arc<Mutex<Option<Option<String>>>>,
    pending_input: Arc<Mutex<Option<Vec<String>>>>,
    input_result: Arc<Mutex<Option<Option<String>>>>,
    pending_input_is_themes: Arc<Mutex<bool>>,
    /// Requested tab to open when the window next shows ("pass" | "clip").
    pending_mode: Arc<Mutex<Option<String>>>,
    input_items: Vec<String>,
    input_filtered: Vec<usize>,
    /// True when the current Input session is a theme picker (mofi --themes).
    input_is_themes: bool,
    /// Theme that was active when the themes picker opened (restored on Escape).
    theme_before_preview: Option<Theme>,
    // ── Theme hot-reload ──
    theme: Theme,
    config_mtime: Option<SystemTime>,
    /// The FontFamily to use for medium-weight text.  Normally
    /// FontFamily::Name("medium") when MapleMono-NF-Medium.ttf is installed,
    /// otherwise falls back to FontFamily::Monospace to prevent a panic.
    medium_font: FontFamily,
    /// Frecency store — tracks launch frequency/recency for Apps and Pass items.
    frecency: FrecencyStore,
    /// The last selected index we issued a scroll_to_rect for.
    /// Used to avoid re-issuing the scroll every frame (which causes the
    /// center-align to drift the view on the first frame when selected=0).
    last_scroll_to: usize,
    /// Incremented on every show/tab-switch.  Used as part of the ScrollArea
    /// id_source so each show gets a fresh egui ID with no stale scroll state.
    scroll_generation: u64,
    /// System-tray handle (Linux only) — used to update the tray icon colour
    /// when the mode or visibility changes.
    #[cfg(target_os = "linux")]
    tray_handle: Option<crate::tray::TrayHandle>,
    /// Cached textures for clipboard image thumbnails (keyed by file path).
    #[cfg(target_os = "linux")]
    image_textures: std::collections::HashMap<String, egui::TextureHandle>,
    /// Single-pane file explorer state.
    file_pane: crate::files::Pane,
    /// Cached zoxide query results (directory paths).
    zoxide_results: Vec<String>,
    /// The query term that produced the current zoxide_results.
    zoxide_last_query: String,
    /// When the user presses space while focused on a zoxide row, this locks
    /// the zoxide path so children can be drilled into.  Cleared on hide,
    /// tab-switch, Ctrl+H, or when the query no longer contains a space.
    drill_target: Option<String>,
}

impl RofiApp {
    #[cfg(target_os = "macos")]
    pub fn new(
        cc: &eframe::CreationContext,
        toggle: Arc<AtomicUsize>,
        pending_entry: Arc<Mutex<Option<Option<String>>>>,
        pending_input: Arc<Mutex<Option<Vec<String>>>>,
        input_result: Arc<Mutex<Option<Option<String>>>>,
        pending_input_is_themes: Arc<Mutex<bool>>,
        pending_mode: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self::new_with_ctx(
            &cc.egui_ctx,
            toggle,
            pending_entry,
            pending_input,
            input_result,
            pending_input_is_themes,
            pending_mode,
        )
    }

    #[cfg(target_os = "linux")]
    pub fn new_linux(
        toggle: Arc<AtomicUsize>,
        pending_entry: Arc<Mutex<Option<Option<String>>>>,
        pending_input: Arc<Mutex<Option<Vec<String>>>>,
        input_result: Arc<Mutex<Option<Option<String>>>>,
        pending_input_is_themes: Arc<Mutex<bool>>,
        pending_mode: Arc<Mutex<Option<String>>>,
        tray_handle: crate::tray::TrayHandle,
    ) -> Self {
        // On Linux with the layer-shell path, setup() in AppHandler will
        // pass a real Context. We create a temporary one just to call the
        // shared constructor; setup() will replace fonts etc. on the real ctx.
        let tmp_ctx = egui::Context::default();
        let mut app = Self::new_with_ctx(
            &tmp_ctx,
            toggle,
            pending_entry,
            pending_input,
            input_result,
            pending_input_is_themes,
            pending_mode,
        );
        app.tray_handle = Some(tray_handle);
        app
    }

    /// Daemonless one-shot constructor.  No IPC Arcs needed; the window opens
    /// immediately (visible=true) and the result is read from `oneshot_result`
    /// after `layer_window::run_oneshot` returns.
    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    pub fn new_oneshot_with_mode(initial_mode: Mode) -> Self {
        let tmp_ctx = egui::Context::default();
        let mut app = Self::new_with_ctx(
            &tmp_ctx,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(false)),
            Arc::new(Mutex::new(None)),
        );
        // Start visible — the surface is already mapped by run_oneshot().
        app.visible = true;
        app.oneshot_mode = true;
        app.mode = initial_mode;
        // Pre-filter for the requested tab so the list is ready on first frame.
        app.refilter(true);
        app
    }

    fn new_with_ctx(
        ctx: &egui::Context,
        toggle: Arc<AtomicUsize>,
        pending_entry: Arc<Mutex<Option<Option<String>>>>,
        pending_input: Arc<Mutex<Option<Vec<String>>>>,
        input_result: Arc<Mutex<Option<Option<String>>>>,
        pending_input_is_themes: Arc<Mutex<bool>>,
        pending_mode: Arc<Mutex<Option<String>>>,
    ) -> Self {
        let medium_font = load_fonts(ctx);

        #[cfg(target_os = "macos")]
        unsafe {
            use objc2::MainThreadMarker;
            let mtm = MainThreadMarker::new_unchecked();
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }

        let history = Arc::new(Mutex::new(load_history()));
        start_poller(Arc::clone(&history));

        let mut all_items: Vec<LaunchItem> =
            discover_apps().into_iter().map(LaunchItem::App).collect();
        {
            let lock = history.lock().unwrap();
            for e in lock.iter().cloned() {
                all_items.push(LaunchItem::Clip(e));
            }
        }
        for e in discover_pass_entries() {
            all_items.push(LaunchItem::Pass(e));
        }

        let filtered: Vec<usize> = (0..all_items.len()).collect();
        let cfg = Config::load();
        let theme = theme_by_name(cfg.active_theme_name());
        let config_mtime = std::fs::metadata(config_path())
            .ok()
            .and_then(|m| m.modified().ok());

        let mut app = Self {
            query: String::new(),
            items: all_items,
            filtered,
            selected: 0,
            launcher: Launcher::new(),
            mode: Mode::Apps,
            should_close: false,
            clip_history: history,
            toast: None,
            frame_count: 0,
            had_keyboard_focus_ever: false,
            toggle,
            visible: false,
            oneshot_result: None,
            oneshot_mode: false,
            #[cfg(target_os = "macos")]
            prev_app: None,
            pending_entry,
            pending_input,
            input_result,
            pending_input_is_themes,
            pending_mode,
            input_items: Vec::new(),
            input_filtered: Vec::new(),
            input_is_themes: false,
            theme_before_preview: None,
            theme,
            config_mtime,
            medium_font,
            frecency: FrecencyStore::load(),
            last_scroll_to: usize::MAX,
            scroll_generation: 0,
            #[cfg(target_os = "linux")]
            tray_handle: None,
            #[cfg(target_os = "linux")]
            image_textures: std::collections::HashMap::new(),
            file_pane: crate::files::Pane::new(
                &dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")),
            ),
            zoxide_results: Vec::new(),
            zoxide_last_query: String::new(),
            drill_target: None,
        };
        app.refilter(true);
        app
    }

    // ── Sync helpers ──────────────────────────────────────────────────────────

    fn sync_clipboard(&mut self) {
        let clips: Vec<ClipboardEntry> = self.clip_history.lock().unwrap().clone();
        self.items
            .retain(|i| matches!(i, LaunchItem::App(_) | LaunchItem::Pass(_)));
        for e in clips {
            self.items.push(LaunchItem::Clip(e));
        }
    }

    fn sync_apps(&mut self) {
        let new_apps: Vec<LaunchItem> = discover_apps().into_iter().map(LaunchItem::App).collect();
        self.items.retain(|i| !matches!(i, LaunchItem::App(_)));
        self.items.extend(new_apps);
    }

    /// Load a PNG from disk as an egui texture, caching it by path.
    #[cfg(target_os = "linux")]
    fn load_image_texture(
        &mut self,
        ctx: &egui::Context,
        path: &str,
    ) -> Option<(egui::TextureHandle, (u32, u32))> {
        if let Some(tex) = self.image_textures.get(path) {
            let [w, h] = tex.size();
            return Some((tex.clone(), (w as u32, h as u32)));
        }
        let data = std::fs::read(path).ok()?;
        let img = image::load_from_memory(&data).ok()?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
        let tex = ctx.load_texture(path, color_image, egui::TextureOptions::LINEAR);
        self.image_textures.insert(path.to_string(), tex.clone());
        Some((tex, (w, h)))
    }

    /// Pre-load all clipboard thumbnail textures so switching to the Clipboard
    /// tab doesn't stutter on the first frame.
    #[cfg(target_os = "linux")]
    fn preload_clipboard_textures(&mut self, ctx: &egui::Context) {
        let paths: Vec<String> = self
            .items
            .iter()
            .filter_map(|item| {
                if let LaunchItem::Clip(ce) = item {
                    ce.thumbnail_path
                        .as_ref()
                        .or(ce.image_path.as_ref())
                        .cloned()
                } else {
                    None
                }
            })
            .collect();
        for path in paths {
            let _ = self.load_image_texture(ctx, &path);
        }
    }

    fn refilter(&mut self, reset_selection: bool) {
        let mode_items: Vec<(usize, &LaunchItem)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| match self.mode {
                Mode::Apps => matches!(item, LaunchItem::App(_)),
                Mode::Clipboard => matches!(item, LaunchItem::Clip(_)),
                Mode::Pass => matches!(item, LaunchItem::Pass(_)),
                Mode::About | Mode::Input | Mode::Themes | Mode::Files => false,
            })
            .collect();

        let search_items: Vec<LaunchItem> = mode_items.iter().map(|(_, i)| (*i).clone()).collect();
        let matched: Vec<usize> = self
            .launcher
            .search(&self.query, &search_items, &self.frecency);
        self.filtered = matched.into_iter().map(|li| mode_items[li].0).collect();

        if reset_selection {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
        }
    }

    fn refilter_input(&mut self) {
        if self.query.is_empty() {
            self.input_filtered = (0..self.input_items.len()).collect();
        } else {
            let q = self.query.to_lowercase();
            self.input_filtered = self
                .input_items
                .iter()
                .enumerate()
                .filter(|(_, s)| s.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect();
        }
        self.selected = 0;
    }

    fn execute_selected(&mut self) {
        if self.mode == Mode::Input || self.mode == Mode::Themes {
            let result = self
                .input_filtered
                .get(self.selected)
                .and_then(|&i| self.input_items.get(i))
                .cloned();
            if self.mode == Mode::Themes {
                // Commit the previewed theme: clear backup and persist to config.
                self.theme_before_preview = None;
                if let Some(ref name) = result {
                    let theme_name = name.trim_start_matches("* ").to_string();
                    let mut cfg = crate::config::Config::load();
                    cfg.theme = theme_name;
                    cfg.save();
                }
                // Close without producing a pass/output string.
                self.oneshot_result = Some(None);
                self.should_close = true;
                return;
            }
            *self.input_result.lock().unwrap() = Some(result.clone());
            // Oneshot: record the result (treat as cancel/no-output for input mode).
            self.oneshot_result = Some(result);
            self.should_close = true;
            return;
        }
        if let Some(&idx) = self.filtered.get(self.selected) {
            match &self.items[idx] {
                LaunchItem::App(app) => {
                    let path = app.path.clone();
                    let name = app.name.clone();
                    launch_app(&path);
                    self.frecency.record(&name);
                    // Oneshot: no text output for app launches — treat as "done".
                    self.oneshot_result = Some(None);
                    self.should_close = true;
                }
                LaunchItem::Clip(e) => {
                    if let Some(ref img_path) = e.image_path {
                        crate::clipboard::write_clipboard_image(img_path);
                    } else {
                        paste_text(&e.text.clone());
                    }
                    // Oneshot: no text output for clipboard pastes.
                    self.oneshot_result = Some(None);
                    self.should_close = true;
                }
                LaunchItem::Pass(e) => {
                    let name = e.name.clone();
                    self.frecency.record(&name);
                    *self.pending_entry.lock().unwrap() = Some(Some(name.clone()));
                    // Oneshot: record the pass entry name so the caller can decrypt it.
                    self.oneshot_result = Some(Some(name));
                    self.should_close = true;
                }
            }
        } else if self.mode == Mode::Apps && self.query.trim().starts_with('!') {
            // Selected the synthetic "Run in shell" row — execute query as
            // a shell command (strip the leading '!').
            let cmd = self.query.trim().trim_start_matches('!').trim();
            #[cfg(target_os = "linux")]
            launch_shell_command(cmd);
            self.oneshot_result = Some(None);
            self.should_close = true;
        }
    }

    /// Query zoxide for directory completions matching `term`.
    /// Results are cached — only re-queries when the term changes.
    fn update_zoxide(&mut self, term: &str) {
        if term == self.zoxide_last_query {
            return;
        }
        self.zoxide_last_query = term.to_string();
        if term.is_empty() {
            self.zoxide_results.clear();
            return;
        }
        let output = std::process::Command::new("zoxide")
            .args(["query", "-l", term])
            .output();
        match output {
            Ok(o) if o.status.success() => {
                self.zoxide_results = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .take(8)
                    .map(|s| s.to_string())
                    .collect();
            }
            _ => {
                self.zoxide_results.clear();
            }
        }
    }

    fn restore_focus(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(app) = self.prev_app.take() {
            restore_app_focus(&app);
        }
    }

    /// Reset a scroll area's offset to zero by clearing its persisted state.
    /// The `id_salt` is hashed with the parent UI id by ScrollArea, so we
    /// clear all persisted scroll_area::State entries matching any parent.
    /// Instead we set a flag; the scroll areas check it on the next frame.
    fn request_scroll_reset(&mut self) {
        self.scroll_generation = self.scroll_generation.wrapping_add(1);
        self.last_scroll_to = usize::MAX;
    }

    fn hide(&mut self, ctx: &egui::Context) {
        self.visible = false;
        self.should_close = false;
        self.query.clear();
        self.selected = 0;
        self.frame_count = 0;
        self.had_keyboard_focus_ever = false;
        self.toast = None;
        self.last_scroll_to = usize::MAX;
        self.zoxide_results.clear();
        self.zoxide_last_query.clear();
        self.drill_target = None;
        // Request scroll reset so the next open starts at the top.
        self.request_scroll_reset();
        // If we were in themes mode and the user cancelled, restore original theme.
        if let Some(original) = self.theme_before_preview.take() {
            self.theme = original;
        }
        self.input_is_themes = false;
        self.mode = Mode::Apps;
        self.refilter(true);
        // In oneshot mode, record the result now (None = cancelled).
        // execute_selected() may have already set oneshot_result to Some(Some(name)).
        // If not set yet, it means the user cancelled (Escape / focus-loss).
        if self.oneshot_result.is_none() {
            self.oneshot_result = Some(None);
        }
        {
            let mut l = self.pending_entry.lock().unwrap();
            if l.is_none() {
                *l = Some(None);
            }
        }
        {
            let mut l = self.input_result.lock().unwrap();
            if l.is_none() && !self.input_items.is_empty() {
                *l = Some(None);
            }
        }
        self.input_items.clear();
        self.input_filtered.clear();
        #[cfg(target_os = "macos")]
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        // On Wayland/Linux, Visible(false) and OuterPosition are no-ops in
        // winit 0.29.  Shrink to 1×1 so the surface is effectively invisible.
        #[cfg(not(target_os = "macos"))]
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1.0, 1.0)));
        self.restore_focus();
        self.update_tray();
    }

    /// Notify the system-tray icon of the current selected item's glyph and colour.
    #[cfg(target_os = "linux")]
    fn update_tray(&self) {
        if let Some(handle) = &self.tray_handle {
            let visible = self.visible;

            // Determine the glyph and colour for the current selection.
            let (glyph, fg, label) = if !visible {
                // Idle — will be handled by set_icon(visible=false).
                (String::new(), [0u8; 3], String::new())
            } else if self.mode == Mode::Input || self.mode == Mode::Themes {
                // Input / Themes list.
                let text = self
                    .input_filtered
                    .get(self.selected)
                    .and_then(|&i| self.input_items.get(i))
                    .cloned()
                    .unwrap_or_default();
                if self.mode == Mode::Themes {
                    let g = theme_glyph_for(text.trim_start_matches("* "));
                    (
                        g.to_string(),
                        [
                            self.theme.icon_sel.r(),
                            self.theme.icon_sel.g(),
                            self.theme.icon_sel.b(),
                        ],
                        format!("mofi — {}", text.trim_start_matches("* ")),
                    )
                } else {
                    (
                        "\u{F0CA}".to_string(), // nf-fa-list_ul
                        [
                            self.theme.icon_sel.r(),
                            self.theme.icon_sel.g(),
                            self.theme.icon_sel.b(),
                        ],
                        "mofi — Input".into(),
                    )
                }
            } else {
                // Normal results list (Apps / Clipboard / Pass).
                // Use a mode-level glyph for the tray (not the per-item glyph).
                let mode_glyph: Option<&str> = match self.mode {
                    Mode::Apps => Some("\u{F0E7}"),      // nf-fa-bolt
                    Mode::Clipboard => Some("\u{F0C6}"), // nf-fa-paperclip
                    Mode::Files => Some("\u{F07B}"),     // nf-fa-folder
                    _ => None,                           // Pass: use per-item glyph
                };
                if let Some(&item_idx) = self.filtered.get(self.selected) {
                    let item = &self.items[item_idx];
                    let g = mode_glyph.unwrap_or_else(|| glyph_for_item(item));
                    let c = glyph_color_for_item(item, &self.theme);
                    let lbl = match item {
                        LaunchItem::App(a) => format!("mofi — {}", a.name),
                        LaunchItem::Clip(_) => "mofi — Clipboard".into(),
                        LaunchItem::Pass(e) => format!("mofi — {}", e.name),
                    };
                    (g.to_string(), [c.r(), c.g(), c.b()], lbl)
                } else {
                    let mode_name = match self.mode {
                        Mode::Apps => "Apps",
                        Mode::Clipboard => "Clipboard",
                        Mode::Pass => "Pass",
                        Mode::Files => "Files",
                        _ => "mofi",
                    };
                    (
                        "\u{F2D0}".to_string(), // generic app glyph
                        [0x7E, 0x9C, 0xD8],     // crystal blue
                        format!("mofi — {}", mode_name),
                    )
                }
            };

            handle.update(move |tray| {
                tray.set_icon(&glyph, fg, &label, visible);
            });
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn update_tray(&self) {}

    /// Switch into themes mode inline (no IPC — used when clicking the Themes tab directly).
    fn enter_themes_mode(&mut self) {
        use crate::config::{Config, THEME_NAMES};
        let active = Config::load().active_theme_name().to_string();
        self.input_items = THEME_NAMES
            .iter()
            .map(|&name| {
                if name == active {
                    format!("* {}", name)
                } else {
                    name.to_string()
                }
            })
            .collect();
        self.input_is_themes = true;
        self.theme_before_preview = Some(self.theme.clone());
        self.mode = Mode::Themes;
        self.query.clear();
        // Pre-select the active theme.
        self.selected = self
            .input_items
            .iter()
            .position(|s| s.starts_with("* "))
            .unwrap_or(0);
        self.refilter_input();
    }

    /// Poll config file mtime and reload theme if it changed.
    fn maybe_reload_theme(&mut self) {
        let mtime = std::fs::metadata(config_path())
            .ok()
            .and_then(|m| m.modified().ok());
        if mtime != self.config_mtime {
            self.config_mtime = mtime;
            let cfg = Config::load();
            self.theme = theme_by_name(cfg.active_theme_name());
        }
    }

    /// In themes mode: instantly apply the theme at the current selection as a preview.
    fn preview_theme_at_selection(&mut self) {
        if let Some(&item_idx) = self.input_filtered.get(self.selected) {
            if let Some(raw) = self.input_items.get(item_idx) {
                let name = raw.trim_start_matches("* ");
                self.theme = crate::config::theme_by_name(name);
            }
        }
    }
}

// ── eframe::App ───────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
impl eframe::App for RofiApp {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.do_update(ctx);
    }
}

// ── layer_window::AppHandler (Linux) ─────────────────────────────────────────

#[cfg(target_os = "linux")]
impl crate::layer_window::AppHandler for RofiApp {
    fn setup(&mut self, ctx: &egui::Context) {
        // Re-run font loading on the real egui Context now that we have one.
        self.medium_font = load_fonts(ctx);
    }

    fn update(&mut self, ctx: &egui::Context) -> bool {
        self.do_update(ctx);
        // In oneshot mode (visible starts true, no toggle mechanism), signal
        // the event loop to exit once the app has hidden itself.
        if self.oneshot_mode && !self.visible && self.oneshot_result.is_some() {
            return true;
        }
        false
    }
}

// ── Shared update body ────────────────────────────────────────────────────────

impl RofiApp {
    fn do_update(&mut self, ctx: &egui::Context) {
        // Hot-reload theme when config file changes (only when not in themes mode).
        if !self.input_is_themes {
            self.maybe_reload_theme();
        }

        // ── SIGUSR1 toggle ────────────────────────────────────────────────────
        // Process at most ONE toggle per frame.  If two signals arrive between
        // frames (show+hide collapsed), process show now and leave the hide for
        // the next frame.  This prevents show+hide from both running in a single
        // egui::run() call and fighting over the last InnerSize command.
        let pending = self
            .toggle
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                if n > 0 {
                    Some(n - 1)
                } else {
                    None
                }
            });
        if pending.is_ok() {
            // If pending_mode is set this is an explicit show-on-tab request
            // (--password / --clipboard / --client).  Always show — never
            // toggle to hide — so a second hotkey press while already visible
            // doesn't accidentally hide and leave the socket client hanging.
            let has_pending_mode = self.pending_mode.lock().unwrap().is_some();
            let want_show = has_pending_mode || !self.visible;
            if want_show && !self.visible {
                // Show the window.
                self.visible = true;
                #[cfg(target_os = "macos")]
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                #[cfg(not(target_os = "macos"))]
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1280.0, 550.0)));
                #[cfg(target_os = "macos")]
                {
                    self.prev_app = capture_previous_app();
                }
                self.query.clear();
                self.frame_count = 0;
                self.toast = None;
                self.selected = 0;
                self.request_scroll_reset();

                let new_items = self.pending_input.lock().unwrap().take();
                let is_themes = *self.pending_input_is_themes.lock().unwrap();
                if let Some(items) = new_items {
                    self.input_items = items;
                    self.input_is_themes = is_themes;
                    if is_themes {
                        self.theme_before_preview = Some(self.theme.clone());
                        self.mode = Mode::Themes;
                        let active = self.theme.name;
                        if let Some(pos) = self
                            .input_items
                            .iter()
                            .position(|s| s.trim_start_matches("* ") == active)
                        {
                            self.selected = pos;
                        } else {
                            self.selected = 0;
                        }
                    } else {
                        self.mode = Mode::Input;
                        self.selected = 0;
                    }
                    self.refilter_input();
                } else {
                    self.input_is_themes = false;
                    let requested = self.pending_mode.lock().unwrap().take();
                    match requested.as_deref() {
                        Some("pass") => {
                            self.mode = Mode::Pass;
                        }
                        Some("clip") => {
                            self.mode = Mode::Clipboard;
                            self.sync_clipboard();
                            #[cfg(target_os = "linux")]
                            self.preload_clipboard_textures(ctx);
                        }
                        Some("files") => {
                            self.mode = Mode::Files;
                            self.file_pane.scan();
                        }
                        _ => {
                            self.mode = Mode::Apps;
                        }
                    }
                    self.sync_apps();
                    self.refilter(true);
                }
                *self.pending_input_is_themes.lock().unwrap() = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.update_tray();
            } else if want_show && self.visible {
                // Already visible but a new tab was requested — switch tab and
                // reset state without hiding/showing the surface.
                self.query.clear();
                self.frame_count = 0;
                self.toast = None;
                self.selected = 0;
                self.request_scroll_reset();
                self.input_is_themes = false;
                let requested = self.pending_mode.lock().unwrap().take();
                match requested.as_deref() {
                    Some("pass") => {
                        self.mode = Mode::Pass;
                    }
                    Some("clip") => {
                        self.mode = Mode::Clipboard;
                        self.sync_clipboard();
                        #[cfg(target_os = "linux")]
                        self.preload_clipboard_textures(ctx);
                    }
                    Some("files") => {
                        self.mode = Mode::Files;
                        self.file_pane.scan();
                    }
                    _ => {
                        self.mode = Mode::Apps;
                    }
                }
                self.sync_apps();
                self.refilter(true);
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.update_tray();
            } else {
                // Plain toggle-off (no pending tab, window was visible).
                self.hide(ctx);
            }
        }
        // If there are still queued toggles (e.g. show was just processed but
        // a hide is still pending), request an immediate repaint so it isn't
        // delayed by the 50 ms poll timeout.
        if self.toggle.load(Ordering::Relaxed) > 0 {
            ctx.request_repaint();
        }

        if self.visible && self.mode != Mode::Input && self.mode != Mode::Themes {
            let new_items = self.pending_input.lock().unwrap().take();
            if let Some(items) = new_items {
                let is_themes = *self.pending_input_is_themes.lock().unwrap();
                self.input_items = items;
                self.input_is_themes = is_themes;
                if is_themes {
                    self.theme_before_preview = Some(self.theme.clone());
                    self.mode = Mode::Themes;
                    let active = self.theme.name;
                    if let Some(pos) = self
                        .input_items
                        .iter()
                        .position(|s| s.trim_start_matches("* ") == active)
                    {
                        self.selected = pos;
                    } else {
                        self.selected = 0;
                    }
                } else {
                    self.mode = Mode::Input;
                    self.selected = 0;
                }
                self.query.clear();
                self.refilter_input();
                *self.pending_input_is_themes.lock().unwrap() = false;
                self.update_tray();
            }
        }

        // When hidden: slow down the repaint poll, force fully transparent
        // background so nothing is painted, and return early.
        if !self.visible {
            // Only repaint when we need to check for a toggle signal.
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            // Override panel_fill to transparent so egui doesn't paint its
            // default dark-grey background over our transparent surface.
            let mut style = (*ctx.style()).clone();
            style.visuals.panel_fill = Color32::TRANSPARENT;
            style.visuals.window_fill = Color32::TRANSPARENT;
            ctx.set_style(style);
            // An empty CentralPanel is required so egui doesn't warn about
            // unclaimed space and doesn't fill it with the old style colour.
            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show(ctx, |_ui| {});
            return;
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(50));

        if self.should_close {
            self.hide(ctx);
            return;
        }

        self.frame_count = self.frame_count.saturating_add(1);

        // Track whether we have ever had keyboard focus this session.
        // On Wayland the focus enter event may arrive after several frames,
        // so we must not auto-hide due to "no focus" before it has ever arrived.
        if ctx.input(|i| i.focused) {
            self.had_keyboard_focus_ever = true;
        }

        // Auto-hide on focus loss.  On Wayland the focus event arrives later
        // than on macOS, so we allow more frames before checking.  Also, we
        // must not check until focus has been received at least once — otherwise
        // the window would close immediately if the compositor is slow to grant
        // keyboard focus.
        let focus_grace = if cfg!(target_os = "macos") { 5 } else { 100 };
        if self.had_keyboard_focus_ever
            && self.frame_count > focus_grace
            && !ctx.input(|i| i.focused)
        {
            self.hide(ctx);
            return;
        }

        // ── Global key handling — MUST happen before any widget rendering ─────
        // Escape / Cmd+R: hide.
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.hide(ctx);
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::R)) {
            self.hide(ctx);
            return;
        }

        // Consume ALL Ctrl+J / Ctrl+K events here — before TextEdit steals
        // them.  We count repeats so holding the key moves multiple rows.
        let mut ctrl_j_count: usize = 0;
        let mut ctrl_k_count: usize = 0;
        // In Files mode, Ctrl+H/L switch panes.
        let mut ctrl_h_pressed = false;
        let mut ctrl_l_pressed = false;
        ctx.input_mut(|i| {
            i.events.retain(|ev| {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = ev
                {
                    if modifiers.ctrl {
                        if *key == Key::J {
                            ctrl_j_count += 1;
                            return false; // consume
                        }
                        if *key == Key::K {
                            ctrl_k_count += 1;
                            return false; // consume
                        }
                        if *key == Key::H {
                            ctrl_h_pressed = true;
                            return false;
                        }
                        if *key == Key::L {
                            ctrl_l_pressed = true;
                            return false;
                        }
                    }
                }
                true // keep
            });
        });

        if self.mode == Mode::Clipboard {
            self.sync_clipboard();
            self.refilter(false);
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        let mut style = (*ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        ctx.set_style(style);

        // Snapshot theme for this frame (avoids borrow issues inside closures).
        let t = self.theme.clone();

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let panel_rect = ui.max_rect();

                ui.painter().rect_filled(
                    panel_rect,
                    Rounding::ZERO,
                    Color32::from_rgba_unmultiplied(t.bg.r(), t.bg.g(), t.bg.b(), t.bg_alpha),
                );
                ui.painter()
                    .rect_stroke(panel_rect, Rounding::ZERO, Stroke::new(1.5, t.border));

                let inner = panel_rect.shrink2(Vec2::new(16.0, 14.0));
                ui.allocate_ui_at_rect(inner, |ui| {
                    ui.vertical(|ui| {
                        // ── Mode tabs ─────────────────────────────────────
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;

                            let normal_tabs: &[(Mode, &str)] = &[
                                (Mode::Apps, "Apps"),
                                (Mode::Clipboard, "Clipboard"),
                                (Mode::Pass, "Pass"),
                                (Mode::Files, "Files"),
                                (Mode::Themes, "Themes"),
                                (Mode::About, "About"),
                            ];
                            // \u{F53F} = nf-md-palette (󰔿) — theme/palette icon
                            let input_tab: &[(Mode, &str)] = &[(Mode::Input, "Input")];
                            let themes_tab: &[(Mode, &str)] = &[(Mode::Themes, "Themes")];
                            let tabs = match self.mode {
                                Mode::Input => input_tab,
                                // Themes tab is now also in normal_tabs, so just fall through.
                                _ => normal_tabs,
                            };
                            let _ = themes_tab; // suppress unused warning

                            for &(mode, label) in tabs {
                                let selected = self.mode == mode;
                                let btn = egui::Button::new(
                                    egui::RichText::new(label)
                                        .font(FontId::new(14.0, FontFamily::Monospace))
                                        .color(if selected { t.accent } else { t.fg_muted }),
                                )
                                .fill(if selected {
                                    t.tab_active_bg
                                } else {
                                    Color32::TRANSPARENT
                                })
                                .stroke(if selected {
                                    Stroke::new(1.0, t.border)
                                } else {
                                    Stroke::NONE
                                })
                                .rounding(Rounding::ZERO);

                                let resp = ui.add(btn);
                                if resp.clicked() && mode != Mode::Input {
                                    if mode == Mode::Themes {
                                        self.enter_themes_mode();
                                    } else {
                                        self.mode = mode;
                                        self.query.clear();
                                        if mode == Mode::Clipboard {
                                            self.sync_clipboard();
                                            #[cfg(target_os = "linux")]
                                            self.preload_clipboard_textures(ctx);
                                        }
                                        self.refilter(true);
                                    }
                                    self.update_tray();
                                }
                            }

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new("Mofi")
                                            .font(FontId::new(15.0, self.medium_font.clone()))
                                            .color(t.brand),
                                    );
                                },
                            );
                        });

                        ui.add_space(10.0);

                        // ── Search bar ────────────────────────────────────
                        let hint = match self.mode {
                            Mode::Apps => "Search apps…",
                            Mode::Clipboard => "Filter clipboard…",
                            Mode::Pass => "Search passwords…",
                            Mode::Files => "Filter files…",
                            Mode::Input => "Filter…",
                            Mode::Themes => "Filter themes…",
                            Mode::About => "",
                        };

                        let response = ui.add(
                            egui::TextEdit::singleline(&mut self.query)
                                .hint_text(egui::RichText::new(hint).color(t.fg_muted))
                                .font(FontId::new(20.0, FontFamily::Monospace))
                                .text_color(t.fg)
                                .frame(false)
                                .desired_width(f32::INFINITY),
                        );
                        response.request_focus();

                        let arrow_down = ctx.input(|i| i.key_pressed(Key::ArrowDown));
                        let arrow_up = ctx.input(|i| i.key_pressed(Key::ArrowUp));
                        let down_count = ctrl_j_count.max(if arrow_down { 1 } else { 0 });
                        let up_count = ctrl_k_count.max(if arrow_up { 1 } else { 0 });
                        let down = down_count > 0;
                        let up = up_count > 0;
                        let tab = ctx.input(|i| i.key_pressed(Key::Tab));
                        let enter = ctx.input(|i| i.key_pressed(Key::Enter));

                        if tab && self.mode != Mode::Input {
                            let next = match self.mode {
                                Mode::Apps => Mode::Clipboard,
                                Mode::Clipboard => Mode::Pass,
                                Mode::Pass => Mode::Files,
                                Mode::Files => Mode::Themes,
                                Mode::Themes => Mode::About,
                                Mode::About => Mode::Apps,
                                Mode::Input => Mode::Input,
                            };
                            if next == Mode::Themes {
                                self.enter_themes_mode();
                            } else {
                                self.mode = next;
                                self.query.clear();
                                self.zoxide_results.clear();
                                self.zoxide_last_query.clear();
                                self.drill_target = None;
                                if self.mode == Mode::Clipboard {
                                    self.sync_clipboard();
                                    #[cfg(target_os = "linux")]
                                    self.preload_clipboard_textures(ctx);
                                }
                                self.refilter(true);
                            }
                            self.update_tray();
                        }

                        if self.mode == Mode::Input || self.mode == Mode::Themes {
                            let len = self.input_filtered.len();
                            if down && len > 0 {
                                let new_sel = (self.selected + down_count).min(len - 1);
                                self.selected = new_sel;
                                if self.mode == Mode::Themes {
                                    self.preview_theme_at_selection();
                                }
                            }
                            if up && len > 0 {
                                self.selected = self.selected.saturating_sub(up_count);
                                if self.mode == Mode::Themes {
                                    self.preview_theme_at_selection();
                                }
                            }
                        } else if self.mode != Mode::Files {
                            let show_shell_row =
                                self.mode == Mode::Apps && self.query.trim().starts_with('!');
                            let len = self.filtered.len() + if show_shell_row { 1 } else { 0 };
                            if down && len > 0 {
                                let new_sel = (self.selected + down_count).min(len - 1);
                                self.selected = new_sel;
                            }
                            if up && len > 0 {
                                self.selected = self.selected.saturating_sub(up_count);
                            }
                        }

                        if enter && self.mode != Mode::Files {
                            self.execute_selected();
                            return;
                        }

                        // ── Files mode keyboard ───────────────────
                        if self.mode == Mode::Files {
                            // Build filtered index list so navigation respects the query.
                            let q = self.query.to_lowercase();
                            let q_tokens: Vec<&str> = q.split_whitespace().collect();
                            let has_space = q.contains(' ');

                            // ── Drill-target management ──
                            // When the user types a space while focused on a
                            // zoxide row, lock that path as the drill target.
                            // When the space is deleted, release it.
                            if !has_space {
                                self.drill_target = None;
                            }
                            // Set drill target on first space if currently on a zoxide row
                            if has_space && self.drill_target.is_none() {
                                let zc = if q_tokens.is_empty() {
                                    0
                                } else {
                                    self.zoxide_results.len()
                                };
                                if self.selected < zc {
                                    self.drill_target =
                                        Some(self.zoxide_results[self.selected].clone());
                                    // Jump selection to first child (index 1;
                                    // index 0 is the locked zoxide row).
                                    self.selected = 1;
                                }
                            }

                            let drill_mode = self.drill_target.is_some();

                            if drill_mode {
                                // ── DRILL MODE ──
                                // Child tokens = everything after the first space.
                                let child_tokens: Vec<&str> = if q_tokens.len() >= 2 {
                                    q_tokens[1..].to_vec()
                                } else {
                                    Vec::new()
                                };
                                let drill_path = self.drill_target.clone().unwrap();
                                let drill_children: Vec<crate::files::FileEntry> =
                                    crate::files::dir_children(
                                        &std::path::PathBuf::from(&drill_path),
                                        &child_tokens.iter().copied().collect::<Vec<_>>(),
                                    );
                                // Virtual list: row 0 = locked zoxide path,
                                // rows 1..N = children.
                                let total = 1 + drill_children.len();

                                // Clamp selection (keep >=1 so we stay on children
                                // after the initial switch, but allow 0 to re-focus
                                // the parent row).
                                if self.selected >= total {
                                    self.selected = if total > 1 { 1 } else { 0 };
                                }

                                // Ctrl+H → exit drill mode, go up
                                if ctrl_h_pressed {
                                    self.drill_target = None;
                                    self.file_pane.go_up();
                                    self.query.clear();
                                    self.zoxide_results.clear();
                                    self.zoxide_last_query.clear();
                                    self.selected = 0;
                                }

                                // Enter / Ctrl+L
                                if ctrl_l_pressed || enter {
                                    if self.selected == 0 {
                                        // Enter on the drill target itself → navigate into it
                                        let path = std::path::PathBuf::from(&drill_path);
                                        if path.is_dir() {
                                            self.file_pane = crate::files::Pane::new(&path);
                                            self.file_pane.scan();
                                        }
                                        self.query.clear();
                                        self.zoxide_results.clear();
                                        self.zoxide_last_query.clear();
                                        self.drill_target = None;
                                        self.selected = 0;
                                    } else {
                                        let ci = self.selected - 1;
                                        if let Some(de) = drill_children.get(ci) {
                                            if de.is_dir {
                                                let p = de.path.clone();
                                                self.file_pane = crate::files::Pane::new(&p);
                                                self.file_pane.scan();
                                                self.query.clear();
                                                self.zoxide_results.clear();
                                                self.zoxide_last_query.clear();
                                                self.drill_target = None;
                                                self.selected = 0;
                                            } else {
                                                crate::files::open_file(&de.path);
                                                self.should_close = true;
                                                self.oneshot_result = Some(None);
                                                return;
                                            }
                                        }
                                    }
                                }

                                // Navigate
                                if down && total > 0 {
                                    self.selected = (self.selected + down_count).min(total - 1);
                                }
                                if up && total > 0 {
                                    self.selected = self.selected.saturating_sub(up_count);
                                }
                            } else {
                                // ── NORMAL MODE (no drill) ──

                                let file_filtered: Vec<usize> = self
                                    .file_pane
                                    .entries
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, e)| {
                                        if q_tokens.is_empty() {
                                            return true;
                                        }
                                        let haystack = e.path.to_string_lossy().to_lowercase();
                                        q_tokens.iter().all(|tok| haystack.contains(tok))
                                    })
                                    .map(|(i, _)| i)
                                    .collect();

                                let zoxide_count = if q.is_empty() {
                                    0
                                } else {
                                    self.zoxide_results.len()
                                };
                                let total = file_filtered.len() + zoxide_count;

                                if total > 0 && self.selected >= total {
                                    self.selected = 0;
                                }

                                // Ctrl+H → go up to parent
                                if ctrl_h_pressed {
                                    self.file_pane.go_up();
                                    self.query.clear();
                                    self.zoxide_results.clear();
                                    self.zoxide_last_query.clear();
                                    self.drill_target = None;
                                    self.selected = 0;
                                }
                                // Ctrl+L or Enter
                                if ctrl_l_pressed || enter {
                                    if self.selected < zoxide_count {
                                        // Zoxide row selected — navigate to that directory
                                        let dir = self.zoxide_results[self.selected].clone();
                                        let path = std::path::PathBuf::from(&dir);
                                        if path.is_dir() {
                                            self.file_pane = crate::files::Pane::new(&path);
                                            self.file_pane.scan();
                                        }
                                        self.query.clear();
                                        self.zoxide_results.clear();
                                        self.zoxide_last_query.clear();
                                        self.drill_target = None;
                                        self.selected = 0;
                                    } else if let Some(&entry_idx) =
                                        file_filtered.get(self.selected - zoxide_count)
                                    {
                                        // Normal pane entry
                                        self.file_pane.selected = entry_idx;
                                        use crate::files::EnterAction;
                                        match self.file_pane.enter_selected() {
                                            Some(EnterAction::NavigatedDir) => {
                                                self.query.clear();
                                                self.zoxide_results.clear();
                                                self.zoxide_last_query.clear();
                                                self.drill_target = None;
                                                self.selected = 0;
                                            }
                                            Some(EnterAction::OpenFile(path)) => {
                                                crate::files::open_file(&path);
                                                self.should_close = true;
                                                self.oneshot_result = Some(None);
                                                return;
                                            }
                                            None => {}
                                        }
                                    }
                                }
                                // Navigate within combined list
                                if down && total > 0 {
                                    self.selected = (self.selected + down_count).min(total - 1);
                                }
                                if up && total > 0 {
                                    self.selected = self.selected.saturating_sub(up_count);
                                }
                            }
                        }

                        if response.changed() {
                            if self.mode == Mode::Input || self.mode == Mode::Themes {
                                self.refilter_input();
                                if self.mode == Mode::Themes {
                                    self.preview_theme_at_selection();
                                }
                            } else {
                                self.refilter(true);
                            }
                            if self.mode == Mode::Files {
                                self.update_zoxide(&self.query.clone());
                            }
                        }

                        if down || up || response.changed() {
                            self.update_tray();
                        }

                        ui.add_space(6.0);
                        let sep_y = ui.cursor().top();
                        ui.painter().hline(
                            ui.max_rect().x_range(),
                            sep_y,
                            Stroke::new(1.0, t.separator),
                        );
                        ui.add_space(6.0);

                        // ── Panels ────────────────────────────────────────
                        let breadcrumb_h = if self.mode == Mode::Files { 34.0 } else { 0.0 };
                        let max_list_height = ui.available_height() - breadcrumb_h;

                        if self.mode == Mode::About {
                            ui.add_space(18.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("Mofi")
                                        .font(FontId::new(27.0, self.medium_font.clone()))
                                        .color(t.accent),
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new("v0.1.0")
                                        .font(FontId::new(14.0, FontFamily::Monospace))
                                        .color(t.fg_muted),
                                );
                                ui.add_space(14.0);
                                ui.label(
                                    egui::RichText::new("App launcher · Clipboard · Pass")
                                        .font(FontId::new(15.0, FontFamily::Monospace))
                                        .color(t.fg_dim),
                                );
                                ui.add_space(14.0);
                                ui.label(
                                    egui::RichText::new("\u{F09B}  github.com/bechampion/mofi")
                                        .font(FontId::new(15.0, FontFamily::Monospace))
                                        .color(t.accent2),
                                );
                                ui.add_space(14.0);
                                ui.label(
                                    egui::RichText::new(format!("Theme: {}", t.name))
                                        .font(FontId::new(14.0, FontFamily::Monospace))
                                        .color(t.fg_muted),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(
                                        "Super/Mod key  open · Esc  close · Tab  cycle tabs",
                                    )
                                    .font(FontId::new(13.0, FontFamily::Monospace))
                                    .color(t.fg_muted),
                                );
                            });
                        } else if self.mode == Mode::Input || self.mode == Mode::Themes {
                            // ── Input / theme picker list ─────────────────
                            let scroll = egui::ScrollArea::vertical()
                                .id_source(("mofi_input", self.scroll_generation))
                                .max_height(max_list_height);
                            scroll.show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                if self.input_filtered.is_empty() {
                                    ui.add_space(20.0);
                                    ui.centered_and_justified(|ui| {
                                        ui.label(
                                            egui::RichText::new("No results")
                                                .font(FontId::new(15.0, FontFamily::Monospace))
                                                .color(t.fg_muted),
                                        );
                                    });
                                    return;
                                }
                                let aw = ui.available_width();
                                // Collect a snapshot so we don't hold a borrow on self.input_filtered
                                // while potentially mutating self inside the loop.
                                let rows: Vec<(usize, usize)> =
                                    self.input_filtered.iter().copied().enumerate().collect();
                                let is_themes = self.mode == Mode::Themes;
                                for (row_idx, item_idx) in rows {
                                    let sel = row_idx == self.selected;
                                    let text = self.input_items[item_idx].clone();
                                    // Per-row icon: theme-specific glyph in themes mode, plain list bullet otherwise.
                                    let row_icon = if is_themes {
                                        theme_glyph_for(text.trim_start_matches("* "))
                                    } else {
                                        "\u{F0CA}" // nf-fa-list_ul
                                    };
                                    let (rr, _) = ui.allocate_exact_size(
                                        Vec2::new(aw, ROW_HEIGHT),
                                        egui::Sense::hover(),
                                    );
                                    if sel && self.selected != self.last_scroll_to {
                                        self.last_scroll_to = self.selected;
                                        ui.scroll_to_rect(rr, None);
                                    }
                                    if sel {
                                        ui.painter().rect_filled(rr, Rounding::ZERO, t.row_sel);
                                        ui.painter().rect_filled(
                                            egui::Rect::from_min_size(
                                                egui::pos2(rr.left(), rr.top() + 4.0),
                                                Vec2::new(3.0, rr.height() - 8.0),
                                            ),
                                            Rounding::ZERO,
                                            t.accent,
                                        );
                                    } else if false && ui.rect_contains_pointer(rr) {
                                        ui.painter().rect_filled(rr, Rounding::ZERO, t.row_hover);
                                    }

                                    let ix = rr.left() + 14.0;
                                    ui.painter().text(
                                        egui::pos2(ix + ICON_SIZE / 2.0, rr.center().y),
                                        egui::Align2::CENTER_CENTER,
                                        row_icon,
                                        FontId::new(ICON_SIZE * 0.75, FontFamily::Monospace),
                                        if sel { t.icon_sel } else { t.icon_dim },
                                    );
                                    ui.painter().text(
                                        egui::pos2(ix + ICON_SIZE + 12.0, rr.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        &text,
                                        FontId::new(16.0, self.medium_font.clone()),
                                        if sel { t.fg } else { t.fg_dim },
                                    );
                                    let click = ui.interact(
                                        rr,
                                        egui::Id::new(("input_row", row_idx)),
                                        egui::Sense::click(),
                                    );
                                    if click.hovered() && false {
                                        if self.mode == Mode::Themes && self.selected != row_idx {
                                            self.selected = row_idx;
                                            self.preview_theme_at_selection();
                                        } else {
                                            self.selected = row_idx;
                                        }
                                    }
                                    if click.double_clicked() && false {
                                        self.selected = row_idx;
                                        self.execute_selected();
                                        return;
                                    }
                                }
                            });
                        } else if self.mode == Mode::Files {
                            // ── Single-pane file explorer ─────────────
                            use crate::files::{format_size, format_time, glyph_for_file};

                            // Snapshot the data we need so the scroll closure doesn't
                            // borrow self.file_pane (which would conflict with
                            // self.load_image_texture).
                            let q = self.query.to_lowercase();
                            let cwd_display = self.file_pane.cwd.to_string_lossy().to_string();
                            let q_tokens: Vec<&str> = q.split_whitespace().collect();

                            let drill_target = self.drill_target.clone();
                            let drill_mode = drill_target.is_some();

                            struct RowData {
                                name: String,
                                path: String,
                                is_dir: bool,
                                size: u64,
                                modified: i64,
                                owner: String,
                                glyph: &'static str,
                                is_git: bool,
                            }

                            // In drill mode we show: locked zoxide row (idx 0)
                            // + children.  In normal mode: zoxide rows + file entries.
                            let locked_zoxide: Option<String>;
                            let zoxide_rows: Vec<String>;
                            let file_rows: Vec<RowData>;

                            if drill_mode {
                                let dt = drill_target.unwrap();
                                locked_zoxide = Some(dt.clone());
                                zoxide_rows = Vec::new(); // not shown in drill mode

                                let child_tokens: Vec<&str> = if q_tokens.len() >= 2 {
                                    q_tokens[1..].to_vec()
                                } else {
                                    Vec::new()
                                };
                                let children = crate::files::dir_children(
                                    &std::path::PathBuf::from(&dt),
                                    &child_tokens,
                                );
                                file_rows = children
                                    .iter()
                                    .map(|e| RowData {
                                        name: e.name.clone(),
                                        path: e.path.to_string_lossy().to_string(),
                                        is_dir: e.is_dir,
                                        size: e.size,
                                        modified: e.modified,
                                        owner: e.owner.clone(),
                                        glyph: glyph_for_file(e),
                                        is_git: e.source == crate::files::Source::Git,
                                    })
                                    .collect();
                            } else {
                                locked_zoxide = None;
                                zoxide_rows = if q.is_empty() {
                                    Vec::new()
                                } else {
                                    self.zoxide_results.clone()
                                };

                                let file_filtered: Vec<usize> = self
                                    .file_pane
                                    .entries
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, e)| {
                                        if q_tokens.is_empty() {
                                            return true;
                                        }
                                        let haystack = e.path.to_string_lossy().to_lowercase();
                                        q_tokens.iter().all(|tok| haystack.contains(tok))
                                    })
                                    .map(|(i, _)| i)
                                    .collect();

                                file_rows = file_filtered
                                    .iter()
                                    .map(|&i| {
                                        let e = &self.file_pane.entries[i];
                                        RowData {
                                            name: e.name.clone(),
                                            path: e.path.to_string_lossy().to_string(),
                                            is_dir: e.is_dir,
                                            size: e.size,
                                            modified: e.modified,
                                            owner: e.owner.clone(),
                                            glyph: glyph_for_file(e),
                                            is_git: e.source == crate::files::Source::Git,
                                        }
                                    })
                                    .collect();
                            }

                            let zoxide_count = zoxide_rows.len();
                            // In drill mode, row 0 = locked zoxide row, rest = children
                            let drill_offset: usize = if locked_zoxide.is_some() { 1 } else { 0 };
                            let selected = self.selected;

                            // Scrollable file list
                            let scroll = egui::ScrollArea::vertical()
                                .id_source(("mofi_files", self.scroll_generation))
                                .max_height(max_list_height);
                            scroll.show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                let total = file_rows.len() + zoxide_count + drill_offset;
                                if total == 0 {
                                    ui.add_space(20.0);
                                    ui.centered_and_justified(|ui| {
                                        ui.label(
                                            egui::RichText::new("Empty")
                                                .font(FontId::new(15.0, FontFamily::Monospace))
                                                .color(t.fg_muted),
                                        );
                                    });
                                    return;
                                }
                                let row_h = ROW_HEIGHT;
                                let aw = ui.available_width();

                                // ── Locked zoxide row (drill mode only, always idx 0) ──
                                if let Some(ref lz) = locked_zoxide {
                                    let sel = selected == 0;
                                    let (rr, _) = ui.allocate_exact_size(
                                        Vec2::new(aw, row_h),
                                        egui::Sense::hover(),
                                    );
                                    if sel {
                                        ui.scroll_to_rect(rr, None);
                                        ui.painter().rect_filled(rr, Rounding::ZERO, t.row_sel);
                                    }
                                    let ix = rr.left() + 4.0;
                                    let gc = if sel { t.accent2 } else { dim_color(t.accent2) };
                                    ui.painter().text(
                                        egui::pos2(ix + ICON_SIZE * 0.5, rr.center().y),
                                        egui::Align2::CENTER_CENTER,
                                        "\u{F126D}", // nf-md-folder_marker
                                        FontId::new(16.0, FontFamily::Monospace),
                                        gc,
                                    );
                                    let name_x = ix + ICON_SIZE + 8.0;
                                    let name_color =
                                        if sel { t.accent2 } else { dim_color(t.accent2) };
                                    ui.painter().text(
                                        egui::pos2(name_x, rr.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        lz,
                                        FontId::new(14.0, FontFamily::Monospace),
                                        name_color,
                                    );

                                    // Separator after locked row
                                    if !file_rows.is_empty() {
                                        ui.add_space(2.0);
                                        let sep_x = ui.cursor().left()..=ui.cursor().left() + aw;
                                        ui.painter().hline(
                                            sep_x,
                                            ui.cursor().top(),
                                            Stroke::new(0.5, t.separator),
                                        );
                                        ui.add_space(2.0);
                                    }
                                }

                                // ── Zoxide "jump to" rows (normal mode only) ──
                                for (zi, zpath) in zoxide_rows.iter().enumerate() {
                                    let sel = selected == zi;
                                    let (rr, _) = ui.allocate_exact_size(
                                        Vec2::new(aw, row_h),
                                        egui::Sense::hover(),
                                    );

                                    if sel {
                                        ui.scroll_to_rect(rr, None);
                                        ui.painter().rect_filled(rr, Rounding::ZERO, t.row_sel);
                                    }

                                    let ix = rr.left() + 4.0;

                                    let gc = if sel { t.accent2 } else { dim_color(t.accent2) };
                                    ui.painter().text(
                                        egui::pos2(ix + ICON_SIZE * 0.5, rr.center().y),
                                        egui::Align2::CENTER_CENTER,
                                        "\u{F126D}", // nf-md-folder_marker
                                        FontId::new(16.0, FontFamily::Monospace),
                                        gc,
                                    );

                                    let name_x = ix + ICON_SIZE + 8.0;
                                    let name_color =
                                        if sel { t.accent2 } else { dim_color(t.accent2) };
                                    ui.painter().text(
                                        egui::pos2(name_x, rr.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        zpath,
                                        FontId::new(14.0, FontFamily::Monospace),
                                        name_color,
                                    );
                                }

                                // ── Separator between zoxide and file entries (normal mode) ──
                                if !zoxide_rows.is_empty() && !file_rows.is_empty() {
                                    ui.add_space(2.0);
                                    let sep_x = ui.cursor().left()..=ui.cursor().left() + aw;
                                    ui.painter().hline(
                                        sep_x,
                                        ui.cursor().top(),
                                        Stroke::new(0.5, t.separator),
                                    );
                                    ui.add_space(2.0);
                                }

                                // ── File/child entry rows ────────────────────
                                // In drill mode: offset by 1 (locked zoxide at idx 0).
                                // In normal mode: offset by zoxide_count.
                                let row_offset = zoxide_count + drill_offset;
                                for (fi, row) in file_rows.iter().enumerate() {
                                    let combined_idx = row_offset + fi;
                                    let sel = selected == combined_idx;
                                    let (rr, _) = ui.allocate_exact_size(
                                        Vec2::new(aw, row_h),
                                        egui::Sense::hover(),
                                    );

                                    if sel {
                                        ui.scroll_to_rect(rr, None);
                                        ui.painter().rect_filled(rr, Rounding::ZERO, t.row_sel);
                                    }

                                    let glyph_color =
                                        if row.is_dir { t.accent } else { t.fg_muted };
                                    let gc = if sel {
                                        glyph_color
                                    } else {
                                        dim_color(glyph_color)
                                    };

                                    let ix = rr.left() + 4.0;

                                    // Glyph icon
                                    ui.painter().text(
                                        egui::pos2(ix + ICON_SIZE * 0.5, rr.center().y),
                                        egui::Align2::CENTER_CENTER,
                                        row.glyph,
                                        FontId::new(16.0, FontFamily::Monospace),
                                        gc,
                                    );

                                    // Name (with match highlighting)
                                    let name_x = ix + ICON_SIZE + 8.0;
                                    let name_color = if sel { t.fg } else { t.fg_dim };
                                    let highlight_color = t.accent;
                                    let name_font = FontId::new(14.0, FontFamily::Monospace);

                                    if q_tokens.is_empty() {
                                        ui.painter().text(
                                            egui::pos2(name_x, rr.center().y),
                                            egui::Align2::LEFT_CENTER,
                                            &row.name,
                                            name_font.clone(),
                                            name_color,
                                        );
                                    } else {
                                        // Build a mask of which chars are highlighted
                                        let name_lower = row.name.to_lowercase();
                                        let mut highlighted = vec![false; row.name.len()];
                                        for tok in &q_tokens {
                                            let tok_lower = tok.to_lowercase();
                                            let mut start = 0;
                                            while let Some(pos) =
                                                name_lower[start..].find(&tok_lower)
                                            {
                                                let abs = start + pos;
                                                for i in abs..abs + tok_lower.len() {
                                                    if i < highlighted.len() {
                                                        highlighted[i] = true;
                                                    }
                                                }
                                                start = abs + 1;
                                            }
                                        }
                                        // Paint segments with alternating colors
                                        let mut cx = name_x;
                                        let mut seg_start = 0;
                                        while seg_start < row.name.len() {
                                            let is_hl = highlighted[seg_start];
                                            let mut seg_end = seg_start + 1;
                                            while seg_end < row.name.len()
                                                && highlighted[seg_end] == is_hl
                                            {
                                                seg_end += 1;
                                            }
                                            let seg = &row.name[seg_start..seg_end];
                                            let col =
                                                if is_hl { highlight_color } else { name_color };
                                            let galley = ui.painter().layout_no_wrap(
                                                seg.to_string(),
                                                name_font.clone(),
                                                col,
                                            );
                                            let w = galley.rect.width();
                                            ui.painter().galley(
                                                egui::pos2(
                                                    cx,
                                                    rr.center().y - galley.rect.height() * 0.5,
                                                ),
                                                galley,
                                                col,
                                            );
                                            cx += w;
                                            seg_start = seg_end;
                                        }
                                    }

                                    // Right-aligned metadata: owner  date  size
                                    let meta_font = FontId::new(11.0, FontFamily::Monospace);
                                    let size_color = t.accent;
                                    let date_color = t.accent2;
                                    let owner_color = t.fg_muted;
                                    let dim = |c: egui::Color32| -> egui::Color32 {
                                        egui::Color32::from_rgba_premultiplied(
                                            (c.r() as u16 * 2 / 3) as u8,
                                            (c.g() as u16 * 2 / 3) as u8,
                                            (c.b() as u16 * 2 / 3) as u8,
                                            c.a(),
                                        )
                                    };
                                    let mut rx = rr.right() - 8.0;

                                    // Size (right-most)
                                    let size_col = 56.0;
                                    if !row.is_dir {
                                        let size_str = format_size(row.size);
                                        ui.painter().text(
                                            egui::pos2(rx, rr.center().y),
                                            egui::Align2::RIGHT_CENTER,
                                            &size_str,
                                            meta_font.clone(),
                                            if sel { size_color } else { dim(size_color) },
                                        );
                                    }
                                    rx -= size_col;

                                    // Date
                                    let date_str = format_time(row.modified);
                                    ui.painter().text(
                                        egui::pos2(rx, rr.center().y),
                                        egui::Align2::RIGHT_CENTER,
                                        &date_str,
                                        meta_font.clone(),
                                        if sel { date_color } else { dim(date_color) },
                                    );
                                    rx -= 100.0;

                                    // Owner
                                    ui.painter().text(
                                        egui::pos2(rx, rr.center().y),
                                        egui::Align2::RIGHT_CENTER,
                                        &row.owner,
                                        meta_font.clone(),
                                        if sel { owner_color } else { dim(owner_color) },
                                    );
                                    rx -= 70.0;

                                    // Git badge (rightmost badge column)
                                    if row.is_git {
                                        let badge_color = t.accent2;
                                        ui.painter().text(
                                            egui::pos2(rx, rr.center().y),
                                            egui::Align2::RIGHT_CENTER,
                                            "\u{EA84}", // nf-cod-github
                                            meta_font,
                                            if sel { badge_color } else { dim(badge_color) },
                                        );
                                    }
                                }
                            });

                            // ── Path bar (pinned to bottom of panel) ──
                            // Paint at an absolute Y near the bottom of the
                            // available rect so it never moves with content.
                            let outer = ui.max_rect();
                            let bar_y = outer.bottom() - 6.0;
                            let bar_left = outer.left() + 8.0;
                            let bar_right = outer.right() - 8.0;

                            // Separator line above the path
                            ui.painter().hline(
                                outer.left()..=outer.right(),
                                bar_y - 14.0,
                                Stroke::new(0.5, t.separator),
                            );

                            // Determine the full path to display
                            let breadcrumb_path: String;
                            if let Some(ref lz) = locked_zoxide {
                                if selected == 0 {
                                    breadcrumb_path = lz.clone();
                                } else if selected - 1 < file_rows.len() {
                                    breadcrumb_path = file_rows[selected - 1].path.clone();
                                } else {
                                    breadcrumb_path = lz.clone();
                                }
                            } else if selected < zoxide_count {
                                breadcrumb_path = zoxide_rows[selected].clone();
                            } else if selected - zoxide_count < file_rows.len() {
                                breadcrumb_path = file_rows[selected - zoxide_count].path.clone();
                            } else {
                                breadcrumb_path = cwd_display.clone();
                            }

                            // Rainbow palette for path components
                            let rainbow: &[egui::Color32] = &[
                                egui::Color32::from_rgb(255, 107, 107), // red
                                egui::Color32::from_rgb(255, 180, 107), // orange
                                egui::Color32::from_rgb(255, 238, 140), // yellow
                                egui::Color32::from_rgb(140, 255, 170), // green
                                egui::Color32::from_rgb(130, 210, 255), // blue
                                egui::Color32::from_rgb(180, 150, 255), // indigo
                                egui::Color32::from_rgb(230, 150, 255), // violet
                            ];

                            let bc_font = FontId::new(14.0, self.medium_font.clone());
                            let slash_font = FontId::new(14.0, FontFamily::Monospace);

                            let components: Vec<&str> = breadcrumb_path
                                .split('/')
                                .filter(|s| !s.is_empty())
                                .collect();

                            let mut cx = bar_left;

                            // Leading /
                            let r = ui.painter().text(
                                egui::pos2(cx, bar_y),
                                egui::Align2::LEFT_CENTER,
                                "/",
                                slash_font.clone(),
                                t.fg_muted,
                            );
                            cx = r.right();

                            for (ci, comp) in components.iter().enumerate() {
                                let color = rainbow[ci % rainbow.len()];

                                let r = ui.painter().text(
                                    egui::pos2(cx, bar_y),
                                    egui::Align2::LEFT_CENTER,
                                    comp,
                                    bc_font.clone(),
                                    color,
                                );
                                cx = r.right();

                                // Slash after each component (including last for dirs,
                                // skip for last if it's a file)
                                if ci < components.len() - 1 {
                                    let r = ui.painter().text(
                                        egui::pos2(cx, bar_y),
                                        egui::Align2::LEFT_CENTER,
                                        "/",
                                        slash_font.clone(),
                                        t.fg_muted,
                                    );
                                    cx = r.right();
                                }

                                if cx > bar_right {
                                    break;
                                }
                            }
                        } else {
                            let scroll = egui::ScrollArea::vertical()
                                .id_source(("mofi_results", self.scroll_generation))
                                .max_height(max_list_height);
                            scroll.show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                // In Apps mode with a non-empty query, we show
                                // an extra "Run: <query>" row at the end so the
                                // user can execute arbitrary shell commands.
                                let show_shell_row =
                                    self.mode == Mode::Apps && self.query.trim().starts_with('!');
                                let total_rows =
                                    self.filtered.len() + if show_shell_row { 1 } else { 0 };
                                if total_rows == 0 {
                                    ui.add_space(20.0);
                                    ui.centered_and_justified(|ui| {
                                        ui.label(
                                            egui::RichText::new("No results")
                                                .font(FontId::new(15.0, FontFamily::Monospace))
                                                .color(t.fg_muted),
                                        );
                                    });
                                    return;
                                }
                                let aw = ui.available_width();
                                let filtered_snapshot: Vec<(usize, usize)> =
                                    self.filtered.iter().copied().enumerate().collect();
                                for (row_idx, item_idx) in filtered_snapshot {
                                    let sel = row_idx == self.selected;
                                    let item = &self.items[item_idx];
                                    let glyph = glyph_for_item(item);
                                    let gc = glyph_color_for_item(item, &t);
                                    let gc = if sel { gc } else { dim_color(gc) };
                                    let display = item.display_name();
                                    let subtitle = item.subtitle();
                                    let img_path = if let LaunchItem::Clip(ce) = item {
                                        ce.thumbnail_path
                                            .as_ref()
                                            .or(ce.image_path.as_ref())
                                            .cloned()
                                    } else {
                                        None
                                    };

                                    let (rr, _) = ui.allocate_exact_size(
                                        Vec2::new(aw, ROW_HEIGHT),
                                        egui::Sense::hover(),
                                    );
                                    if sel && self.selected != self.last_scroll_to {
                                        self.last_scroll_to = self.selected;
                                        ui.scroll_to_rect(rr, None);
                                    }

                                    if sel {
                                        ui.painter().rect_filled(rr, Rounding::ZERO, t.row_sel);
                                        ui.painter().rect_filled(
                                            egui::Rect::from_min_size(
                                                egui::pos2(rr.left(), rr.top() + 4.0),
                                                Vec2::new(3.0, rr.height() - 8.0),
                                            ),
                                            Rounding::ZERO,
                                            t.accent,
                                        );
                                    } else if false && ui.rect_contains_pointer(rr) {
                                        ui.painter().rect_filled(rr, Rounding::ZERO, t.row_hover);
                                    }

                                    let ix = rr.left() + 14.0;

                                    // Check if this is an image clipboard entry — show thumbnail.
                                    let mut drew_thumbnail = false;
                                    #[cfg(target_os = "linux")]
                                    if let Some(ref img_path) = img_path {
                                        if let Some((tex, _dims)) =
                                            self.load_image_texture(ctx, img_path)
                                        {
                                            let thumb_h = ROW_HEIGHT - 8.0;
                                            let [tw, th] = tex.size();
                                            let aspect = tw as f32 / th.max(1) as f32;
                                            let thumb_w = (thumb_h * aspect).min(thumb_h * 2.0);
                                            let thumb_rect = egui::Rect::from_min_size(
                                                egui::pos2(ix, rr.center().y - thumb_h / 2.0),
                                                Vec2::new(thumb_w, thumb_h),
                                            );
                                            ui.painter().image(
                                                tex.id(),
                                                thumb_rect,
                                                egui::Rect::from_min_max(
                                                    egui::pos2(0.0, 0.0),
                                                    egui::pos2(1.0, 1.0),
                                                ),
                                                Color32::WHITE,
                                            );
                                            let lx = ix + thumb_w + 10.0;
                                            ui.painter().text(
                                                egui::pos2(lx, rr.center().y - 7.0),
                                                egui::Align2::LEFT_CENTER,
                                                &display,
                                                FontId::new(14.0, self.medium_font.clone()),
                                                if sel { t.fg } else { t.fg_dim },
                                            );
                                            if let Some(ref sub) = subtitle {
                                                ui.painter().text(
                                                    egui::pos2(lx, rr.center().y + 7.0),
                                                    egui::Align2::LEFT_CENTER,
                                                    sub,
                                                    FontId::new(13.0, FontFamily::Monospace),
                                                    if sel { t.accent2 } else { t.fg_muted },
                                                );
                                            }
                                            drew_thumbnail = true;
                                        }
                                    }

                                    if !drew_thumbnail {
                                        ui.painter().text(
                                            egui::pos2(ix + ICON_SIZE / 2.0, rr.center().y),
                                            egui::Align2::CENTER_CENTER,
                                            glyph,
                                            FontId::new(ICON_SIZE * 0.75, FontFamily::Monospace),
                                            gc,
                                        );

                                        let tx = ix + ICON_SIZE + 12.0;
                                        if let Some(sub) = subtitle {
                                            ui.painter().text(
                                                egui::pos2(tx, rr.center().y - 7.0),
                                                egui::Align2::LEFT_CENTER,
                                                &display,
                                                FontId::new(14.0, self.medium_font.clone()),
                                                if sel { t.fg } else { t.fg_dim },
                                            );
                                            ui.painter().text(
                                                egui::pos2(tx, rr.center().y + 7.0),
                                                egui::Align2::LEFT_CENTER,
                                                &sub,
                                                FontId::new(13.0, FontFamily::Monospace),
                                                if sel { t.accent2 } else { t.fg_muted },
                                            );
                                        } else {
                                            ui.painter().text(
                                                egui::pos2(tx, rr.center().y),
                                                egui::Align2::LEFT_CENTER,
                                                &display,
                                                FontId::new(14.0, self.medium_font.clone()),
                                                if sel { t.fg } else { t.fg_dim },
                                            );
                                        }
                                    }

                                    let click = ui.interact(
                                        rr,
                                        egui::Id::new(("row", row_idx)),
                                        egui::Sense::click(),
                                    );
                                    if click.hovered() && false {
                                        self.selected = row_idx;
                                    }
                                    if click.double_clicked() && false {
                                        self.selected = row_idx;
                                        self.execute_selected();
                                        return;
                                    }
                                }

                                // ── Synthetic "Run in shell" row ──────────
                                if show_shell_row {
                                    let shell_row_idx = self.filtered.len();
                                    let sel = self.selected == shell_row_idx;
                                    let shell_cmd =
                                        self.query.trim().trim_start_matches('!').trim();
                                    let shell_label = format!("Run: {}", shell_cmd);
                                    let shell_glyph = "\u{F489}"; // nf-md-console_line

                                    let (rr, _) = ui.allocate_exact_size(
                                        Vec2::new(aw, ROW_HEIGHT),
                                        egui::Sense::hover(),
                                    );
                                    if sel && self.selected != self.last_scroll_to {
                                        self.last_scroll_to = self.selected;
                                        ui.scroll_to_rect(rr, None);
                                    }
                                    if sel {
                                        ui.painter().rect_filled(rr, Rounding::ZERO, t.row_sel);
                                        ui.painter().rect_filled(
                                            egui::Rect::from_min_size(
                                                egui::pos2(rr.left(), rr.top() + 4.0),
                                                Vec2::new(3.0, rr.height() - 8.0),
                                            ),
                                            Rounding::ZERO,
                                            t.accent,
                                        );
                                    }
                                    let ix = rr.left() + 14.0;
                                    ui.painter().text(
                                        egui::pos2(ix + ICON_SIZE / 2.0, rr.center().y),
                                        egui::Align2::CENTER_CENTER,
                                        shell_glyph,
                                        FontId::new(ICON_SIZE * 0.75, FontFamily::Monospace),
                                        if sel { t.accent } else { dim_color(t.accent) },
                                    );
                                    let tx = ix + ICON_SIZE + 12.0;
                                    ui.painter().text(
                                        egui::pos2(tx, rr.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        &shell_label,
                                        FontId::new(14.0, self.medium_font.clone()),
                                        if sel { t.fg } else { t.fg_dim },
                                    );
                                }
                            });
                        }

                        // ── Toast ─────────────────────────────────────────
                        if let Some((msg, since)) = &self.toast {
                            if since.elapsed().as_secs_f32() < 1.5 {
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(msg.as_str())
                                            .font(FontId::new(15.0, FontFamily::Monospace))
                                            .color(t.toast),
                                    );
                                });
                                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                            } else {
                                self.toast = None;
                            }
                        }
                    });
                });
            });
    }
}
