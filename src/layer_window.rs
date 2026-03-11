/// Layer-shell window for mofi on Wayland/Linux.
///
/// Uses `smithay-client-toolkit` for the Wayland event loop and
/// `zwlr_layer_shell_v1` for a surface that can be truly hidden (unmapped)
/// while the daemon process stays alive.  GPU rendering is done via
/// `glutin` (EGL) + `egui_glow`.
///
/// Public API:
///   `LayerWindow::new(…)` — create the window in the *unmapped* state
///   `LayerWindow::run(app)`  — block; calls `app.update(ctx)` each frame
///
/// The window is shown/hidden by the `RofiApp` logic inside `app.update()`:
///   show: `ctx.send_viewport_cmd(ViewportCommand::InnerSize(640×380))`
///   hide: `ctx.send_viewport_cmd(ViewportCommand::InnerSize(1×1))`
/// We intercept those commands and map/unmap the layer surface accordingly.
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use egui::ViewportCommand;
use egui_glow::Painter;
use glutin::config::ConfigTemplateBuilder;
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentContext, PossiblyCurrentContext, Version,
};
use glutin::display::{Display, DisplayApiPreference};
use glutin::prelude::*;
use glutin::surface::{Surface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{HasRawDisplayHandle, RawWindowHandle, WaylandWindowHandle};
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
    Connection, QueueHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};

// Window dimensions when visible.
const WIN_W: u32 = 810;
const WIN_H: u32 = 550;

// ── Public entry points ───────────────────────────────────────────────────────

/// One-shot mode: create the window already mapped (visible), run until the app
/// signals close (update returns true), then return the app so the caller can
/// read the result.  Used by `--client` on Linux when running without a daemon.
#[allow(dead_code)]
pub fn run_oneshot<A: AppHandler>(app: A) -> A {
    let dummy_sigterm = Arc::new(AtomicBool::new(false));
    run_inner(app, dummy_sigterm, true)
}

/// Daemon mode: start unmapped, run until SIGTERM or the app closes.
pub fn run<A: AppHandler>(app: A, sigterm: Arc<AtomicBool>) {
    run_inner(app, sigterm, false);
}

