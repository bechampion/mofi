/// mofisw — Option+Tab window switcher for macOS.
///
/// Hold Option and press Tab to cycle through windows.
/// Release Option to activate the selected window and dismiss.
///
/// Requires Accessibility permission (System Settings → Privacy → Accessibility).
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy, NSWorkspace,
};

// ── Raw CoreFoundation / CoreGraphics FFI ─────────────────────────────────────

#[allow(non_upper_case_globals, non_snake_case, dead_code)]
mod ffi {
    use std::os::raw::{c_int, c_void};

    // ── basic CF types ────────────────────────────────────────────────────────
    pub type CFTypeRef        = *const c_void;
    pub type CFArrayRef       = *const c_void;
    pub type CFDictionaryRef  = *const c_void;
    pub type CFStringRef      = *const c_void;
    pub type CFNumberRef      = *const c_void;
    pub type CFAllocatorRef   = *const c_void;
    pub type CFIndex          = isize;
    pub type Boolean          = u8;
    pub type CFNumberType     = u32;
    pub type CFRunLoopRef     = *mut c_void;
    pub type CFRunLoopSourceRef = *mut c_void;

    // ── CGEvent types ─────────────────────────────────────────────────────────
    pub type CGEventRef       = *mut c_void;
    pub type CGEventTapRef    = *mut c_void;     // = MachPortRef under the hood
    pub type CGEventMask      = u64;
    pub type CGEventType      = u32;
    pub type CGKeyCode        = u16;
    pub type CGEventField     = u32;
    pub type CGEventFlags     = u64;
    pub type CGEventTapLocation  = u32;
    pub type CGEventTapPlacement = u32;
    pub type CGEventTapOptions   = u32;

    pub const kCGEventTapOptionDefault:    CGEventTapOptions   = 0;
    pub const kCGEventTapOptionListenOnly: CGEventTapOptions   = 1;
    pub const kCGHIDEventTap:              CGEventTapLocation  = 0;
    pub const kCGHeadInsertEventTap:       CGEventTapPlacement = 0;
    pub const kCGEventKeyDown:             CGEventType = 10;
    pub const kCGEventKeyUp:               CGEventType = 11;
    pub const kCGEventFlagsChanged:        CGEventType = 12;
    pub const kCGKeyboardEventKeycode:     CGEventField = 9;
    pub const kCGEventFlagMaskAlternate:   CGEventFlags = 0x00080000;

    pub const kCFNumberSInt32Type: CFNumberType = 3;

    // kCFStringEncodingUTF8
    pub const kCFStringEncodingUTF8: u32 = 0x08000100;

    // CGWindowList options
    pub type CGWindowListOption = u32;
    pub type CGWindowID         = u32;
    pub const kCGWindowListOptionOnScreenOnly:       CGWindowListOption = 1 << 0;
    pub const kCGWindowListExcludeDesktopElements:   CGWindowListOption = 1 << 4;
    pub const kCGNullWindowID:                       CGWindowID         = 0;

    pub type CGEventTapCallBack = unsafe extern "C" fn(
        proxy: *mut c_void,
        event_type: CGEventType,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGWindowListCopyWindowInfo(
            option: CGWindowListOption,
            relativeToWindow: CGWindowID,
        ) -> CFArrayRef;

        pub fn CGEventTapCreate(
            tap: CGEventTapLocation,
            place: CGEventTapPlacement,
            options: CGEventTapOptions,
            eventsOfInterest: CGEventMask,
            callback: CGEventTapCallBack,
            userInfo: *mut c_void,
        ) -> CGEventTapRef;

        pub fn CGEventGetIntegerValueField(
            event: CGEventRef,
            field: CGEventField,
        ) -> i64;

        pub fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;

        pub fn CGEventTapEnable(tap: CGEventTapRef, enable: Boolean);

        pub fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> Boolean;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFArrayGetCount(theArray: CFArrayRef) -> CFIndex;
        pub fn CFArrayGetValueAtIndex(theArray: CFArrayRef, idx: CFIndex) -> CFTypeRef;
        pub fn CFDictionaryGetValue(theDict: CFDictionaryRef, key: CFStringRef) -> CFTypeRef;
        pub fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            cStr: *const c_int,
            encoding: u32,
        ) -> CFStringRef;
        pub fn CFStringGetCString(
            theString: CFStringRef,
            buffer: *mut u8,
            bufferSize: CFIndex,
            encoding: u32,
        ) -> Boolean;
        pub fn CFNumberGetValue(
            number: CFNumberRef,
            theType: CFNumberType,
            valuePtr: *mut c_void,
        ) -> Boolean;
        pub fn CFRelease(cf: CFTypeRef);
        pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
        pub fn CFStringGetTypeID() -> usize;
        pub fn CFNumberGetTypeID() -> usize;
        pub fn CFDictionaryGetTypeID() -> usize;

