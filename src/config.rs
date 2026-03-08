use egui::Color32;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── Config file path ──────────────────────────────────────────────────────────

pub fn config_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config"));
    base.join("mofi").join("config.toml")
}

// ── On-disk schema ────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub theme: String, // empty string → "kanagawa"
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = toml::from_str::<Config>(&text) {
                return cfg;
            }
        }
        Self::default()
    }

    pub fn save(&self) {
        let path = config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = toml::to_string(self) {
            let _ = std::fs::write(path, text);
        }
    }

    pub fn active_theme_name(&self) -> &str {
        if self.theme.is_empty() {
            "kanagawa"
        } else {
            &self.theme
        }
    }
}

// ── Theme palette ─────────────────────────────────────────────────────────────

/// All colors needed to render the UI.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: &'static str,

    // Backgrounds
    pub bg: Color32,        // window fill (with alpha)
    pub bg_alpha: u8,       // alpha for the window fill
    pub row_hover: Color32, // unfocused hover row
    pub row_sel: Color32,   // selected row bg
    pub border: Color32,    // window border
    pub separator: Color32, // horizontal rule

    // Foregrounds
    pub fg: Color32,            // main text
    pub fg_dim: Color32,        // dim / secondary text
    pub fg_muted: Color32,      // very dim / comments / hints
    pub accent: Color32,        // active tab label, selection bar, titles
    pub accent2: Color32,       // subtitle text when selected
    pub tab_active_bg: Color32, // active tab fill
    pub icon_sel: Color32,      // icon when row selected
    pub icon_dim: Color32,      // icon when row not selected
    pub toast: Color32,         // toast message text
    pub brand: Color32,         // "Mofi" label in tab bar
}