fn run_inner<A: AppHandler>(mut app: A, sigterm: Arc<AtomicBool>, start_mapped: bool) -> A {
    let _ = env_logger::try_init();

    let conn = Connection::connect_to_env().expect("cannot connect to Wayland compositor");
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor not available");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("zwlr_layer_shell_v1 not available");

    // ── wp_viewporter — corrects HiDPI blur by telling the compositor to
    //    display the physical-pixel buffer at logical dimensions.
    let wp_viewporter: wp_viewporter::WpViewporter = globals
        .bind(&qh, 1..=1, ())
        .expect("wp_viewporter not available");

    // ── EGL display from the Wayland connection ───────────────────────────────
    // `client_system` feature on wayland-backend enables `display_ptr()` and
    // `HasRawDisplayHandle` on the sys backend type.
    let raw_display = conn.backend().raw_display_handle();

    let gl_display = unsafe {
        Display::new(raw_display, DisplayApiPreference::Egl).expect("failed to create EGL display")
    };

    // ── EGL config ───────────────────────────────────────────────────────────
    let config_template = ConfigTemplateBuilder::new().with_alpha_size(8).build();
    let gl_config = unsafe {
        gl_display
            .find_configs(config_template)
            .unwrap()
            .next()
            .expect("no EGL config found")
    };

    // ── Create a wl_surface but DO NOT commit yet (start unmapped) ────────────
    let wl_surface = compositor.create_surface(&qh);

    // Create a wp_viewport for this surface before attaching it to the layer.
    let viewport: wp_viewport::WpViewport = wp_viewporter.get_viewport(&wl_surface, &qh, ());

    // Build the layer surface (anchored to centre, overlay layer).
    let layer = layer_shell.create_layer_surface(
        &qh,
        wl_surface.clone(),
        Layer::Overlay,
        Some("mofi"),
        None,
    );
    layer.set_anchor(Anchor::empty()); // centred
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_size(WIN_W, WIN_H);
    // Initial commit — tells the compositor the surface exists but we haven't
    // attached a buffer yet, so it stays unmapped until we paint.
    layer.commit();

    // ── Read scale factor early — needed for physical EGL surface size ────────
    // We need scale before creating the EGL surface so the buffer is the right
    // physical size from the start (gl_surface.resize is advisory on some drivers).
    // We read MOFI_SCALE here; the wl_output integer scale is a fallback read
    // after the roundtrip below.
    let early_scale = std::env::var("MOFI_SCALE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(1.0);
    let phys_w_init = (WIN_W as f32 * early_scale).round() as u32;
    let phys_h_init = (WIN_H as f32 * early_scale).round() as u32;

    // ── EGL context (not-yet-current) ─────────────────────────────────────────
    let ctx_attrs = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 0))))
        .build(None); // no native window needed at context-creation time
    let gl_ctx_nc: NotCurrentContext = unsafe {
        gl_display
            .create_context(&gl_config, &ctx_attrs)
            .expect("failed to create EGL context")
    };

    // ── EGL surface (need a wl_surface pointer) ───────────────────────────────
    let wl_surface_ptr = wl_surface.id().as_ptr() as *mut c_void;
    let mut wl_window_handle = WaylandWindowHandle::empty();
    wl_window_handle.surface = wl_surface_ptr;
    let raw_window = RawWindowHandle::Wayland(wl_window_handle);

    let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
        raw_window,
        NonZeroU32::new(phys_w_init).unwrap(),
        NonZeroU32::new(phys_h_init).unwrap(),
    );
    let gl_surface = unsafe {
        gl_display
            .create_window_surface(&gl_config, &surface_attrs)
            .expect("failed to create EGL surface")
    };

    // Make context current so we can create the glow/painter.
    let gl_ctx: PossiblyCurrentContext = gl_ctx_nc
        .make_current(&gl_surface)
        .expect("failed to make EGL context current");

    // Disable vsync (swap interval 0) so swap_buffers returns immediately.
    // We drive the frame rate ourselves via the 16ms poll loop.  With interval=1
    // EGL blocks on an internal wl_surface.frame callback which can stall
    // permanently after an unmap/remap cycle if the compositor stops sending them.
    gl_surface
        .set_swap_interval(&gl_ctx, glutin::surface::SwapInterval::DontWait)
        .ok();

    // ── glow + egui_glow Painter ──────────────────────────────────────────────
    let gl = unsafe {
        glow::Context::from_loader_function(|s| {
            let s = std::ffi::CString::new(s).unwrap();
            gl_display.get_proc_address(s.as_c_str()) as *const _
        })
    };
    let gl = Arc::new(gl);
    let painter =
        Painter::new(Arc::clone(&gl), "", None).expect("failed to create egui_glow Painter");

    // ── egui Context ─────────────────────────────────────────────────────────
    let egui_ctx = egui::Context::default();
    // NOTE: app.setup() is called AFTER set_pixels_per_point below, so that
    // fonts are rasterized at the correct HiDPI scale from the first frame.

    // ── Build the main state ──────────────────────────────────────────────────
    let mut state = LayerState {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),

        layer,
        wl_surface,
        viewport,

        keyboard: None,
        pointer: None,
        keyboard_focus: false,
        had_focus: false,

        // EGL / painting
        gl_ctx,
        gl_surface,
        gl: Arc::clone(&gl),
        painter,
        egui_ctx: egui_ctx.clone(),

        // Dimensions & state
        width: WIN_W,
        height: WIN_H,
        scale: 1.0, // updated after roundtrip below
        mapped: false,
        mapping_pending: false,
        should_close: false,
        pending_resize: None,

        // Egui input accumulator
        egui_input: egui::RawInput::default(),
        pointer_pos: None,
        modifiers: egui::Modifiers::default(),
        repeat_key: None,
    };

    // Do a blocking roundtrip so that outputs are enumerated before we read
    // the scale factor.  This fills OutputState with real data.
    event_queue.roundtrip(&mut state).ok();

    // Read the scale factor from the first available output.
    // Note: wl_output.scale is integer-only. For fractional scaling (e.g. 1.5×
    // on Hyprland), set the MOFI_SCALE environment variable.
    let wl_scale = state
        .output_state
        .outputs()
        .next()
        .and_then(|o| state.output_state.info(&o))
        .map(|info| info.scale_factor as f32)
        .unwrap_or(1.0);
    let scale = std::env::var("MOFI_SCALE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(wl_scale);
    eprintln!(
        "[mofi] output scale: {} (wl_output={}, MOFI_SCALE env={})",
        scale,
        wl_scale,
        std::env::var("MOFI_SCALE").unwrap_or_else(|_| "unset".into())
    );
    state.scale = scale;
    state.egui_ctx.set_pixels_per_point(scale);
    // Now that pixels_per_point is set, load fonts so the atlas is rasterized
    // at the correct HiDPI density from the very first frame.
    app.setup(&state.egui_ctx);

    // ── Centering: margins are computed dynamically in map_surface() ────────
    // Each time the surface is mapped, map_surface queries the focused monitor
    // via hyprctl and sets margins to centre the window on that output.
    // Set initial anchor for the layer shell (will be refined in map_surface).
    if let Some((sw, sh)) = focused_monitor_logical_size() {
        let margin_x = ((sw - WIN_W as i32) / 2).max(0);
        let margin_y = ((sh - WIN_H as i32) / 2).max(0);
        eprintln!(
            "[mofi] initial screen logical {sw}x{sh}, margins top={margin_y} left={margin_x}"
        );
        state.layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        state.layer.set_margin(margin_y, 0, 0, margin_x);
        state.layer.commit();
        event_queue.flush().ok();
    } else {
        eprintln!("[mofi] focused monitor unavailable at startup, using Anchor::empty()");
    }

    // In oneshot mode, map the surface immediately so the window appears as
    // soon as the event loop starts (no SIGUSR1 needed).
    if start_mapped {
        state.map_surface(WIN_W, WIN_H);
        // Roundtrip to get the configure ack before entering the main loop.
        event_queue.roundtrip(&mut state).ok();
    }

    // ── Main event loop ───────────────────────────────────────────────────────
    // We use a non-blocking poll+dispatch pattern so that the app's toggle
    // (SIGUSR1) is processed even while the surface is unmapped and the
    // compositor sends no Wayland events.
    let wayland_fd = conn.backend().poll_fd().as_raw_fd();

    loop {
        // Flush any pending outbound Wayland messages.
        event_queue.flush().ok();

        // Poll the Wayland fd for up to 16 ms so we stay responsive but
        // don't spin at 100 % CPU when nothing is happening.
        let mut pfd = libc::pollfd {
            fd: wayland_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut pfd, 1, 16) };

        // Read any events that arrived, then dispatch them.
        if pfd.revents & libc::POLLIN != 0 {
            if let Some(guard) = event_queue.prepare_read() {
                guard.read().ok();
            }
        }
        event_queue
            .dispatch_pending(&mut state)
            .expect("Wayland event dispatch failed");

        if state.should_close || sigterm.load(Ordering::Relaxed) {
            break;
        }

        // If the compositor sent us a configure, handle pending resize.
        if let Some((w, h)) = state.pending_resize.take() {
            if w > 0 && h > 0 {
                state.width = w;
                state.height = h;
                // EGL surface needs physical pixels.
                state.gl_surface.resize(
                    &state.gl_ctx,
                    NonZeroU32::new(state.phys_w()).unwrap(),
                    NonZeroU32::new(state.phys_h()).unwrap(),
                );
            }
        }

        // Generate synthetic key-repeat events for held keys.
        state.pump_key_repeat();

        // Always run app logic (processes toggle / viewport commands) so the
        // surface can be mapped even when currently unmapped.  Rendering is
        // skipped inside paint_frame when !mapped.
        state.paint_frame(&mut app);

        if state.should_close || sigterm.load(Ordering::Relaxed) {
            break;
        }
    }
    app
}