        pub fn CFMachPortCreateRunLoopSource(
            allocator: CFAllocatorRef,
            port: CGEventTapRef,
            order: CFIndex,
        ) -> CFRunLoopSourceRef;
        pub fn CFRunLoopAddSource(
            rl: CFRunLoopRef,
            source: CFRunLoopSourceRef,
            mode: CFStringRef,
        );
        pub fn CFRunLoopRun();
        pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;

        pub static kCFRunLoopCommonModes: CFStringRef;
    }
}

// ── Key messages ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum KeyMsg {
    /// Option+Tab — show overlay or advance selection
    TabPressed,
    /// Option key released — commit and dismiss
    OptionReleased,
}

// ── CGEventTap callback context ───────────────────────────────────────────────

struct TapContext {
    tx: std::sync::mpsc::Sender<KeyMsg>,
    option_down: bool,
}

unsafe extern "C" fn event_tap_callback(
    _proxy: *mut std::os::raw::c_void,
    event_type: ffi::CGEventType,
    event: ffi::CGEventRef,
    user_info: *mut std::os::raw::c_void,
) -> ffi::CGEventRef {
    let ctx = &mut *(user_info as *mut TapContext);

    match event_type {
        ffi::kCGEventFlagsChanged => {
            let flags = unsafe { ffi::CGEventGetFlags(event) };
            let alt_now = (flags & ffi::kCGEventFlagMaskAlternate) != 0;
            if !alt_now && ctx.option_down {
                ctx.option_down = false;
                let _ = ctx.tx.send(KeyMsg::OptionReleased);
            } else if alt_now && !ctx.option_down {
                ctx.option_down = true;
            }
        }
        ffi::kCGEventKeyDown => {
            let keycode = unsafe {
                ffi::CGEventGetIntegerValueField(event, ffi::kCGKeyboardEventKeycode)
            };
            // 48 = kVK_Tab
            if keycode == 48 && ctx.option_down {
                let _ = ctx.tx.send(KeyMsg::TabPressed);
            }
        }
        _ => {}
    }

    event
}

// ── Accessibility check ───────────────────────────────────────────────────────

fn check_accessibility() -> bool {
    // AXIsProcessTrustedWithOptions with prompt = true
    // We pass NULL to skip the prompt (we'll print our own message).
    let trusted = unsafe { ffi::AXIsProcessTrustedWithOptions(std::ptr::null()) };
    trusted != 0
}

// ── Start key listener (own thread + CFRunLoop) ───────────────────────────────

fn start_key_listener(tx: std::sync::mpsc::Sender<KeyMsg>) {
    let ctx = Box::new(TapContext { tx, option_down: false });
    // Store as usize so the closure is Send (raw pointers are not Send).
    let ctx_addr: usize = Box::into_raw(ctx) as usize;

    std::thread::spawn(move || {
        let ctx_raw = ctx_addr as *mut std::os::raw::c_void;
        // Events we care about: key down + key up + flags changed
        let mask: ffi::CGEventMask = (1 << ffi::kCGEventKeyDown)
            | (1 << ffi::kCGEventKeyUp)
            | (1 << ffi::kCGEventFlagsChanged);

        let tap = unsafe {
            ffi::CGEventTapCreate(
                ffi::kCGHIDEventTap,
                ffi::kCGHeadInsertEventTap,
                ffi::kCGEventTapOptionListenOnly,
                mask,
                event_tap_callback,
                ctx_raw,
            )
        };

        if tap.is_null() {
            eprintln!(
                "mofisw: failed to create CGEventTap.\n\
                 Grant Accessibility access in:\n\
                 System Settings → Privacy & Security → Accessibility\n\
                 Then re-run mofisw."
            );
            std::process::exit(1);
        }

        let source = unsafe {
            ffi::CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0)
        };
        let rl = unsafe { ffi::CFRunLoopGetCurrent() };
        unsafe {
            ffi::CFRunLoopAddSource(rl, source, ffi::kCFRunLoopCommonModes);
            ffi::CGEventTapEnable(tap, 1);
            ffi::CFRunLoopRun(); // blocks this thread forever
        }
    });
}

// ── CGWindowList ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct WinEntry {
    pid:       i32,
    app_name:  String,
    win_title: String,
}

