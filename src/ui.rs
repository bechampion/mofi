use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Key, Rounding, Stroke, Vec2,
};
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy,
    NSRunningApplication, NSWorkspace,
};

use crate::apps::discover_apps;
use crate::clipboard::{load_history, start_poller, ClipboardEntry, ClipboardHistory};
use crate::launcher::{launch_app, paste_text, LaunchItem, Launcher};
use crate::pass::discover_pass_entries;

/// Capture the currently active (frontmost) app that is not us.
/// Called right before we show the rofi window.
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

/// Restore focus to a previously captured app.
fn restore_app_focus(app: &NSRunningApplication) {
    app.activateWithOptions(NSApplicationActivationOptions(0));
}

const MAPLE_MONO_REGULAR: &str = "/Users/jgarcia/Library/Fonts/MapleMono-NF-Regular.ttf";
const MAPLE_MONO_MEDIUM: &str = "/Users/jgarcia/Library/Fonts/MapleMono-NF-Medium.ttf";
const ICON_SIZE: f32 = 24.0;
const ROW_HEIGHT: f32 = 38.0;
const MAX_VISIBLE_ROWS: usize = 7;

// ── Kanagawa palette ────────────────────────────────────────────────────────
#[allow(dead_code)]
mod kana {
    use eframe::egui::Color32;

    pub const fn hex(r: u8, g: u8, b: u8) -> Color32 {
        Color32::from_rgb(r, g, b)
    }

    // Backgrounds
    pub const SUMI_INK0: Color32 = hex(0x16, 0x16, 0x1D); // darkest bg
    pub const SUMI_INK1: Color32 = hex(0x1F, 0x1F, 0x28); // default bg
    pub const SUMI_INK2: Color32 = hex(0x2A, 0x2A, 0x37); // lighter bg
    pub const SUMI_INK3: Color32 = hex(0x36, 0x36, 0x46); // cursorline
    pub const SUMI_INK4: Color32 = hex(0x54, 0x54, 0x6D); // non-text / borders
    pub const WAVE_BLUE1: Color32 = hex(0x22, 0x32, 0x49); // popup bg / visual selection
    pub const WAVE_BLUE2: Color32 = hex(0x2D, 0x4F, 0x67); // popup selection / search

    // Foregrounds
    pub const FUJI_WHITE: Color32 = hex(0xDC, 0xD7, 0xBA); // default fg
    pub const OLD_WHITE: Color32 = hex(0xC8, 0xC0, 0x93); // dim fg / statusline
    pub const FUJI_GRAY: Color32 = hex(0x72, 0x71, 0x69); // comments
    pub const SPRING_VIOLET1: Color32 = hex(0x93, 0x8A, 0xA9); // light fg
    pub const ONI_VIOLET: Color32 = hex(0x95, 0x7F, 0xB8); // keywords
    pub const CRYSTAL_BLUE: Color32 = hex(0x7E, 0x9C, 0xD8); // functions / titles
    pub const SPRING_BLUE: Color32 = hex(0x7F, 0xB4, 0xCA); // specials
    pub const WAVE_AQUA2: Color32 = hex(0x7A, 0xA8, 0x9F); // types
    pub const SPRING_GREEN: Color32 = hex(0x98, 0xBB, 0x6C); // strings
    pub const BOAT_YELLOW2: Color32 = hex(0xC0, 0xA3, 0x6E); // operators
    pub const CARP_YELLOW: Color32 = hex(0xE6, 0xC3, 0x84); // identifiers
    pub const SAKURA_PINK: Color32 = hex(0xD2, 0x7E, 0x99); // numbers
    pub const WAVE_RED: Color32 = hex(0xE4, 0x68, 0x76); // builtins
    pub const SURIMI_ORANGE: Color32 = hex(0xFF, 0xA0, 0x66); // constants
}

// ── Nerd Font glyph lookup ───────────────────────────────────────────────────

fn glyph_for_item(item: &LaunchItem) -> &'static str {
    match item {
        LaunchItem::Clip(_) => "\u{F328}",  // nf-fa-clipboard
        LaunchItem::Pass(_) => "\u{F0756}", // nf-md-lock
        LaunchItem::App(a) => glyph_for_app(&a.name),
    }
}