// ── Trait the caller implements ───────────────────────────────────────────────

pub trait AppHandler {
    /// Called once before the first frame, with a valid egui Context.
    fn setup(&mut self, ctx: &egui::Context);
    /// Called every frame. Return `true` to close the window.
    fn update(&mut self, ctx: &egui::Context) -> bool;
}

// ── Internal state ────────────────────────────────────────────────────────────

struct LayerState {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,

    layer: LayerSurface,
    wl_surface: wl_surface::WlSurface,
    /// wp_viewport for this surface — used to tell the compositor to render
    /// the physical-pixel EGL buffer at logical dimensions (fixes HiDPI blur).
    viewport: wp_viewport::WpViewport,

    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard_focus: bool,
    /// True once the compositor has granted keyboard focus at least once.
    /// Used to defer the focus-loss auto-hide guard until focus has arrived.
    had_focus: bool,

    // EGL
    gl_ctx: PossiblyCurrentContext,
    gl_surface: Surface<WindowSurface>,
    gl: Arc<glow::Context>,
    painter: Painter,
    egui_ctx: egui::Context,

    /// Logical width/height (surface-local coordinates, used for set_size and egui).
    width: u32,
    height: u32,
    /// Output scale factor (e.g. 1.5 on HiDPI).  Physical pixels = logical * scale.
    scale: f32,
    mapped: bool,
    /// True when map_surface() has been called but we haven't yet received
    /// the compositor's configure acknowledgement.  We hold off rendering
    /// until the configure arrives so the compositor accepts our buffer.
    mapping_pending: bool,
    should_close: bool,
    pending_resize: Option<(u32, u32)>,