unsafe fn cf_string_to_rust(s: ffi::CFStringRef) -> Option<String> {
    if s.is_null() { return None; }
    let mut buf = [0u8; 512];
    let ok = unsafe {
        ffi::CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as ffi::CFIndex, ffi::kCFStringEncodingUTF8)
    };
    if ok == 0 { return None; }
    std::ffi::CStr::from_bytes_until_nul(&buf).ok().map(|c| c.to_string_lossy().into_owned())
}

unsafe fn dict_string(dict: ffi::CFDictionaryRef, key: &str) -> Option<String> {
    let k = std::ffi::CString::new(key).ok()?;
    let cf_key = unsafe {
        ffi::CFStringCreateWithCString(std::ptr::null(), k.as_ptr() as *const _, ffi::kCFStringEncodingUTF8)
    };
    if cf_key.is_null() { return None; }
    let val = unsafe { ffi::CFDictionaryGetValue(dict, cf_key) };
    unsafe { ffi::CFRelease(cf_key) };
    if val.is_null() { return None; }
    if unsafe { ffi::CFGetTypeID(val) } != unsafe { ffi::CFStringGetTypeID() } { return None; }
    unsafe { cf_string_to_rust(val as ffi::CFStringRef) }
}

unsafe fn dict_i32(dict: ffi::CFDictionaryRef, key: &str) -> Option<i32> {
    let k = std::ffi::CString::new(key).ok()?;
    let cf_key = unsafe {
        ffi::CFStringCreateWithCString(std::ptr::null(), k.as_ptr() as *const _, ffi::kCFStringEncodingUTF8)
    };
    if cf_key.is_null() { return None; }
    let val = unsafe { ffi::CFDictionaryGetValue(dict, cf_key) };
    unsafe { ffi::CFRelease(cf_key) };
    if val.is_null() { return None; }
    if unsafe { ffi::CFGetTypeID(val) } != unsafe { ffi::CFNumberGetTypeID() } { return None; }
    let mut out: i32 = 0;
    let ok = unsafe { ffi::CFNumberGetValue(val as ffi::CFNumberRef, ffi::kCFNumberSInt32Type, &mut out as *mut _ as *mut _) };
    if ok != 0 { Some(out) } else { None }
}

fn list_windows() -> Vec<WinEntry> {
    let opts = ffi::kCGWindowListOptionOnScreenOnly | ffi::kCGWindowListExcludeDesktopElements;
    let array = unsafe { ffi::CGWindowListCopyWindowInfo(opts, ffi::kCGNullWindowID) };
    if array.is_null() { return vec![]; }

    let count   = unsafe { ffi::CFArrayGetCount(array) };
    let our_pid = std::process::id() as i32;
    let mut out: Vec<WinEntry> = Vec::new();

    for i in 0..count {
        let item = unsafe { ffi::CFArrayGetValueAtIndex(array, i) };
        if item.is_null() { continue; }
        if unsafe { ffi::CFGetTypeID(item) } != unsafe { ffi::CFDictionaryGetTypeID() } { continue; }
        let dict = item as ffi::CFDictionaryRef;

        let layer = unsafe { dict_i32(dict, "kCGWindowLayer") }.unwrap_or(999);
        if layer != 0 { continue; }

        let pid = unsafe { dict_i32(dict, "kCGWindowOwnerPID") }.unwrap_or(0);
        if pid == 0 || pid == our_pid { continue; }

        let app_name = unsafe { dict_string(dict, "kCGWindowOwnerName") }.unwrap_or_default();
        if app_name.is_empty() { continue; }

        let win_title = unsafe { dict_string(dict, "kCGWindowName") }.unwrap_or_default();

        out.push(WinEntry { pid, app_name, win_title });
    }
    unsafe { ffi::CFRelease(array) };

    // Remove redundant titles (single-window app where title == app name).
    let count_per_pid: HashMap<i32, usize> = {
        let mut m: HashMap<i32, usize> = HashMap::new();
        for e in &out { *m.entry(e.pid).or_insert(0) += 1; }
        m
    };
    for e in out.iter_mut() {
        if count_per_pid.get(&e.pid).copied().unwrap_or(0) == 1 && e.win_title == e.app_name {
            e.win_title.clear();
        }
    }
    out
}

// ── App glyph ─────────────────────────────────────────────────────────────────

