//! System-tray icon for mofi on Linux via the StatusNotifierItem D-Bus protocol.
//!
//! Renders the same Nerd Font glyph shown next to the currently selected item
//! in the UI, coloured with the matching accent colour on a dark rounded-square
//! background — visually identical to the macOS in-window glyph rendering.
//!
//! The icon updates live as the user navigates the list, switches tabs, or
//! hides/shows the window.

use fontdue::{Font, FontSettings};

/// Icon size in pixels (SNI standard).
const ICON_SIZE: i32 = 22;

/// Font size in px for rasterising active glyphs.
const GLYPH_PX: f32 = 16.0;

/// Font size for the idle "M" — larger to fill the icon.
const IDLE_GLYPH_PX: f32 = 18.0;

/// Active background (Kanagawa sumiInk1 — dark panel).
const BG: [u8; 3] = [0x1F, 0x1F, 0x28];

/// Idle background — dark Kanagawa violet (#362B50).
const IDLE_BG: [u8; 3] = [0x36, 0x2B, 0x50];

/// Border colour — lighter violet so it reads against both bgs (#7E6BA8).
const BORDER: [u8; 3] = [0x7E, 0x6B, 0xA8];

/// Border width in pixels.
const BORDER_W: f32 = 1.5;

/// Idle glyph colour — white so it pops on the purple bg.
const IDLE_FG: [u8; 3] = [0xFF, 0xFF, 0xFF];

/// Corner radius for the rounded-square.
const CORNER_R: f32 = 4.0;

/// Default idle glyph — plain capital "M" from MapleMono.
const IDLE_GLYPH: &str = "M";

// ── Tray struct ───────────────────────────────────────────────────────────────

pub struct MofiTray {
    pub label: String,
    glyph: String,
    fg: [u8; 3],
    bg: [u8; 3],
    glyph_px: f32,
    font: Font,
}

impl MofiTray {
    pub fn new(font: Font) -> Self {
        Self {
            label: "mofi".into(),
            glyph: IDLE_GLYPH.into(),
            fg: IDLE_FG,
            bg: IDLE_BG,
            glyph_px: IDLE_GLYPH_PX,
            font,
        }
    }

    pub fn set_icon(&mut self, glyph: &str, fg: [u8; 3], label: &str, visible: bool) {
        if visible {
            self.glyph = glyph.into();
            self.fg = fg;
            self.bg = BG;
            self.glyph_px = GLYPH_PX;
            self.label = label.into();
        } else {
            self.glyph = IDLE_GLYPH.into();
            self.fg = IDLE_FG;
            self.bg = IDLE_BG;
            self.glyph_px = IDLE_GLYPH_PX;
            self.label = "mofi".into();
        }
    }
}

impl ksni::Tray for MofiTray {
    fn id(&self) -> String {
        "mofi".into()
    }

    fn title(&self) -> String {
        self.label.clone()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![render_icon(
            &self.font,
            &self.glyph,
            self.fg,
            self.bg,
            self.glyph_px,
        )]
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![ksni::MenuItem::Standard(ksni::menu::StandardItem {
            label: self.label.clone(),
            enabled: false,
            ..Default::default()
        })]
    }
}

// ── Icon rendering ────────────────────────────────────────────────────────────