    egui_input: egui::RawInput,
    pointer_pos: Option<egui::Pos2>,
    modifiers: egui::Modifiers,

    /// Key-repeat state.  We implement repeat ourselves because SCTK's
    /// calloop-based repeat isn't used (we drive our own poll loop).
    repeat_key: Option<RepeatState>,
}

/// Tracks a held key for software key-repeat.
struct RepeatState {
    keysym: Keysym,
    /// When the key was first pressed.
    pressed_at: std::time::Instant,
    /// When we last emitted a repeat event.
    last_repeat: std::time::Instant,
    /// UTF-8 text associated with the key (for Text events).
    utf8: Option<String>,
}

/// Delay before first repeat fires (ms).
const REPEAT_DELAY_MS: u64 = 300;
/// Interval between subsequent repeats (ms).
const REPEAT_INTERVAL_MS: u64 = 30;

impl LayerState {
    /// Physical pixel dimensions of the window (logical * scale).
    fn phys_w(&self) -> u32 {
        (self.width as f32 * self.scale).round() as u32
    }
    fn phys_h(&self) -> u32 {
        (self.height as f32 * self.scale).round() as u32
    }

    /// Generate synthetic key-repeat events for a held key.
    fn pump_key_repeat(&mut self) {
        let repeat = match self.repeat_key.as_mut() {
            Some(r) => r,
            None => return,
        };
        let now = std::time::Instant::now();
        let held = now.duration_since(repeat.pressed_at);
        if held.as_millis() < REPEAT_DELAY_MS as u128 {
            return;
        }
        let since_last = now.duration_since(repeat.last_repeat);
        if since_last.as_millis() < REPEAT_INTERVAL_MS as u128 {
            return;
        }
        // Emit a synthetic press event.
        if let Some(ev) = keysym_to_egui(repeat.keysym, true, self.modifiers) {
            self.egui_input.events.push(ev);
        }
        if let Some(ref utf8) = repeat.utf8 {
            self.egui_input.events.push(egui::Event::Text(utf8.clone()));
        }
        repeat.last_repeat = now;
    }