fn glyph_for(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("safari")                                            { "\u{E748}" }
    else if n.contains("firefox")                                      { "\u{E745}" }
    else if n.contains("chrome") || n.contains("chromium")             { "\u{E743}" }
    else if n.contains("terminal") || n.contains("iterm")
         || n.contains("alacritty") || n.contains("warp")
         || n.contains("kitty") || n.contains("ghostty")               { "\u{EA85}" }
    else if n.contains("cursor") || n.contains("code") || n.contains("vscode") { "\u{E8DA}" }
    else if n.contains("xcode")                                        { "\u{E8E8}" }
    else if n.contains("sublime")                                      { "\u{E7AA}" }
    else if n.contains("vim") || n.contains("neovim")                  { "\u{E6AC}" }
    else if n.contains("slack")                                        { "\u{F198}" }
    else if n.contains("discord")                                      { "\u{F099}" }
    else if n.contains("telegram")                                     { "\u{F2C6}" }
    else if n.contains("whatsapp")                                     { "\u{F232}" }
    else if n.contains("mail") || n.contains("outlook")                { "\u{F0E0}" }
    else if n.contains("calendar")                                     { "\u{F073}" }
    else if n.contains("notes")                                        { "\u{F249}" }
    else if n.contains("finder")                                       { "\u{F07C}" }
    else if n.contains("spotify")                                      { "\u{F1BC}" }
    else if n.contains("music") || n.contains("itunes")                { "\u{F001}" }
    else if n.contains("system preferences") || n.contains("system settings") { "\u{F013}" }
    else if n.contains("activity monitor")                             { "\u{F080}" }
    else if n.contains("preview")                                      { "\u{F1C5}" }
    else if n.contains("figma")                                        { "\u{E6AD}" }
    else if n.contains("notion")                                       { "\u{F46D}" }
    else if n.contains("obsidian")                                     { "\u{F040}" }
    else if n.contains("1password") || n.contains("bitwarden")         { "\u{F023}" }
    else if n.contains("docker")                                       { "\u{F308}" }
    else if n.contains("postman")                                      { "\u{F441}" }
    else if n.contains("tableplus") || n.contains("sequel")            { "\u{F1C0}" }
    else                                                               { "\u{F108}" }
}

// ── Fonts ─────────────────────────────────────────────────────────────────────

fn load_fonts(ctx: &egui::Context) {
    let font_path = dirs::home_dir().unwrap().join("Library/Fonts/MapleMono-NF-Regular.ttf");
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(bytes) = std::fs::read(&font_path) {
        fonts.font_data.insert("maple".into(), egui::FontData::from_owned(bytes));
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(fam).or_default().insert(0, "maple".into());
        }
    }
    ctx.set_fonts(fonts);
}

// ── Colours ───────────────────────────────────────────────────────────────────

struct Colors {
    bg:       egui::Color32,
    border:   egui::Color32,
    card_bg:  egui::Color32,
    card_sel: egui::Color32,
    fg:       egui::Color32,
    fg_dim:   egui::Color32,
    accent:   egui::Color32,
}

impl Colors {
    fn kanagawa() -> Self {
        Self {
            bg:       egui::Color32::from_rgba_premultiplied(22, 22, 34, 230),
            border:   egui::Color32::from_rgb(84, 84, 109),
            card_bg:  egui::Color32::from_rgba_premultiplied(31, 31, 48, 240),
            card_sel: egui::Color32::from_rgba_premultiplied(42, 89, 137, 255),
            fg:       egui::Color32::from_rgb(220, 215, 186),
            fg_dim:   egui::Color32::from_rgb(114, 113, 105),
            accent:   egui::Color32::from_rgb(126, 156, 216),
        }
    }
}

// ── Layout ────────────────────────────────────────────────────────────────────

const CARD_W:   f32 = 110.0;
const CARD_H:   f32 = 90.0;
const CARD_PAD: f32 = 10.0;
const WIN_PAD:  f32 = 16.0;
const HINT_H:   f32 = 20.0;

// ── Switcher app ──────────────────────────────────────────────────────────────

struct SwitcherApp {
    visible:  Arc<AtomicBool>,
    selected: Arc<AtomicUsize>,
    windows:  Arc<Mutex<Vec<WinEntry>>>,
    msg_rx:   std::sync::mpsc::Receiver<KeyMsg>,
    colors:   Colors,
}