fn glyph_for_app(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("safari") {
        "\u{E748}" // nf-dev-safari
    } else if n.contains("firefox") {
        "\u{E745}" // nf-dev-firefox
    } else if n.contains("chrome") || n.contains("chromium") {
        "\u{E743}" // nf-dev-chrome
    } else if n.contains("terminal")
        || n.contains("iterm")
        || n.contains("alacritty")
        || n.contains("warp")
        || n.contains("kitty")
        || n.contains("ghostty")
    {
        "\u{EA85}" // nf-cod-terminal
    } else if n.contains("code") || n.contains("vscode") || n.contains("cursor") {
        "\u{E8DA}" // nf-dev-vscode
    } else if n.contains("xcode") {
        "\u{E8E8}" // nf-dev-xcode
    } else if n.contains("sublime") {
        "\u{E7AA}" // nf-dev-sublime
    } else if n.contains("finder") {
        "\u{F0036}" // nf-md-apple_finder
    } else if n.contains("mail") {
        "\u{F0E0}" // nf-fa-envelope
    } else if n.contains("messages") {
        "\u{F27A}" // nf-fa-message
    } else if n.contains("calendar") {
        "\u{F073}" // nf-fa-calendar
    } else if n.contains("music") {
        "\u{F001}" // nf-fa-music
    } else if n.contains("spotify") {
        "\u{F1BC}" // nf-fa-spotify
    } else if n.contains("discord") {
        "\u{F1FF}" // nf-fa-discord
    } else if n.contains("slack") {
        "\u{E8A4}" // nf-dev-slack
    } else if n.contains("docker") {
        "\u{E7B0}" // nf-dev-docker
    } else if n.contains("github desktop") || n.contains("github") {
        "\u{E709}" // nf-dev-github
    } else if n.contains("figma") {
        "\u{E7DA}" // nf-dev-figma
    } else if n.contains("system preferences") || n.contains("system settings") {
        "\u{EB52}" // nf-cod-settings
    } else if n.contains("app store") {
        "\u{F0BD}" // nf-fa-app_store
    } else if n.contains("photos") {
        "\u{F03E}" // nf-fa-image
    } else if n.contains("notes") {
        "\u{F249}" // nf-fa-sticky_note
    } else if n.contains("maps") {
        "\u{F279}" // nf-fa-map
    } else if n.contains("calculator") {
        "\u{F1EC}" // nf-fa-calculator
    } else if n.contains("disk utility") {
        "\u{F02CA}" // nf-md-harddisk
    } else if n.contains("activity monitor") {
        "\u{F0128}" // nf-md-chart_bar
    } else if n.contains("time machine") {
        "\u{F006F}" // nf-md-backup_restore
    } else if n.contains("vlc") {
        "\u{F057C}" // nf-md-vlc
    } else if n.contains("steam") {
        "\u{F1B6}" // nf-fa-steam
    } else if n.contains("postman") {
        "\u{E86B}" // nf-dev-postman
    } else if n.contains("1password") || n.contains("onepassword") {
        "\u{F0881}" // nf-md-onepassword
    } else if n.contains("dropbox") {
        "\u{E707}" // nf-dev-dropbox
    } else {
        "\u{F2D0}" // nf-fa-window-maximize (fallback)
    }
}

// ── Glyph colour (accent per-item) ──────────────────────────────────────────