    fn paint_frame<A: AppHandler>(&mut self, app: &mut A) {
        // Build the screen rect for egui (in egui points = logical pixels at scale=1.0,
        // or width/height when ppp=scale since phys_w/ppp = width).
        self.egui_input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(self.width as f32, self.height as f32),
        ));

        let raw_input = self.egui_input.take();
        // Inject current keyboard focus state so egui's ctx.input(|i| i.focused)
        // returns the correct value every frame (not just on focus-change events).
        let mut raw_input = raw_input;
        raw_input.focused = self.keyboard_focus;
        let mut app_wants_close = false;
        let full_output = self.egui_ctx.run(raw_input, |ctx| {
            app_wants_close = app.update(ctx);
        });

        // Handle viewport commands emitted by the app.
        let mut new_size: Option<(u32, u32)> = None;
        let mut want_close = false;
        let cmds: &[ViewportCommand] = full_output
            .viewport_output
            .values()
            .next()
            .map(|v| v.commands.as_slice())
            .unwrap_or(&[]);
        for cmd in cmds {
            match cmd {
                ViewportCommand::InnerSize(sz) => {
                    let w = sz.x.round() as u32;
                    let h = sz.y.round() as u32;
                    new_size = Some((w.max(1), h.max(1)));
                }
                ViewportCommand::Focus => {
                    // Keyboard interactivity is Exclusive when mapped — focus comes automatically.
                }
                ViewportCommand::Close => {
                    want_close = true;
                }
                _ => {}
            }
        }

        if let Some((w, h)) = new_size {
            if w <= 4 || h <= 4 {
                // Hide: unmap by attaching null buffer.
                self.unmap_surface();
            } else if !self.mapping_pending {
                // Show: map/resize (skip if a map is already in progress).
                if !self.mapped || self.width != w || self.height != h {
                    self.map_surface(w, h);
                }
            }
        }

        // App signalled close (update() returned true) or sent ViewportCommand::Close.
        if want_close || app_wants_close {
            self.should_close = true;
            return;
        }

        // ── Tessellate (always, so shapes are consumed) ───────────────────────
        let clipped_primitives = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        if !self.mapped || self.mapping_pending {
            // Upload any pending textures (font atlas etc.) to the GPU even
            // while unmapped, so they are ready for the first visible frame.
            // Pass an empty primitive list so nothing is actually drawn.
            self.painter.paint_and_update_textures(
                [self.phys_w(), self.phys_h()],
                full_output.pixels_per_point,
                &[],
                &full_output.textures_delta,
            );
            return;
        }

        // ── Render ────────────────────────────────────────────────────────────
        // Clear to fully transparent so the semi-transparent panel background
        // painted by egui (using the theme's bg_alpha) shows through to the
        // desktop beneath the window.
        let pw = self.phys_w() as i32;
        let ph = self.phys_h() as i32;
        use glow::HasContext as _;
        unsafe {
            self.gl.viewport(0, 0, pw, ph);
            self.gl.clear_color(0.0, 0.0, 0.0, 0.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }

        self.painter.paint_and_update_textures(
            [self.phys_w(), self.phys_h()],
            full_output.pixels_per_point,
            &clipped_primitives,
            &full_output.textures_delta,
        );

        self.gl_surface
            .swap_buffers(&self.gl_ctx)
            .expect("swap_buffers failed");

        // Request continuous repaint while visible.
        self.egui_ctx.request_repaint();
    }

    fn map_surface(&mut self, w: u32, h: u32) {
        self.width = w;
        self.height = h;
        // Don't set mapped=true yet — wait for the compositor's configure
        // callback so we know it has accepted the surface before we render.
        self.mapping_pending = true;

        // ── Recompute centering margins for the focused monitor ──────────
        // Query Hyprland for the currently focused monitor so the window
        // appears centred on the monitor where the mouse/keyboard focus is,
        // not just the first output in the list.
        if let Some((sw, sh)) = focused_monitor_logical_size() {
            let margin_x = ((sw - w as i32) / 2).max(0);
            let margin_y = ((sh - h as i32) / 2).max(0);
            self.layer.set_anchor(Anchor::TOP | Anchor::LEFT);
            self.layer.set_margin(margin_y, 0, 0, margin_x);
        }

        eprintln!(
            "[mofi] map_surface({w}x{h} logical, {}x{} phys) — waiting for configure",
            self.phys_w(),
            self.phys_h()
        );

        // Resize the EGL surface in physical pixels.
        self.gl_surface.resize(
            &self.gl_ctx,
            NonZeroU32::new(self.phys_w()).unwrap(),
            NonZeroU32::new(self.phys_h()).unwrap(),
        );

        // Tell the compositor to display our physical-pixel buffer at logical
        // dimensions — this is what prevents the blurry / over-sized rendering
        // at fractional HiDPI scales (e.g. 1.5× on Hyprland).
        self.viewport.set_destination(w as i32, h as i32);

        // Update layer-shell size in logical pixels and commit.
        // The compositor will send a configure event; we set mapped=true there.
        self.layer.set_size(w, h);
        self.layer
            .set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        self.layer.commit();

        // Update egui screen rect immediately (in egui points).
        self.egui_input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(w as f32, h as f32),
        ));
    }

    fn unmap_surface(&mut self) {
        eprintln!("[mofi] unmap_surface");
        self.mapped = false;
        self.mapping_pending = false;
        self.repeat_key = None; // Stop any in-progress key repeat.
                                // Disable the viewport so the compositor doesn't try to scale a null buffer.
        self.viewport.set_destination(-1, -1);
        // Attach null buffer + commit → compositor unmaps the surface.
        self.wl_surface.attach(None, 0, 0);
        self.wl_surface.commit();
    }
}

