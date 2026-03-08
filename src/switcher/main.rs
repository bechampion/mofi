/// mofisw — Option+Tab window switcher for macOS.
///
/// Hold Option and press Tab to cycle through windows.
/// Release Option to activate the selected window and dismiss.
///
/// On the first Option+Tab press the overlay appears with the previously
/// focused window pre-selected.  Release Option immediately to quick-swap
/// back to it, or keep pressing Tab to cycle through all windows.
///
/// Requires Accessibility permission (System Settings → Privacy → Accessibility).
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
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
    pub const kCFNumberSInt64Type: CFNumberType = 4;

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

    // ── Accessibility (AX) API ────────────────────────────────────────────────
    pub type AXUIElementRef = *mut c_void;
    pub type AXError        = i32;
    pub const kAXErrorSuccess: AXError = 0;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        pub fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        pub fn AXUIElementPerformAction(
            element: AXUIElementRef,
            action: CFStringRef,
        ) -> AXError;
        pub fn AXUIElementGetPid(
            element: AXUIElementRef,
            pid: *mut i32,
        ) -> AXError;
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
    /// Option+Tab pressed — swallow and notify
    TabPressed,
    /// Option key released — commit and dismiss
    OptionReleased,
}

// ── CGEventTap callback context ───────────────────────────────────────────────

struct TapContext {
    tx:          std::sync::mpsc::Sender<KeyMsg>,
    option_down: bool,
    /// Shared with the UI thread so it can read live Option key state.
    option_down_shared: Arc<AtomicBool>,
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
                ctx.option_down_shared.store(false, Ordering::Relaxed);
                let _ = ctx.tx.send(KeyMsg::OptionReleased);
            } else if alt_now && !ctx.option_down {
                ctx.option_down = true;
                ctx.option_down_shared.store(true, Ordering::Relaxed);
            }
        }
        ffi::kCGEventKeyDown => {
            let keycode = unsafe {
                ffi::CGEventGetIntegerValueField(event, ffi::kCGKeyboardEventKeycode)
            };
            // 48 = kVK_Tab
            if keycode == 48 && ctx.option_down {
                let _ = ctx.tx.send(KeyMsg::TabPressed);
                // Swallow the event — return null so it never reaches the
                // focused application.
                return std::ptr::null_mut();
            }
        }
        _ => {}
    }

    event
}

// ── Accessibility check ───────────────────────────────────────────────────────

fn check_accessibility() -> bool {
    let trusted = unsafe { ffi::AXIsProcessTrustedWithOptions(std::ptr::null()) };
    trusted != 0
}

// ── Start key listener (own thread + CFRunLoop) ───────────────────────────────