fn glyph_color_for_item(item: &LaunchItem) -> Color32 {
    match item {
        LaunchItem::Clip(_) => kana::SPRING_BLUE,
        LaunchItem::Pass(_) => kana::SPRING_GREEN,
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

// ── Font loading ─────────────────────────────────────────────────────────────

fn load_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    if let Ok(bytes) = std::fs::read(MAPLE_MONO_REGULAR) {
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

    if let Ok(bytes) = std::fs::read(MAPLE_MONO_MEDIUM) {
        fonts
            .font_data
            .insert("MapleMonoMedium".to_owned(), FontData::from_owned(bytes));
        fonts.families.insert(
            FontFamily::Name("medium".into()),
            vec!["MapleMonoMedium".to_owned()],
        );
    }

    ctx.set_fonts(fonts);
}

// ── Mode ─────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Mode {
    Apps,
    Clipboard,
    Pass,
    About,
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
    /// Shared clipboard history — updated by background poller.
    clip_history: ClipboardHistory,
    /// Feedback message shown briefly after copying a password ("Copied!").
    toast: Option<(String, std::time::Instant)>,
    /// Frame counter — ignore focus loss until the window has had time to appear.
    frame_count: u32,
    /// SIGUSR1 toggle flag — set by signal handler, polled each frame.
    toggle: Arc<AtomicBool>,
    /// Whether the window is currently visible.
    visible: bool,
    /// The app that was frontmost before we showed — restored on hide.
    prev_app: Option<Retained<NSRunningApplication>>,
    /// IPC slot: when a Pass entry is selected, daemon puts the name here
    /// for the client to pick up and decrypt.
    pending_entry: Arc<Mutex<Option<Option<String>>>>,
}

impl RofiApp {
    pub fn new(
        cc: &eframe::CreationContext,
        toggle: Arc<AtomicBool>,
        pending_entry: Arc<Mutex<Option<Option<String>>>>,
    ) -> Self {
        load_fonts(&cc.egui_ctx);

        // Accessory policy: no Dock icon, no Cmd-Tab entry — pure background utility.
        unsafe {
            use objc2::MainThreadMarker;
            let mtm = MainThreadMarker::new_unchecked();
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }

        // ── Clipboard history ──
        let history = Arc::new(Mutex::new(load_history()));
        start_poller(Arc::clone(&history));

        // ── Build initial item list ──
        let mut all_items: Vec<LaunchItem> = discover_apps()
            .into_iter()
            .map(LaunchItem::App)
            .collect();

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
            toggle,
            visible: false,
            prev_app: None,
            pending_entry,
        };

        app.refilter(true);
        app
    }

    /// Rebuild the Clip items from the shared history, keeping App and Pass items.
    fn sync_clipboard(&mut self) {
        let current_clips: Vec<ClipboardEntry> = {
            let lock = self.clip_history.lock().unwrap();
            lock.clone()
        };
        self.items
            .retain(|i| matches!(i, LaunchItem::App(_) | LaunchItem::Pass(_)));
        for entry in current_clips {
            self.items.push(LaunchItem::Clip(entry));
        }
    }

    /// Refresh the App items from the filesystem (picks up newly installed apps).
    fn sync_apps(&mut self) {
        let new_apps: Vec<LaunchItem> = discover_apps()
            .into_iter()
            .map(LaunchItem::App)
            .collect();
        self.items.retain(|i| !matches!(i, LaunchItem::App(_)));
        self.items.extend(new_apps);
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
                Mode::About => false,
            })
            .collect();

        let search_items: Vec<LaunchItem> = mode_items.iter().map(|(_, i)| (*i).clone()).collect();
        let matched_local: Vec<usize> = self.launcher.search(&self.query, &search_items);

        self.filtered = matched_local
            .into_iter()
            .map(|local_idx| mode_items[local_idx].0)
            .collect();

        if reset_selection {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
        }
    }

    fn execute_selected(&mut self) {
        if let Some(&idx) = self.filtered.get(self.selected) {
            match &self.items[idx] {
                LaunchItem::App(app) => {
                    launch_app(&app.path.clone());
                    self.should_close = true;
                }
                LaunchItem::Clip(entry) => {
                    let text = entry.text.clone();
                    paste_text(&text);
                    self.should_close = true;
                }
                LaunchItem::Pass(entry) => {
                    let name = entry.name.clone();
                    *self.pending_entry.lock().unwrap() = Some(Some(name));
                    self.should_close = true;
                }
            }
        }
    }

    fn restore_focus(&mut self) {
        if let Some(app) = self.prev_app.take() {
            restore_app_focus(&app);
        }
    }

    fn hide(&mut self, ctx: &egui::Context) {
        self.visible = false;
        self.should_close = false;
        self.query.clear();
        self.mode = Mode::Apps;
        self.frame_count = 0;
        self.toast = None;
        self.refilter(true);
        // If dismissed without selecting a pass entry, unblock the waiting client.
        let mut lock = self.pending_entry.lock().unwrap();
        if lock.is_none() {
            *lock = Some(None);
        }
        drop(lock);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.restore_focus();
    }
}