/// Render the tray icon: border (outer) -> fill (inner) -> glyph.
/// The border persists across idle and active states.
fn render_icon(font: &Font, glyph: &str, fg: [u8; 3], bg: [u8; 3], glyph_px: f32) -> ksni::Icon {
    let size = ICON_SIZE;
    let mut argb = vec![0u8; (size * size * 4) as usize];

    for y in 0..size {
        for x in 0..size {
            let idx = ((y * size + x) * 4) as usize;

            // Outer rounded rect = border fill.
            let outer_a =
                rounded_rect_alpha(x as f32, y as f32, size as f32, size as f32, CORNER_R);
            if outer_a == 0 {
                continue;
            }

            // Inner rounded rect = background fill (inset by BORDER_W).
            let inner_a = rounded_rect_alpha(
                x as f32 - BORDER_W,
                y as f32 - BORDER_W,
                size as f32 - 2.0 * BORDER_W,
                size as f32 - 2.0 * BORDER_W,
                (CORNER_R - BORDER_W).max(0.5),
            );

            // Blend: where inner covers, show bg; where only outer covers, show border.
            // inner_a is relative to the inner rect, but we need it as coverage
            // within the outer rect's alpha.
            let inner_coverage = (inner_a as u32 * outer_a as u32 / 255) as u8;
            let border_coverage = outer_a.saturating_sub(inner_coverage);

            // Composite: border colour * border_coverage + bg * inner_coverage.
            let total = border_coverage as u32 + inner_coverage as u32;
            if total == 0 {
                continue;
            }
            let r = (BORDER[0] as u32 * border_coverage as u32
                + bg[0] as u32 * inner_coverage as u32)
                / total;
            let g = (BORDER[1] as u32 * border_coverage as u32
                + bg[1] as u32 * inner_coverage as u32)
                / total;
            let b = (BORDER[2] as u32 * border_coverage as u32
                + bg[2] as u32 * inner_coverage as u32)
                / total;

            argb[idx] = outer_a;
            argb[idx + 1] = r.min(255) as u8;
            argb[idx + 2] = g.min(255) as u8;
            argb[idx + 3] = b.min(255) as u8;
        }
    }

    // Rasterise the glyph and composite over the background.
    if let Some(ch) = glyph.chars().next() {
        let (metrics, bitmap) = font.rasterize(ch, glyph_px);
        if !bitmap.is_empty() && metrics.width > 0 && metrics.height > 0 {
            let gw = metrics.width as i32;
            let gh = metrics.height as i32;
            let ox = (size - gw) / 2;
            let oy = (size - gh) / 2;

            for gy in 0..gh {
                for gx in 0..gw {
                    let px = ox + gx;
                    let py = oy + gy;
                    if px < 0 || py < 0 || px >= size || py >= size {
                        continue;
                    }
                    let coverage = bitmap[(gy * gw + gx) as usize];
                    if coverage == 0 {
                        continue;
                    }
                    let idx = ((py * size + px) * 4) as usize;
                    let bg_a = argb[idx] as u32;
                    let fg_a = coverage as u32;
                    let out_a = fg_a + bg_a * (255 - fg_a) / 255;
                    if out_a == 0 {
                        continue;
                    }
                    argb[idx] = out_a.min(255) as u8;
                    argb[idx + 1] = ((fg[0] as u32 * fg_a
                        + argb[idx + 1] as u32 * bg_a * (255 - fg_a) / 255)
                        / out_a)
                        .min(255) as u8;
                    argb[idx + 2] = ((fg[1] as u32 * fg_a
                        + argb[idx + 2] as u32 * bg_a * (255 - fg_a) / 255)
                        / out_a)
                        .min(255) as u8;
                    argb[idx + 3] = ((fg[2] as u32 * fg_a
                        + argb[idx + 3] as u32 * bg_a * (255 - fg_a) / 255)
                        / out_a)
                        .min(255) as u8;
                }
            }
        }
    }

    ksni::Icon {
        width: size,
        height: size,
        data: argb,
    }
}

/// Returns pixel alpha (0-255) for a rounded rectangle at the given offset/size.
fn rounded_rect_alpha(x: f32, y: f32, w: f32, h: f32, r: f32) -> u8 {
    let cx = x + 0.5;
    let cy = y + 0.5;

    if cx < 0.0 || cy < 0.0 || cx > w || cy > h {
        return 0;
    }

    let in_x = cx >= r && cx <= w - r;
    let in_y = cy >= r && cy <= h - r;

    if in_x || in_y {
        let dx = cx.min(w - cx);
        let dy = cy.min(h - cy);
        return aa_edge(dx.min(dy));
    }

    let corner_cx = if cx < r { r } else { w - r };
    let corner_cy = if cy < r { r } else { h - r };
    let dist = ((cx - corner_cx).powi(2) + (cy - corner_cy).powi(2)).sqrt();
    aa_edge(r - dist)
}

fn aa_edge(d: f32) -> u8 {
    if d >= 0.5 {
        255
    } else if d <= -0.5 {
        0
    } else {
        ((d + 0.5) * 255.0) as u8
    }
}

// ── Font loading ──────────────────────────────────────────────────────────────

fn load_nerd_font() -> Option<Font> {
    let stems = ["MapleMono-NF-Regular"];
    let dirs = font_search_dirs();
    for stem in &stems {
        for ext in &["ttf", "otf"] {
            let filename = format!("{}.{}", stem, ext);
            for dir in &dirs {
                let path = dir.join(&filename);
                if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(f) = Font::from_bytes(bytes, FontSettings::default()) {
                        return Some(f);
                    }
                }
            }
        }
        for dir in &dirs {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.contains(stem)
                        && (name_str.ends_with(".ttf") || name_str.ends_with(".otf"))
                    {
                        if let Ok(bytes) = std::fs::read(entry.path()) {
                            if let Ok(f) = Font::from_bytes(bytes, FontSettings::default()) {
                                return Some(f);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn font_search_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
    }
    dirs.push(std::path::PathBuf::from("/usr/local/share/fonts"));
    dirs.push(std::path::PathBuf::from("/usr/share/fonts"));
    dirs.push(std::path::PathBuf::from("/usr/share/fonts/truetype"));
    dirs.push(std::path::PathBuf::from("/usr/share/fonts/OTF"));
    dirs
}

// ── Public API ────────────────────────────────────────────────────────────────

pub type TrayHandle = ksni::Handle<MofiTray>;

pub fn spawn_tray() -> TrayHandle {
    let font = load_nerd_font().unwrap_or_else(|| {
        eprintln!("[mofi-tray] MapleMono NF not found, using built-in fallback");
        Font::from_bytes(
            include_bytes!("/usr/share/fonts/noto/NotoSans-Regular.ttf").to_vec(),
            FontSettings::default(),
        )
        .expect("fallback font load failed")
    });
    let tray = MofiTray::new(font);
    let service = ksni::TrayService::new(tray);
    let handle = service.handle();
    service.spawn();
    handle
}