fn start_key_listener(tx: std::sync::mpsc::Sender<KeyMsg>, option_down_shared: Arc<AtomicBool>) {
    let ctx = Box::new(TapContext { tx, option_down: false, option_down_shared });
    // Store as usize so the closure is Send (raw pointers are not Send).
    let ctx_addr: usize = Box::into_raw(ctx) as usize;

    std::thread::spawn(move || {
        let ctx_raw = ctx_addr as *mut std::os::raw::c_void;
        let mask: ffi::CGEventMask = (1 << ffi::kCGEventKeyDown)
            | (1 << ffi::kCGEventKeyUp)
            | (1 << ffi::kCGEventFlagsChanged);

        let tap = unsafe {
            ffi::CGEventTapCreate(
                ffi::kCGHIDEventTap,
                ffi::kCGHeadInsertEventTap,
                ffi::kCGEventTapOptionDefault,  // intercepting, not listen-only
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
    wid:       u32,   // CGWindowID — unique per window, used for rotation & raise
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

/// Fetch window titles for a given PID via the Accessibility API.
/// Returns titles in front-to-back order (matching CGWindowList Z-order).
/// Returns an empty vec if AX is unavailable or the app has no windows.
fn ax_window_titles(pid: i32) -> Vec<String> {
    unsafe {
        let app_elem = ffi::AXUIElementCreateApplication(pid);
        if app_elem.is_null() { return vec![]; }

        let attr_name = std::ffi::CString::new("AXWindows").unwrap();
        let cf_attr = ffi::CFStringCreateWithCString(
            std::ptr::null(),
            attr_name.as_ptr() as *const _,
            ffi::kCFStringEncodingUTF8,
        );
        if cf_attr.is_null() {
            ffi::CFRelease(app_elem as ffi::CFTypeRef);
            return vec![];
        }

        let mut windows_val: ffi::CFTypeRef = std::ptr::null();
        let err = ffi::AXUIElementCopyAttributeValue(app_elem, cf_attr, &mut windows_val);
        ffi::CFRelease(cf_attr);
        ffi::CFRelease(app_elem as ffi::CFTypeRef);

        if err != ffi::kAXErrorSuccess || windows_val.is_null() { return vec![]; }

        let count = ffi::CFArrayGetCount(windows_val as ffi::CFArrayRef);
        let mut titles = Vec::new();

        let title_attr_name = std::ffi::CString::new("AXTitle").unwrap();
        let cf_title_attr = ffi::CFStringCreateWithCString(
            std::ptr::null(),
            title_attr_name.as_ptr() as *const _,
            ffi::kCFStringEncodingUTF8,
        );

        for i in 0..count {
            let win = ffi::CFArrayGetValueAtIndex(windows_val as ffi::CFArrayRef, i);
            if win.is_null() { titles.push(String::new()); continue; }

            let mut title_val: ffi::CFTypeRef = std::ptr::null();
            let err2 = ffi::AXUIElementCopyAttributeValue(
                win as ffi::AXUIElementRef,
                cf_title_attr,
                &mut title_val,
            );
            if err2 == ffi::kAXErrorSuccess && !title_val.is_null() {
                if ffi::CFGetTypeID(title_val) == ffi::CFStringGetTypeID() {
                    let s = cf_string_to_rust(title_val as ffi::CFStringRef)
                        .unwrap_or_default();
                    ffi::CFRelease(title_val);
                    titles.push(s);
                } else {
                    ffi::CFRelease(title_val);
                    titles.push(String::new());
                }
            } else {
                titles.push(String::new());
            }
        }

        if !cf_title_attr.is_null() { ffi::CFRelease(cf_title_attr); }
        ffi::CFRelease(windows_val);

        titles
    }
}

/// Raise and focus a specific window identified by its CGWindowID.
///
/// Strategy (in order):
///   1. Match AX window by AXWindowIdentifier (CGWindowID, read as i64).
///   2. Fall back to matching by AXTitle == win_title.
///   3. Fall back to activate_pid only (brings app to front, macOS picks window).
///
/// AXRaise is called on the matched element, then activate_pid brings the app
/// to front.  The ordering matters: raise first, then activate, so the raised
/// window ends up as the frontmost one.
fn raise_window(pid: i32, wid: u32, win_title: &str) {
    unsafe {
        let app_elem = ffi::AXUIElementCreateApplication(pid);
        if app_elem.is_null() { activate_pid(pid); return; }

        // --- fetch AXWindows list ------------------------------------------
        let cf_ax_windows = cf_str("AXWindows");
        let mut windows_val: ffi::CFTypeRef = std::ptr::null();
        let err = ffi::AXUIElementCopyAttributeValue(app_elem, cf_ax_windows, &mut windows_val);
        ffi::CFRelease(cf_ax_windows);

        if err != ffi::kAXErrorSuccess || windows_val.is_null() {
            ffi::CFRelease(app_elem as ffi::CFTypeRef);
            activate_pid(pid);
            return;
        }

        let cf_wid_attr    = cf_str("AXWindowIdentifier");
        let cf_title_attr  = cf_str("AXTitle");
        let cf_raise_action = cf_str("AXRaise");

        let count = ffi::CFArrayGetCount(windows_val as ffi::CFArrayRef);
        let mut target: ffi::AXUIElementRef = std::ptr::null_mut();

        // Pass 1 — match by CGWindowID (AXWindowIdentifier, 64-bit).
        for i in 0..count {
            let win = ffi::CFArrayGetValueAtIndex(windows_val as ffi::CFArrayRef, i);
            if win.is_null() { continue; }
            let mut id_val: ffi::CFTypeRef = std::ptr::null();
            let e = ffi::AXUIElementCopyAttributeValue(
                win as ffi::AXUIElementRef, cf_wid_attr, &mut id_val,
            );
            if e == ffi::kAXErrorSuccess && !id_val.is_null() {
                if ffi::CFGetTypeID(id_val) == ffi::CFNumberGetTypeID() {
                    let mut win_id: i64 = 0;
                    ffi::CFNumberGetValue(
                        id_val as ffi::CFNumberRef,
                        ffi::kCFNumberSInt64Type,
                        &mut win_id as *mut _ as *mut _,
                    );
                    ffi::CFRelease(id_val);
                    if win_id as u32 == wid {
                        target = win as ffi::AXUIElementRef;
                        break;
                    }
                } else {
                    ffi::CFRelease(id_val);
                }
            }
        }

        // Pass 2 — match by AXTitle if wid match failed and we have a title.
        if target.is_null() && !win_title.is_empty() {
            for i in 0..count {
                let win = ffi::CFArrayGetValueAtIndex(windows_val as ffi::CFArrayRef, i);
                if win.is_null() { continue; }
                let title = ax_element_title(win as ffi::AXUIElementRef, cf_title_attr);
                if title.as_deref() == Some(win_title) {
                    target = win as ffi::AXUIElementRef;
                    break;
                }
            }
        }

        if !target.is_null() {
            ffi::AXUIElementPerformAction(target, cf_raise_action);
        }

        ffi::CFRelease(cf_wid_attr);
        ffi::CFRelease(cf_title_attr);
        ffi::CFRelease(cf_raise_action);
        ffi::CFRelease(windows_val);
        ffi::CFRelease(app_elem as ffi::CFTypeRef);

        // Activate the app — must come AFTER AXRaise so the raised window wins.
        activate_pid(pid);
    }
}

/// Create a CFStringRef from a &str (UTF-8).  Caller must CFRelease.
unsafe fn cf_str(s: &str) -> ffi::CFStringRef {
    let c = std::ffi::CString::new(s).unwrap();
    ffi::CFStringCreateWithCString(std::ptr::null(), c.as_ptr() as *const _, ffi::kCFStringEncodingUTF8)
}

/// Read AXTitle from an AX element.
unsafe fn ax_element_title(
    elem: ffi::AXUIElementRef,
    cf_title_attr: ffi::CFStringRef,
) -> Option<String> {
    let mut val: ffi::CFTypeRef = std::ptr::null();
    let e = ffi::AXUIElementCopyAttributeValue(elem, cf_title_attr, &mut val);
    if e != ffi::kAXErrorSuccess || val.is_null() { return None; }
    if ffi::CFGetTypeID(val) != ffi::CFStringGetTypeID() {
        ffi::CFRelease(val);
        return None;
    }
    let s = cf_string_to_rust(val as ffi::CFStringRef);
    ffi::CFRelease(val);
    s
}

/// Returns the raw on-screen window list **without** rotation.
/// Index 0 = currently focused window (CGWindowList Z-order).
fn list_windows_raw() -> Vec<WinEntry> {
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

        let wid = unsafe { dict_i32(dict, "kCGWindowNumber") }.unwrap_or(0) as u32;
        let win_title = unsafe { dict_string(dict, "kCGWindowName") }.unwrap_or_default();

        out.push(WinEntry { pid, wid, app_name, win_title });
    }
    unsafe { ffi::CFRelease(array) };

    // kCGWindowName is often empty on macOS 13+ without Screen Recording
    // permission.  Fill in missing titles via the Accessibility API instead,
    // which only needs the Accessibility permission we already require.
    // AX window order matches CGWindowList front-to-back order for the same PID.
    let mut ax_cache: HashMap<i32, Vec<String>> = HashMap::new();
    // per-pid window index counter so we map AX titles positionally
    let mut pid_idx: HashMap<i32, usize> = HashMap::new();
    for e in out.iter_mut() {
        if e.win_title.is_empty() {
            let titles = ax_cache.entry(e.pid).or_insert_with(|| ax_window_titles(e.pid));
            let idx = pid_idx.entry(e.pid).or_insert(0);
            if let Some(t) = titles.get(*idx) {
                if !t.is_empty() { e.win_title = t.clone(); }
            }
            *idx += 1;
        }
    }

    // Remove titles that are identical to the app name and the app has only
    // one window (no disambiguation value).
    let count_per_pid: HashMap<i32, usize> = {
        let mut m: HashMap<i32, usize> = HashMap::new();
        for e in &out { *m.entry(e.pid).or_insert(0) += 1; }
        m
    };
    for e in out.iter_mut() {
        if count_per_pid.get(&e.pid).copied().unwrap_or(0) == 1
            && e.win_title == e.app_name
        {
            e.win_title.clear();
        }
    }

    out
}

/// Build the display list for the switcher overlay.
///
/// `raw` is the unmodified CGWindowList (index 0 = current foreground window).
///
/// All windows are shown — nothing is filtered out.  The list is rotated so
/// that `prev_pid` (the window focused just before the current one) lands at
/// index 0 and is pre-selected.  The current window ends up somewhere later
/// in the list (wherever it naturally falls after the rotation).
fn build_window_list(raw: Vec<WinEntry>, prev_pid: i32) -> (Vec<WinEntry>, usize) {
    if raw.is_empty() { return (raw, 0); }

    let mut out = raw;

    // Rotate so prev_pid is at index 0.
    if prev_pid > 0 {
        if let Some(idx) = out.iter().position(|e| e.pid == prev_pid) {
            if idx > 0 {
                let len = out.len();
                out.rotate_left(idx.min(len - 1));
            }
        }
    }
    // If prev_pid not found (first launch, single window, etc.) index 0 is
    // whatever CGWindowList returns first after the current window — fine.

    (out, 0)
}

// ── Per-window accent colour palette (Kanagawa-adjacent) ─────────────────────
//
// Used to visually distinguish multiple windows from the same application.
// The palette cycles per-window *within* an app group (not globally).

const WIN_PALETTE: &[egui::Color32] = &[
    egui::Color32::from_rgb(126, 156, 216), // crystalBlue
    egui::Color32::from_rgb(152, 187, 108), // springGreen
    egui::Color32::from_rgb(229, 183, 103), // carpYellow
    egui::Color32::from_rgb(210, 126, 153), // sakuraPink
    egui::Color32::from_rgb(149, 127, 184), // oniViolet
    egui::Color32::from_rgb(127, 180, 202), // dragonBlue
    egui::Color32::from_rgb(255, 160, 102), // surimiOrange
    egui::Color32::from_rgb(106, 153, 85),  // leafGreen
];

/// Returns a per-window accent color.
/// Windows of the same app share a palette slot assignment so each window
/// within an app gets a unique (cycling) color from WIN_PALETTE.
fn build_window_colors(wins: &[WinEntry]) -> Vec<egui::Color32> {
    // Count occurrences per app name so we can assign per-window offsets.
    let mut app_counter: HashMap<String, usize> = HashMap::new();
    let mut colors = Vec::with_capacity(wins.len());

    for entry in wins {
        let idx = app_counter.entry(entry.app_name.clone()).or_insert(0);
        let color = WIN_PALETTE[*idx % WIN_PALETTE.len()];
        colors.push(color);
        *idx += 1;
    }
    colors
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
        }
    }
}

// ── Layout ────────────────────────────────────────────────────────────────────

const CARD_W:   f32 = 100.0;
const CARD_H:   f32 = 110.0;
const CARD_PAD: f32 = 14.0;
const WIN_PAD:  f32 = 28.0;
const HINT_H:   f32 = 0.0;  // legend removed

// ── Switcher app ──────────────────────────────────────────────────────────────

struct SwitcherApp {
    visible:    Arc<AtomicBool>,
    selected:   Arc<AtomicUsize>,
    windows:    Arc<Mutex<Vec<WinEntry>>>,
    win_colors: Arc<Mutex<Vec<egui::Color32>>>,
    msg_rx:     std::sync::mpsc::Receiver<KeyMsg>,
    colors:     Colors,
    /// PID of the window that was focused before the *current* foreground app.
    prev_pid:   Arc<AtomicI32>,
    /// Live Option key state written by the event tap thread.
    option_down: Arc<AtomicBool>,
    /// prev_pid captured at first Tab press for silent swap if Option was
    /// already released by the time update() runs.
    pending_prev_pid: i32,
    /// wid (CGWindowID) counterpart to pending_prev_pid.
    pending_prev_wid: u32,
    /// win_title counterpart to pending_prev_pid (for title-based fallback).
    pending_prev_title: String,
}

impl eframe::App for SwitcherApp {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut do_tab  = false;
        let mut do_hide = false;

        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                KeyMsg::TabPressed     => do_tab  = true,
                KeyMsg::OptionReleased => do_hide = true,
            }
        }

        // ── Tab pressed ───────────────────────────────────────────────────────
        if do_tab {
            if self.visible.load(Ordering::Relaxed) {
                // Overlay already showing — cycle to next window.
                let len = self.windows.lock().unwrap().len();
                if len > 0 {
                    let cur = self.selected.load(Ordering::Relaxed);
                    self.selected.store((cur + 1) % len, Ordering::Relaxed);
                }
            } else {
                // Capture window list once — raw[0] is current foreground window,
                // raw[1] is previously focused (our silent-swap target).
                let raw  = list_windows_raw();
                let prev        = raw.get(1).map(|e| e.pid).unwrap_or(0);
                let prev_wid    = raw.get(1).map(|e| e.wid).unwrap_or(0);
                let prev_title  = raw.get(1).map(|e| e.win_title.clone()).unwrap_or_default();
                self.pending_prev_pid   = prev;
                self.pending_prev_wid   = prev_wid;
                self.pending_prev_title = prev_title;
                self.prev_pid.store(prev, Ordering::Relaxed);

                // Pass raw into build_window_list so it strips the current app
                // and uses the same snapshot (no second CGWindowList call).
                let (wins, sel) = build_window_list(raw, prev);
                let colors = build_window_colors(&wins);
                *self.windows.lock().unwrap()    = wins;
                *self.win_colors.lock().unwrap() = colors;
                self.selected.store(sel, Ordering::Relaxed);

                if self.option_down.load(Ordering::Relaxed) {
                    // Option is still held — show the overlay.
                    self.visible.store(true, Ordering::Relaxed);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                } else {
                    // Option already released before update() ran — silent swap.
                    if self.pending_prev_pid > 0 {
                        raise_window(self.pending_prev_pid, self.pending_prev_wid, &self.pending_prev_title.clone());
                    }
                    self.pending_prev_pid   = 0;
                    self.pending_prev_wid   = 0;
                    self.pending_prev_title = String::new();
                }
            }
        }

        // ── Option released ───────────────────────────────────────────────────
        if do_hide {
            if self.visible.load(Ordering::Relaxed) {
                // Overlay was shown — activate selected window.
                self.visible.store(false, Ordering::Relaxed);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                let wins = self.windows.lock().unwrap().clone();
                let sel  = self.selected.load(Ordering::Relaxed);
                if let Some(e) = wins.get(sel) {
                    raise_window(e.pid, e.wid, &e.win_title);
                }
            }
            self.pending_prev_pid   = 0;
            self.pending_prev_wid   = 0;
            self.pending_prev_title = String::new();
        }

        ctx.request_repaint_after(if self.visible.load(Ordering::Relaxed) {
            std::time::Duration::from_millis(16)
        } else {
            std::time::Duration::from_millis(50)
        });

        if !self.visible.load(Ordering::Relaxed) { return; }

        let wins   = self.windows.lock().unwrap().clone();
        let colors = self.win_colors.lock().unwrap().clone();
        let sel    = self.selected.load(Ordering::Relaxed);
        let c      = &self.colors;

        let n     = wins.len().max(1) as f32;
        let win_w = (n * (CARD_W + CARD_PAD) - CARD_PAD + WIN_PAD * 2.0).min(1200.0);
        let win_h = CARD_H + WIN_PAD * 2.0 + HINT_H;

        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(win_w, win_h)));

        // Re-center on screen every frame (InnerSize alone doesn't reposition).
        if let Some(monitor) = ctx.input(|i| i.viewport().monitor_size) {
            let x = (monitor.x - win_w) / 2.0;
            let y = monitor.y * 0.42 - win_h / 2.0;
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
        }

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
                        let is_sel    = i == sel;
                        let win_color = colors.get(i).copied()
                            .unwrap_or(egui::Color32::from_rgb(126, 156, 216));

                        let (r, _) = ui.allocate_exact_size(
                            egui::vec2(CARD_W, CARD_H),
                            egui::Sense::hover(),
                        );

                        // Card background
                        ui.painter().rect(
                            r,
                            egui::Rounding::ZERO,
                            if is_sel { c.card_sel } else { c.card_bg },
                            egui::Stroke::new(
                                if is_sel { 2.0 } else { 1.0 },
                                if is_sel { win_color } else { c.border },
                            ),
                        );

                        // Thin color bar at top of card to identify the window
                        let bar_rect = egui::Rect::from_min_size(
                            r.min,
                            egui::vec2(CARD_W, 3.0),
                        );
                        ui.painter().rect_filled(bar_rect, egui::Rounding::ZERO, win_color);

                        // Glyph (app icon)
                        ui.painter().text(
                            egui::pos2(r.center().x, r.top() + 36.0),
                            egui::Align2::CENTER_CENTER,
                            glyph_for(&entry.app_name),
                            egui::FontId::proportional(24.0),
                            if is_sel { win_color } else { c.fg },
                        );

                        // App name
                        ui.painter().text(
                            egui::pos2(r.center().x, r.top() + 63.0),
                            egui::Align2::CENTER_CENTER,
                            truncate(&entry.app_name, 13),
                            egui::FontId::proportional(10.0),
                            c.fg,
                        );

                        // Window title — always shown, color-tinted on selected
                        let title_text = if entry.win_title.is_empty() {
                            "—".to_string()
                        } else {
                            truncate(&entry.win_title, 14)
                        };
                        ui.painter().text(
                            egui::pos2(r.center().x, r.top() + 80.0),
                            egui::Align2::CENTER_CENTER,
                            title_text,
                            egui::FontId::proportional(9.0),
                            if is_sel { win_color } else { c.fg_dim },
                        );
                    }
                });
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
    if !check_accessibility() {
        eprintln!(
            "mofisw: Accessibility permission not granted.\n\
             Open System Settings → Privacy & Security → Accessibility\n\
             and add mofisw, then re-run."
        );
        std::process::exit(1);
    }

    let (tx, rx) = std::sync::mpsc::channel::<KeyMsg>();
    let option_down = Arc::new(AtomicBool::new(false));
    start_key_listener(tx, Arc::clone(&option_down));

    let visible   = Arc::new(AtomicBool::new(false));
    let selected  = Arc::new(AtomicUsize::new(0));
    let prev_pid  = Arc::new(AtomicI32::new(0));
    let windows: Arc<Mutex<Vec<WinEntry>>>         = Arc::new(Mutex::new(Vec::new()));
    let win_colors: Arc<Mutex<Vec<egui::Color32>>> = Arc::new(Mutex::new(Vec::new()));
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
                Box::new(SwitcherApp {
                    visible,
                    selected,
                    windows,
                    win_colors,
                    msg_rx: rx,
                    colors: Colors::kanagawa(),
                    prev_pid,
                    option_down,
                    pending_prev_pid:   0,
                    pending_prev_wid:   0,
                    pending_prev_title: String::new(),
                })
        }),
    )
}