// ── eframe::App impl ─────────────────────────────────────────────────────────

impl eframe::App for RofiApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── SIGUSR1 toggle ────────────────────────────────────────────────────
        if self.toggle.swap(false, Ordering::Relaxed) {
            self.visible = !self.visible;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(self.visible));
            if self.visible {
                // Capture the frontmost app before we steal focus.
                self.prev_app = capture_previous_app();
                // Reset state every time the window is shown.
                self.query.clear();
                self.mode = Mode::Apps;
                self.frame_count = 0;
                self.toast = None;
                // Refresh app list on every show.
                self.sync_apps();
                self.refilter(true);
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else {
                self.restore_focus();
            }
        }

        // Keep polling for the signal even when hidden.
        ctx.request_repaint_after(std::time::Duration::from_millis(50));

        if !self.visible {
            return;
        }

        if self.should_close {
            self.hide(ctx);
            return;
        }

        self.frame_count = self.frame_count.saturating_add(1);

        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.hide(ctx);
            return;
        }

        // Cmd+R toggles the launcher — pressing it again hides.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::R)) {
            self.hide(ctx);
            return;
        }

        // Focus loss: hide and restore previous app.
        if self.frame_count > 5 && !ctx.input(|i| i.focused) {
            self.hide(ctx);
            return;
        }

        // Sync clipboard live while in clipboard mode.
        if self.mode == Mode::Clipboard {
            self.sync_clipboard();
            self.refilter(false);
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        let mut style = (*ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        ctx.set_style(style);

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let panel_rect = ui.max_rect();

                // Window background: sumiInk0 with slight transparency
                ui.painter().rect_filled(
                    panel_rect,
                    Rounding::ZERO,
                    Color32::from_rgba_unmultiplied(0x16, 0x16, 0x1D, 210),
                );
                // Border: sumiInk4
                ui.painter().rect_stroke(
                    panel_rect,
                    Rounding::ZERO,
                    Stroke::new(1.5, kana::SUMI_INK4),
                );

                let inner = panel_rect.shrink2(Vec2::new(16.0, 14.0));
                ui.allocate_ui_at_rect(inner, |ui| {
                    ui.vertical(|ui| {
                        // ── Mode tabs ──────────────────────────────────────
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for (mode, label) in [
                                (Mode::Apps, "Apps"),
                                (Mode::Clipboard, "Clipboard"),
                                (Mode::Pass, "Pass"),
                                (Mode::About, "About"),
                            ] {
                                let selected = self.mode == mode;
                                let btn = egui::Button::new(
                                    egui::RichText::new(label)
                                        .font(FontId::new(10.0, FontFamily::Monospace))
                                        .color(if selected {
                                            kana::CRYSTAL_BLUE
                                        } else {
                                            kana::FUJI_GRAY
                                        }),
                                )
                                .fill(if selected {
                                    kana::WAVE_BLUE1
                                } else {
                                    Color32::TRANSPARENT
                                })
                                .stroke(if selected {
                                    Stroke::new(1.0, kana::SUMI_INK4)
                                } else {
                                    Stroke::NONE
                                })
                                .rounding(Rounding::ZERO);

                                if ui.add(btn).clicked() {
                                    self.mode = mode;
                                    self.query.clear();
                                    if mode == Mode::Clipboard {
                                        self.sync_clipboard();
                                    }
                                    self.refilter(true);
                                }
                            }

                            // "Mofi" label pushed to the right.
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(
                                    egui::RichText::new("Mofi")
                                        .font(FontId::new(11.0, FontFamily::Name("medium".into())))
                                        .color(kana::SUMI_INK4),
                                );
                            });
                        });

                        ui.add_space(10.0);

                        // ── Search bar ────────────────────────────────────
                        // Consume C-j / C-k before TextEdit sees them.
                        let ctrl_j = ctx.input_mut(|i| {
                            i.consume_key(egui::Modifiers::CTRL, Key::J)
                        });
                        let ctrl_k = ctx.input_mut(|i| {
                            i.consume_key(egui::Modifiers::CTRL, Key::K)
                        });

                        let hint = match self.mode {
                            Mode::Apps => "Search apps…",
                            Mode::Clipboard => "Filter clipboard…",
                            Mode::Pass => "Search passwords…",
                            Mode::About => "",
                        };

                        let response = ui.add(
                            egui::TextEdit::singleline(&mut self.query)
                                .hint_text(egui::RichText::new(hint).color(kana::FUJI_GRAY))
                                .font(FontId::new(15.0, FontFamily::Monospace))
                                .text_color(kana::FUJI_WHITE)
                                .frame(false)
                                .desired_width(f32::INFINITY),
                        );
                        response.request_focus();

                        // Keyboard navigation
                        let down = ctx.input(|i| i.key_pressed(Key::ArrowDown)) || ctrl_j;
                        let up = ctx.input(|i| i.key_pressed(Key::ArrowUp)) || ctrl_k;
                        let tab = ctx.input(|i| i.key_pressed(Key::Tab));
                        let enter = ctx.input(|i| i.key_pressed(Key::Enter));

                        // Tab cycles through mode tabs.
                        if tab {
                            self.mode = match self.mode {
                                Mode::Apps => Mode::Clipboard,
                                Mode::Clipboard => Mode::Pass,
                                Mode::Pass => Mode::About,
                                Mode::About => Mode::Apps,
                            };
                            self.query.clear();
                            if self.mode == Mode::Clipboard {
                                self.sync_clipboard();
                            }
                            self.refilter(true);
                        }
                        if down && !self.filtered.is_empty() {
                            self.selected = (self.selected + 1) % self.filtered.len();
                        }
                        if up && !self.filtered.is_empty() {
                            self.selected = self
                                .selected
                                .checked_sub(1)
                                .unwrap_or(self.filtered.len() - 1);
                        }
                        if enter {
                            self.execute_selected();
                            return;
                        }
                        if response.changed() {
                            self.refilter(true);
                        }

                        ui.add_space(6.0);

                        // Separator
                        let sep_y = ui.cursor().top();
                        ui.painter().hline(
                            ui.max_rect().x_range(),
                            sep_y,
                            Stroke::new(1.0, kana::SUMI_INK3),
                        );
                        ui.add_space(6.0);

                        // ── Results list / About panel ────────────────────
                        let max_list_height = ROW_HEIGHT * MAX_VISIBLE_ROWS as f32;

                        if self.mode == Mode::About {
                            ui.add_space(18.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("Mofi")
                                        .font(FontId::new(22.0, FontFamily::Name("medium".into())))
                                        .color(kana::CRYSTAL_BLUE),
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new("v0.1.0")
                                        .font(FontId::new(10.0, FontFamily::Monospace))
                                        .color(kana::FUJI_GRAY),
                                );
                                ui.add_space(14.0);
                                ui.label(
                                    egui::RichText::new("App launcher · Clipboard · Pass")
                                        .font(FontId::new(11.0, FontFamily::Monospace))
                                        .color(kana::OLD_WHITE),
                                );
                                ui.add_space(14.0);
                                ui.label(
                                    egui::RichText::new("\u{F09B}  github.com/bechampion/mofi") // nf-fa-github
                                        .font(FontId::new(11.0, FontFamily::Monospace))
                                        .color(kana::SPRING_VIOLET1),
                                );
                                ui.add_space(18.0);
                                ui.label(
                                    egui::RichText::new("Cmd+Space / Cmd+R  open · Esc  close · Tab  cycle tabs")
                                        .font(FontId::new(9.0, FontFamily::Monospace))
                                        .color(kana::FUJI_GRAY),
                                );
                            });
                        } else {

                        egui::ScrollArea::vertical()
                            .max_height(max_list_height)
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());

                                if self.filtered.is_empty() {
                                    ui.add_space(20.0);
                                    ui.centered_and_justified(|ui| {
                                        ui.label(
                                            egui::RichText::new("No results")
                                                .font(FontId::new(11.0, FontFamily::Monospace))
                                                .color(kana::FUJI_GRAY),
                                        );
                                    });
                                    return;
                                }

                                let available_width = ui.available_width();

                                for (row_idx, &item_idx) in self.filtered.iter().enumerate() {
                                    let is_selected = row_idx == self.selected;
                                    let item = &self.items[item_idx];

                                    let glyph = glyph_for_item(item);
                                    let glyph_color = if is_selected {
                                        glyph_color_for_item(item)
                                    } else {
                                        let c = glyph_color_for_item(item);
                                        Color32::from_rgba_unmultiplied(
                                            (c.r() as u16 * 2 / 3) as u8,
                                            (c.g() as u16 * 2 / 3) as u8,
                                            (c.b() as u16 * 2 / 3) as u8,
                                            255,
                                        )
                                    };

                                    let display = item.display_name();
                                    let subtitle = item.subtitle();

                                    let (row_rect, _) = ui.allocate_exact_size(
                                        Vec2::new(available_width, ROW_HEIGHT),
                                        egui::Sense::hover(),
                                    );

                                    if is_selected {
                                        ui.scroll_to_rect(row_rect, None);
                                    }

                                    // Row background
                                    if is_selected {
                                        ui.painter().rect_filled(
                                            row_rect,
                                            Rounding::ZERO,
                                            kana::WAVE_BLUE2,
                                        );
                                        let bar = egui::Rect::from_min_size(
                                            egui::pos2(
                                                row_rect.left(),
                                                row_rect.top() + 4.0,
                                            ),
                                            Vec2::new(3.0, row_rect.height() - 8.0),
                                        );
                                        ui.painter().rect_filled(
                                            bar,
                                            Rounding::ZERO,
                                            kana::CRYSTAL_BLUE,
                                        );
                                    } else if ui.rect_contains_pointer(row_rect) {
                                        ui.painter().rect_filled(
                                            row_rect,
                                            Rounding::ZERO,
                                            kana::SUMI_INK2,
                                        );
                                    }

                                    // ── Glyph icon ──
                                    let icon_x = row_rect.left() + 14.0;
                                    let icon_center = egui::pos2(
                                        icon_x + ICON_SIZE / 2.0,
                                        row_rect.center().y,
                                    );
                                    ui.painter().text(
                                        icon_center,
                                        egui::Align2::CENTER_CENTER,
                                        glyph,
                                        FontId::new(ICON_SIZE * 0.75, FontFamily::Monospace),
                                        glyph_color,
                                    );

                                    // ── Text ──
                                    let text_x = icon_x + ICON_SIZE + 12.0;

                                    if let Some(sub) = subtitle {
                                        let name_y = row_rect.center().y - 7.0;
                                        let sub_y = row_rect.center().y + 7.0;
                                        ui.painter().text(
                                            egui::pos2(text_x, name_y),
                                            egui::Align2::LEFT_CENTER,
                                            &display,
                                            FontId::new(12.0, FontFamily::Name("medium".into())),
                                            if is_selected { kana::FUJI_WHITE } else { kana::OLD_WHITE },
                                        );
                                        ui.painter().text(
                                            egui::pos2(text_x, sub_y),
                                            egui::Align2::LEFT_CENTER,
                                            &sub,
                                            FontId::new(9.0, FontFamily::Monospace),
                                            if is_selected { kana::SPRING_VIOLET1 } else { kana::FUJI_GRAY },
                                        );
                                    } else {
                                        ui.painter().text(
                                            egui::pos2(text_x, row_rect.center().y),
                                            egui::Align2::LEFT_CENTER,
                                            &display,
                                            FontId::new(12.0, FontFamily::Name("medium".into())),
                                            if is_selected { kana::FUJI_WHITE } else { kana::OLD_WHITE },
                                        );
                                    }

                                    // ── Click interaction ──
                                    let click = ui.interact(
                                        row_rect,
                                        egui::Id::new(("row", row_idx)),
                                        egui::Sense::click(),
                                    );
                                    if click.hovered() {
                                        self.selected = row_idx;
                                    }
                                    if click.double_clicked() {
                                        self.selected = row_idx;
                                        self.execute_selected();
                                        return;
                                    }
                                }
                            });

                        } // end else (not About mode)

                        // ── Toast ─────────────────────────────────────────
                        if let Some((msg, since)) = &self.toast {
                            let elapsed = since.elapsed().as_secs_f32();
                            if elapsed < 1.5 {
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(msg.as_str())
                                            .font(FontId::new(11.0, FontFamily::Monospace))
                                            .color(kana::SPRING_GREEN),
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