const fn c(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

// ── 10 built-in themes ────────────────────────────────────────────────────────

pub const THEME_NAMES: &[&str] = &[
    "kanagawa",
    "gruvbox",
    "nord",
    "tokyonight",
    "dracula",
    "solarized",
    "monokai",
    "catppuccin",
    "onedark",
    "rosepine",
];

pub fn theme_by_name(name: &str) -> Theme {
    match name {
        "gruvbox" => gruvbox(),
        "nord" => nord(),
        "tokyonight" => tokyonight(),
        "dracula" => dracula(),
        "solarized" => solarized(),
        "monokai" => monokai(),
        "catppuccin" => catppuccin(),
        "onedark" => onedark(),
        "rosepine" => rosepine(),
        _ => kanagawa(),
    }
}

// ── Kanagawa (default) ───────────────────────────────────────────────────────
fn kanagawa() -> Theme {
    Theme {
        name: "kanagawa",
        bg: c(0x16, 0x16, 0x1D),
        bg_alpha: 210,
        row_hover: c(0x2A, 0x2A, 0x37),
        row_sel: c(0x2D, 0x4F, 0x67),
        border: c(0x54, 0x54, 0x6D),
        separator: c(0x36, 0x36, 0x46),
        fg: c(0xDC, 0xD7, 0xBA),
        fg_dim: c(0xC8, 0xC0, 0x93),
        fg_muted: c(0x72, 0x71, 0x69),
        accent: c(0x7E, 0x9C, 0xD8),
        accent2: c(0x93, 0x8A, 0xA9),
        tab_active_bg: c(0x22, 0x32, 0x49),
        icon_sel: c(0x7E, 0x9C, 0xD8),
        icon_dim: c(0x54, 0x54, 0x6D),
        toast: c(0x98, 0xBB, 0x6C),
        brand: c(0x54, 0x54, 0x6D),
    }
}

// ── Gruvbox Dark ─────────────────────────────────────────────────────────────
fn gruvbox() -> Theme {
    Theme {
        name: "gruvbox",
        bg: c(0x28, 0x28, 0x28),
        bg_alpha: 220,
        row_hover: c(0x3C, 0x38, 0x36),
        row_sel: c(0x50, 0x49, 0x45),
        border: c(0x66, 0x5C, 0x54),
        separator: c(0x3C, 0x38, 0x36),
        fg: c(0xEB, 0xDB, 0xB2),
        fg_dim: c(0xD5, 0xC4, 0xA1),
        fg_muted: c(0x92, 0x83, 0x74),
        accent: c(0xFB, 0xBF, 0x2E),  // bright yellow
        accent2: c(0x83, 0xA5, 0x98), // aqua
        tab_active_bg: c(0x45, 0x40, 0x3D),
        icon_sel: c(0xFB, 0xBF, 0x2E),
        icon_dim: c(0x66, 0x5C, 0x54),
        toast: c(0xB8, 0xBB, 0x26), // green
        brand: c(0x66, 0x5C, 0x54),
    }
}

// ── Nord ─────────────────────────────────────────────────────────────────────
fn nord() -> Theme {
    Theme {
        name: "nord",
        bg: c(0x2E, 0x34, 0x40),
        bg_alpha: 220,
        row_hover: c(0x3B, 0x42, 0x52),
        row_sel: c(0x43, 0x4C, 0x5E),
        border: c(0x4C, 0x56, 0x6A),
        separator: c(0x3B, 0x42, 0x52),
        fg: c(0xEC, 0xEF, 0xF4),
        fg_dim: c(0xD8, 0xDE, 0xE9),
        fg_muted: c(0x61, 0x6E, 0x88),
        accent: c(0x88, 0xC0, 0xD0), // frost blue
        accent2: c(0x81, 0xA1, 0xC1),
        tab_active_bg: c(0x3B, 0x42, 0x52),
        icon_sel: c(0x88, 0xC0, 0xD0),
        icon_dim: c(0x4C, 0x56, 0x6A),
        toast: c(0xA3, 0xBE, 0x8C), // green
        brand: c(0x4C, 0x56, 0x6A),
    }
}

// ── Tokyo Night ──────────────────────────────────────────────────────────────
fn tokyonight() -> Theme {
    Theme {
        name: "tokyonight",
        bg: c(0x1A, 0x1B, 0x26),
        bg_alpha: 215,
        row_hover: c(0x24, 0x28, 0x3A),
        row_sel: c(0x28, 0x3A, 0x57),
        border: c(0x41, 0x4A, 0x67),
        separator: c(0x2A, 0x2D, 0x3E),
        fg: c(0xC0, 0xCA, 0xF5),
        fg_dim: c(0xA9, 0xB1, 0xD6),
        fg_muted: c(0x56, 0x5F, 0x89),
        accent: c(0x7A, 0xA2, 0xF7),  // blue
        accent2: c(0x9D, 0x7C, 0xD8), // purple
        tab_active_bg: c(0x24, 0x28, 0x3A),
        icon_sel: c(0x7A, 0xA2, 0xF7),
        icon_dim: c(0x41, 0x4A, 0x67),
        toast: c(0x9E, 0xCE, 0x6A), // green
        brand: c(0x41, 0x4A, 0x67),
    }
}

// ── Dracula ──────────────────────────────────────────────────────────────────
fn dracula() -> Theme {
    Theme {
        name: "dracula",
        bg: c(0x28, 0x2A, 0x36),
        bg_alpha: 220,
        row_hover: c(0x38, 0x3A, 0x4A),
        row_sel: c(0x44, 0x47, 0x5A),
        border: c(0x62, 0x72, 0xA4),
        separator: c(0x38, 0x3A, 0x4A),
        fg: c(0xF8, 0xF8, 0xF2),
        fg_dim: c(0xCC, 0xCC, 0xCC),
        fg_muted: c(0x62, 0x72, 0xA4),
        accent: c(0xBD, 0x93, 0xF9),  // purple
        accent2: c(0xFF, 0x79, 0xC6), // pink
        tab_active_bg: c(0x44, 0x47, 0x5A),
        icon_sel: c(0xBD, 0x93, 0xF9),
        icon_dim: c(0x62, 0x72, 0xA4),
        toast: c(0x50, 0xFA, 0x7B), // green
        brand: c(0x62, 0x72, 0xA4),
    }
}

// ── Solarized Dark ────────────────────────────────────────────────────────────
fn solarized() -> Theme {
    Theme {
        name: "solarized",
        bg: c(0x00, 0x2B, 0x36),
        bg_alpha: 220,
        row_hover: c(0x07, 0x36, 0x42),
        row_sel: c(0x0D, 0x3D, 0x4E),
        border: c(0x58, 0x6E, 0x75),
        separator: c(0x07, 0x36, 0x42),
        fg: c(0x83, 0x94, 0x96),
        fg_dim: c(0x65, 0x7B, 0x83),
        fg_muted: c(0x58, 0x6E, 0x75),
        accent: c(0x26, 0x8B, 0xD2),  // blue
        accent2: c(0x2A, 0xA1, 0x98), // cyan
        tab_active_bg: c(0x07, 0x36, 0x42),
        icon_sel: c(0x26, 0x8B, 0xD2),
        icon_dim: c(0x58, 0x6E, 0x75),
        toast: c(0x85, 0x99, 0x00), // green
        brand: c(0x58, 0x6E, 0x75),
    }
}

// ── Monokai ──────────────────────────────────────────────────────────────────
fn monokai() -> Theme {
    Theme {
        name: "monokai",
        bg: c(0x27, 0x28, 0x22),
        bg_alpha: 220,
        row_hover: c(0x38, 0x39, 0x30),
        row_sel: c(0x49, 0x49, 0x3E),
        border: c(0x75, 0x71, 0x5E),
        separator: c(0x38, 0x39, 0x30),
        fg: c(0xF8, 0xF8, 0xF2),
        fg_dim: c(0xCC, 0xCC, 0xBB),
        fg_muted: c(0x75, 0x71, 0x5E),
        accent: c(0x66, 0xD9, 0xE8),  // cyan
        accent2: c(0xAE, 0x81, 0xFF), // purple
        tab_active_bg: c(0x49, 0x49, 0x3E),
        icon_sel: c(0x66, 0xD9, 0xE8),
        icon_dim: c(0x75, 0x71, 0x5E),
        toast: c(0xA6, 0xE2, 0x2E), // green
        brand: c(0x75, 0x71, 0x5E),
    }
}

// ── Catppuccin Mocha ──────────────────────────────────────────────────────────
fn catppuccin() -> Theme {
    Theme {
        name: "catppuccin",
        bg: c(0x1E, 0x1E, 0x2E),
        bg_alpha: 215,
        row_hover: c(0x31, 0x32, 0x44),
        row_sel: c(0x45, 0x47, 0x5A),
        border: c(0x58, 0x5B, 0x70),
        separator: c(0x31, 0x32, 0x44),
        fg: c(0xCD, 0xD6, 0xF4),
        fg_dim: c(0xBA, 0xC2, 0xDE),
        fg_muted: c(0x6C, 0x70, 0x86),
        accent: c(0x89, 0xB4, 0xFA),  // blue
        accent2: c(0xCB, 0xA6, 0xF7), // mauve
        tab_active_bg: c(0x31, 0x32, 0x44),
        icon_sel: c(0x89, 0xB4, 0xFA),
        icon_dim: c(0x58, 0x5B, 0x70),
        toast: c(0xA6, 0xE3, 0xA1), // green
        brand: c(0x58, 0x5B, 0x70),
    }
}

// ── One Dark ─────────────────────────────────────────────────────────────────
fn onedark() -> Theme {
    Theme {
        name: "onedark",
        bg: c(0x28, 0x2C, 0x34),
        bg_alpha: 220,
        row_hover: c(0x33, 0x37, 0x3E),
        row_sel: c(0x3E, 0x44, 0x51),
        border: c(0x4B, 0x52, 0x63),
        separator: c(0x33, 0x37, 0x3E),
        fg: c(0xAB, 0xB2, 0xBF),
        fg_dim: c(0x9D, 0xA5, 0xB4),
        fg_muted: c(0x5C, 0x63, 0x70),
        accent: c(0x61, 0xAF, 0xEF),  // blue
        accent2: c(0xC6, 0x78, 0xDD), // purple
        tab_active_bg: c(0x3E, 0x44, 0x51),
        icon_sel: c(0x61, 0xAF, 0xEF),
        icon_dim: c(0x4B, 0x52, 0x63),
        toast: c(0x98, 0xC3, 0x79), // green
        brand: c(0x4B, 0x52, 0x63),
    }
}

// ── Rosé Pine ────────────────────────────────────────────────────────────────
fn rosepine() -> Theme {
    Theme {
        name: "rosepine",
        bg: c(0x19, 0x17, 0x24),
        bg_alpha: 215,
        row_hover: c(0x26, 0x23, 0x33),
        row_sel: c(0x40, 0x3D, 0x52),
        border: c(0x6E, 0x6A, 0x86),
        separator: c(0x26, 0x23, 0x33),
        fg: c(0xE0, 0xDE, 0xF4),
        fg_dim: c(0xC4, 0xC1, 0xD9),
        fg_muted: c(0x6E, 0x6A, 0x86),
        accent: c(0x9C, 0xCF, 0xD8),  // foam (cyan)
        accent2: c(0xC4, 0xA7, 0xE7), // iris (purple)
        tab_active_bg: c(0x26, 0x23, 0x33),
        icon_sel: c(0x9C, 0xCF, 0xD8),
        icon_dim: c(0x6E, 0x6A, 0x86),
        toast: c(0x31, 0x74, 0x8F), // pine (teal)
        brand: c(0x6E, 0x6A, 0x86),
    }
}