impl eframe::App for SwitcherApp {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut do_show = false;
        let mut do_hide = false;

        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                KeyMsg::TabPressed    => do_show = true,
                KeyMsg::OptionReleased => do_hide = true,
            }
        }

        if do_show {
            if !self.visible.load(Ordering::Relaxed) {
                *self.windows.lock().unwrap() = list_windows();
                self.selected.store(0, Ordering::Relaxed);
                self.visible.store(true, Ordering::Relaxed);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else {
                let len = self.windows.lock().unwrap().len();
                if len > 0 {
                    let cur = self.selected.load(Ordering::Relaxed);
                    self.selected.store((cur + 1) % len, Ordering::Relaxed);
                }
            }
        }

        if do_hide && self.visible.load(Ordering::Relaxed) {
            self.visible.store(false, Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            let wins = self.windows.lock().unwrap().clone();
            let sel  = self.selected.load(Ordering::Relaxed);
            if let Some(e) = wins.get(sel) {
                activate_pid(e.pid);
            }
        }

        ctx.request_repaint_after(if self.visible.load(Ordering::Relaxed) {
            std::time::Duration::from_millis(16)
        } else {
            std::time::Duration::from_millis(50)
        });

        if !self.visible.load(Ordering::Relaxed) { return; }

        let wins = self.windows.lock().unwrap().clone();
        let sel  = self.selected.load(Ordering::Relaxed);
        let c    = &self.colors;

        let n     = wins.len().max(1) as f32;
        let win_w = (n * (CARD_W + CARD_PAD) - CARD_PAD + WIN_PAD * 2.0).min(1200.0);
        let win_h = CARD_H + WIN_PAD * 2.0 + HINT_H;

        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(win_w, win_h)));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(c.bg)
                    .stroke(egui::Stroke::new(1.0, c.border))
                    .inner_margin(egui::Margin::same(WIN_PAD)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(CARD_PAD, 0.0);
                    for (i, entry) in wins.iter().enumerate() {
                        let is_sel = i == sel;
                        let (r, _) = ui.allocate_exact_size(
                            egui::vec2(CARD_W, CARD_H),
                            egui::Sense::hover(),
                        );
                        ui.painter().rect(
                            r,
                            egui::Rounding::ZERO,
                            if is_sel { c.card_sel } else { c.card_bg },
                            egui::Stroke::new(
                                if is_sel { 2.0 } else { 1.0 },
                                if is_sel { c.accent } else { c.border },
                            ),
                        );
                        // Glyph
                        ui.painter().text(
                            egui::pos2(r.center().x, r.top() + 30.0),
                            egui::Align2::CENTER_CENTER,
                            glyph_for(&entry.app_name),
                            egui::FontId::proportional(28.0),
                            if is_sel { c.accent } else { c.fg },
                        );
                        // App name
                        ui.painter().text(
                            egui::pos2(r.center().x, r.top() + 57.0),
                            egui::Align2::CENTER_CENTER,
                            truncate(&entry.app_name, 13),
                            egui::FontId::proportional(11.0),
                            c.fg,
                        );
                        // Window title
                        if !entry.win_title.is_empty() {
                            ui.painter().text(
                                egui::pos2(r.center().x, r.top() + 71.0),
                                egui::Align2::CENTER_CENTER,
                                truncate(&entry.win_title, 14),
                                egui::FontId::proportional(9.0),
                                c.fg_dim,
                            );
                        }
                    }
                });
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("release ⌥ to switch   ⌥Tab to cycle")
                        .color(c.fg_dim)
                        .size(9.0),
                );
            });
    }
}

fn truncate(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n { s.to_string() }
    else { chars[..n - 1].iter().collect::<String>() + "…" }
}

// ── Activate window by PID ────────────────────────────────────────────────────

fn activate_pid(pid: i32) {
    let apps = NSWorkspace::sharedWorkspace().runningApplications();
    for app in apps.iter() {
        if app.processIdentifier() == pid {
            app.activateWithOptions(NSApplicationActivationOptions(1 << 0));
            break;
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    // Check accessibility permission before attempting to create a CGEventTap.
    if !check_accessibility() {
        eprintln!(
            "mofisw: Accessibility permission not granted.\n\
             Open System Settings → Privacy & Security → Accessibility\n\
             and add mofisw, then re-run."
        );
        std::process::exit(1);
    }

    let (tx, rx) = std::sync::mpsc::channel::<KeyMsg>();
    start_key_listener(tx);

    let visible  = Arc::new(AtomicBool::new(false));
    let selected = Arc::new(AtomicUsize::new(0));
    let windows: Arc<Mutex<Vec<WinEntry>>> = Arc::new(Mutex::new(Vec::new()));

    let win_h = CARD_H + WIN_PAD * 2.0 + HINT_H;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, win_h])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(false)
            .with_active(false)
            .with_visible(false),
        centered: true,
        vsync: true,
        ..Default::default()
    };

    eframe::run_native(
        "mofisw",
        options,
        Box::new(move |cc| {
            load_fonts(&cc.egui_ctx);
            unsafe {
                use objc2::MainThreadMarker;
                let mtm = MainThreadMarker::new_unchecked();
                let app = NSApplication::sharedApplication(mtm);
                app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            }
            Box::new(SwitcherApp { visible, selected, windows, msg_rx: rx, colors: Colors::kanagawa() })
        }),
    )
}