// ── sctk delegate implementations ────────────────────────────────────────────

impl CompositorHandler for LayerState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        // Frame callback — nothing needed; we repaint in the main loop.
    }
}

impl OutputHandler for LayerState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for LayerState {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.should_close = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        eprintln!(
            "[mofi] configure: new_size={:?} mapped={} mapping_pending={}",
            configure.new_size, self.mapped, self.mapping_pending
        );
        let (w, h) = configure.new_size;
        if w != 0 && h != 0 {
            if self.mapping_pending {
                // First configure after map_surface — now we can actually render.
                self.mapping_pending = false;
                self.mapped = true;
                eprintln!("[mofi] configure: mapped=true after pending configure");
                // Resize the EGL surface to the compositor-confirmed size (physical px).
                self.width = w;
                self.height = h;
                self.gl_surface.resize(
                    &self.gl_ctx,
                    NonZeroU32::new(self.phys_w()).unwrap(),
                    NonZeroU32::new(self.phys_h()).unwrap(),
                );
            } else {
                self.pending_resize = Some((w, h));
            }
        }
        // If the compositor sends (0,0) after an unmap, just ignore it —
        // we don't want to resize the EGL surface to zero.
    }
}

impl SeatHandler for LayerState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let kb = self
                .seat_state
                .get_keyboard(qh, &seat, None)
                .expect("failed to create keyboard");
            self.keyboard = Some(kb);
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let ptr = self
                .seat_state
                .get_pointer(qh, &seat)
                .expect("failed to create pointer");
            self.pointer = Some(ptr);
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(kb) = self.keyboard.take() {
                kb.release();
            }
        }
        if capability == Capability::Pointer {
            if let Some(ptr) = self.pointer.take() {
                ptr.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for LayerState {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _keysyms: &[Keysym],
    ) {
        if self.layer.wl_surface() == surface {
            self.keyboard_focus = true;
            self.had_focus = true;
            self.egui_input
                .events
                .push(egui::Event::WindowFocused(true));
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self.layer.wl_surface() == surface {
            self.keyboard_focus = false;
            self.repeat_key = None;
            self.egui_input
                .events
                .push(egui::Event::WindowFocused(false));
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if let Some(ev) = keysym_to_egui(event.keysym, true, self.modifiers) {
            self.egui_input.events.push(ev);
        }
        // Text input — skip control characters (e.g. \x0b from Ctrl+K)
        let utf8 = event
            .utf8
            .filter(|s| !s.is_empty() && s.chars().all(|c| c >= ' ' || c == '\t'));
        if let Some(ref utf8) = utf8 {
            self.egui_input.events.push(egui::Event::Text(utf8.clone()));
        }
        // Start key repeat tracking — only for navigation / modifier keys,
        // NOT for plain text input (which would cause stuck-key rapid fire).
        let should_repeat = self.modifiers.ctrl
            || self.modifiers.alt
            || matches!(
                event.keysym,
                Keysym::Up
                    | Keysym::Down
                    | Keysym::Left
                    | Keysym::Right
                    | Keysym::BackSpace
                    | Keysym::Delete
                    | Keysym::Tab
                    | Keysym::Home
                    | Keysym::End
                    | Keysym::Page_Up
                    | Keysym::Page_Down
            );
        if should_repeat {
            let now = std::time::Instant::now();
            self.repeat_key = Some(RepeatState {
                keysym: event.keysym,
                pressed_at: now,
                last_repeat: now,
                utf8,
            });
        } else {
            self.repeat_key = None;
        }
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if let Some(ev) = keysym_to_egui(event.keysym, false, self.modifiers) {
            self.egui_input.events.push(ev);
        }
        // Stop repeat if this key was being repeated.
        if let Some(ref rk) = self.repeat_key {
            if rk.keysym == event.keysym {
                self.repeat_key = None;
            }
        }
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
    ) {
        self.modifiers = egui::Modifiers {
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            mac_cmd: false,
            command: modifiers.ctrl,
        };
    }
}

impl PointerHandler for LayerState {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.layer.wl_surface() {
                continue;
            }
            match event.kind {
                PointerEventKind::Enter { .. } => {
                    let pos = egui::pos2(event.position.0 as f32, event.position.1 as f32);
                    self.pointer_pos = Some(pos);
                    self.egui_input.events.push(egui::Event::PointerMoved(pos));
                }
                PointerEventKind::Leave { .. } => {
                    self.pointer_pos = None;
                    self.egui_input.events.push(egui::Event::PointerGone);
                }
                PointerEventKind::Motion { .. } => {
                    let pos = egui::pos2(event.position.0 as f32, event.position.1 as f32);
                    self.pointer_pos = Some(pos);
                    self.egui_input.events.push(egui::Event::PointerMoved(pos));
                }
                PointerEventKind::Press { button, .. } => {
                    if let Some(pos) = self.pointer_pos {
                        if let Some(btn) = wayland_button_to_egui(button) {
                            self.egui_input.events.push(egui::Event::PointerButton {
                                pos,
                                button: btn,
                                pressed: true,
                                modifiers: self.modifiers,
                            });
                        }
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if let Some(pos) = self.pointer_pos {
                        if let Some(btn) = wayland_button_to_egui(button) {
                            self.egui_input.events.push(egui::Event::PointerButton {
                                pos,
                                button: btn,
                                pressed: false,
                                modifiers: self.modifiers,
                            });
                        }
                    }
                }
                PointerEventKind::Axis {
                    vertical,
                    horizontal,
                    ..
                } => {
                    let delta = egui::vec2(
                        horizontal.absolute as f32 * 10.0,
                        vertical.absolute as f32 * 10.0,
                    );
                    self.egui_input.events.push(egui::Event::Scroll(delta));
                }
            }
        }
    }
}

// ── sctk delegate macros ──────────────────────────────────────────────────────

delegate_compositor!(LayerState);
delegate_output!(LayerState);
delegate_seat!(LayerState);
delegate_keyboard!(LayerState);
delegate_pointer!(LayerState);
delegate_layer!(LayerState);
delegate_registry!(LayerState);

impl ProvidesRegistryState for LayerState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

// ── Dispatch impls for wp_viewporter / wp_viewport ───────────────────────────
// These protocols have no events we need to handle, so the impls are no-ops.

use smithay_client_toolkit::reexports::client::Dispatch;

impl Dispatch<wp_viewporter::WpViewporter, ()> for LayerState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_viewporter::WpViewporter,
        _event: wp_viewporter::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // wp_viewporter has no events.
    }
}

impl Dispatch<wp_viewport::WpViewport, ()> for LayerState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_viewport::WpViewport,
        _event: wp_viewport::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // wp_viewport has no events.
    }
}

// ── Key translation helpers ───────────────────────────────────────────────────

fn keysym_to_egui(sym: Keysym, pressed: bool, modifiers: egui::Modifiers) -> Option<egui::Event> {
    use smithay_client_toolkit::seat::keyboard::Keysym as K;
    let key = match sym {
        K::Escape => egui::Key::Escape,
        K::Return | K::KP_Enter => egui::Key::Enter,
        K::Tab => egui::Key::Tab,
        K::BackSpace => egui::Key::Backspace,
        K::Delete => egui::Key::Delete,
        K::Up => egui::Key::ArrowUp,
        K::Down => egui::Key::ArrowDown,
        K::Left => egui::Key::ArrowLeft,
        K::Right => egui::Key::ArrowRight,
        K::Home => egui::Key::Home,
        K::End => egui::Key::End,
        K::Page_Up => egui::Key::PageUp,
        K::Page_Down => egui::Key::PageDown,
        K::F1 => egui::Key::F1,
        K::F2 => egui::Key::F2,
        K::F3 => egui::Key::F3,
        K::F4 => egui::Key::F4,
        K::F5 => egui::Key::F5,
        K::F6 => egui::Key::F6,
        K::F7 => egui::Key::F7,
        K::F8 => egui::Key::F8,
        K::F9 => egui::Key::F9,
        K::F10 => egui::Key::F10,
        K::F11 => egui::Key::F11,
        K::F12 => egui::Key::F12,
        K::a | K::A => egui::Key::A,
        K::b | K::B => egui::Key::B,
        K::c | K::C => egui::Key::C,
        K::d | K::D => egui::Key::D,
        K::e | K::E => egui::Key::E,
        K::f | K::F => egui::Key::F,
        K::g | K::G => egui::Key::G,
        K::h | K::H => egui::Key::H,
        K::i | K::I => egui::Key::I,
        K::j | K::J => egui::Key::J,
        K::k | K::K => egui::Key::K,
        K::l | K::L => egui::Key::L,
        K::m | K::M => egui::Key::M,
        K::n | K::N => egui::Key::N,
        K::o | K::O => egui::Key::O,
        K::p | K::P => egui::Key::P,
        K::q | K::Q => egui::Key::Q,
        K::r | K::R => egui::Key::R,
        K::s | K::S => egui::Key::S,
        K::t | K::T => egui::Key::T,
        K::u | K::U => egui::Key::U,
        K::v | K::V => egui::Key::V,
        K::w | K::W => egui::Key::W,
        K::x | K::X => egui::Key::X,
        K::y | K::Y => egui::Key::Y,
        K::z | K::Z => egui::Key::Z,
        K::_0 | K::KP_0 => egui::Key::Num0,
        K::_1 | K::KP_1 => egui::Key::Num1,
        K::_2 | K::KP_2 => egui::Key::Num2,
        K::_3 | K::KP_3 => egui::Key::Num3,
        K::_4 | K::KP_4 => egui::Key::Num4,
        K::_5 | K::KP_5 => egui::Key::Num5,
        K::_6 | K::KP_6 => egui::Key::Num6,
        K::_7 | K::KP_7 => egui::Key::Num7,
        K::_8 | K::KP_8 => egui::Key::Num8,
        K::_9 | K::KP_9 => egui::Key::Num9,
        _ => return None,
    };
    Some(egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    })
}

fn wayland_button_to_egui(button: u32) -> Option<egui::PointerButton> {
    // Linux input event codes: BTN_LEFT=0x110, BTN_RIGHT=0x111, BTN_MIDDLE=0x112
    match button {
        0x110 => Some(egui::PointerButton::Primary),
        0x111 => Some(egui::PointerButton::Secondary),
        0x112 => Some(egui::PointerButton::Middle),
        _ => None,
    }
}

// ── Hyprland monitor helpers ──────────────────────────────────────────────────

/// Query `hyprctl monitors -j` to find the focused monitor's logical size.
/// Returns `Some((width, height))` for the monitor that currently has focus
/// (where the mouse/keyboard is), or `None` if the query fails.
fn focused_monitor_logical_size() -> Option<(i32, i32)> {
    let output = std::process::Command::new("hyprctl")
        .args(["monitors", "-j"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let monitors: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    for m in &monitors {
        if m.get("focused").and_then(|v| v.as_bool()) == Some(true) {
            let w = m.get("width").and_then(|v| v.as_i64())? as f64;
            let h = m.get("height").and_then(|v| v.as_i64())? as f64;
            let scale = m.get("scale").and_then(|v| v.as_f64()).unwrap_or(1.0);
            return Some(((w / scale).round() as i32, (h / scale).round() as i32));
        }
    }
    None
}
