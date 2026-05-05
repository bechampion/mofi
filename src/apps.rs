use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Represents a discovered application.
#[derive(Clone, Debug)]
pub struct AppEntry {
    pub name: String,
    pub path: String,
    /// Resolved filesystem path to the app icon (PNG), if found.
    pub icon_path: Option<String>,
}

pub fn discover_apps() -> Vec<AppEntry> {
    #[cfg(target_os = "macos")]
    return discover_apps_macos();

    #[cfg(target_os = "linux")]
    return discover_apps_linux();

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Vec::new();
}

// ── macOS: scan .app bundles ──────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn discover_apps_macos() -> Vec<AppEntry> {
    let home = dirs::home_dir().unwrap_or_default();

    let search_dirs = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        home.join("Applications"),
    ];

    let mut seen: HashSet<String> = HashSet::new();
    let mut apps = Vec::new();

    for dir in &search_dirs {
        scan_dir_macos(dir, &mut seen, &mut apps);
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

#[cfg(target_os = "macos")]
fn scan_dir_macos(dir: &Path, seen: &mut HashSet<String>, apps: &mut Vec<AppEntry>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());

        if ext == Some("app") {
            collect_app(&path, seen, apps);
        } else if path.is_dir() {
            // One level of subdirectory (Utilities, Setapp, etc.)
            if let Ok(sub) = std::fs::read_dir(&path) {
                for sub_entry in sub.flatten() {
                    let sub_path = sub_entry.path();
                    if sub_path.extension().and_then(|e| e.to_str()) == Some("app") {
                        collect_app(&sub_path, seen, apps);
                    }
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn collect_app(path: &Path, seen: &mut HashSet<String>, apps: &mut Vec<AppEntry>) {
    let path_str = path.to_string_lossy().to_string();
    if !seen.insert(path_str.clone()) {
        return;
    }
    let name = match path.file_stem().and_then(|s| s.to_str()) {
        Some(n) => n.to_string(),
        None => return,
    };
    apps.push(AppEntry {
        name,
        path: path_str,
        icon_path: None,
    });
}

// ── Linux: scan XDG .desktop files ───────────────────────────────────────────

#[cfg(target_os = "linux")]
fn discover_apps_linux() -> Vec<AppEntry> {
    let mut search_dirs: Vec<PathBuf> = Vec::new();

    // User-local applications first.
    if let Some(home) = dirs::home_dir() {
        search_dirs.push(home.join(".local/share/applications"));
    }

    // XDG_DATA_DIRS (colon-separated) — defaults to /usr/local/share:/usr/share.
    let xdg_data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for dir in xdg_data_dirs.split(':') {
        search_dirs.push(PathBuf::from(dir).join("applications"));
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut apps = Vec::new();

    for dir in &search_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
                    if let Some(app) = parse_desktop_file(&path, &mut seen) {
                        apps.push(app);
                    }
                }
            }
        }
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

/// Parse a .desktop file and return an AppEntry if it is a launchable application.
/// Skips entries where NoDisplay=true, Hidden=true, or Type != Application.
#[cfg(target_os = "linux")]
fn parse_desktop_file(path: &Path, seen: &mut HashSet<String>) -> Option<AppEntry> {
    let content = std::fs::read_to_string(path).ok()?;

    let mut name: Option<String> = None;
    let mut exec: Option<String> = None;
    let mut icon: Option<String> = None;
    let mut app_type: Option<String> = None;
    let mut no_display = false;
    let mut hidden = false;
    let mut in_desktop_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line == "[Desktop Entry]" {
            in_desktop_entry = true;
            continue;
        }
        // Stop at the next section header.
        if line.starts_with('[') && line != "[Desktop Entry]" {
            if in_desktop_entry {
                break;
            }
            continue;
        }
        if !in_desktop_entry {
            continue;
        }

        if let Some(val) = line.strip_prefix("Type=") {
            app_type = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("Name=") {
            if name.is_none() {
                name = Some(val.to_string());
            }
        } else if let Some(val) = line.strip_prefix("Exec=") {
            exec = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("Icon=") {
            icon = Some(val.to_string());
        } else if line == "NoDisplay=true" {
            no_display = true;
        } else if line == "Hidden=true" {
            hidden = true;
        }
    }

    if no_display || hidden {
        return None;
    }
    if app_type.as_deref() != Some("Application") {
        return None;
    }

    let name = name?;
    // Use the desktop file path as the unique launch key.
    let path_str = path.to_string_lossy().to_string();
    if !seen.insert(path_str.clone()) {
        return None;
    }

    // Resolve the icon to a filesystem path.
    let icon_path = icon.and_then(|i| resolve_icon(&i));

    // Build an Exec string that can be passed to xdg-open or run directly.
    // We store the desktop file path as the "path" so launch_app can use it.
    let _ = exec; // exec is used via the path in launcher.rs
    Some(AppEntry {
        name,
        path: path_str,
        icon_path,
    })
}

/// Resolve an icon name (or absolute path) to a PNG file on disk.
/// Searches hicolor icon theme at preferred sizes, then /usr/share/pixmaps.
#[cfg(target_os = "linux")]
fn resolve_icon(icon: &str) -> Option<String> {
    // If it's already an absolute path, use it directly (if it exists and is PNG).
    if icon.starts_with('/') {
        let p = Path::new(icon);
        if p.exists() {
            // Accept PNG directly; skip SVG since we only handle PNG loading.
            if icon.ends_with(".png") {
                return Some(icon.to_string());
            }
        }
        return None;
    }

    // Preferred sizes to search (larger = better quality for 18px display after downscale).
    let sizes = ["48x48", "64x64", "32x32", "96x96", "128x128", "256x256", "24x24", "16x16"];
    let base_dirs = ["/usr/share/icons/hicolor"];

    for base in &base_dirs {
        for size in &sizes {
            let candidate = format!("{}/{}/apps/{}.png", base, size, icon);
            if Path::new(&candidate).exists() {
                return Some(candidate);
            }
        }
    }

    // Check /usr/share/pixmaps.
    let pixmap = format!("/usr/share/pixmaps/{}.png", icon);
    if Path::new(&pixmap).exists() {
        return Some(pixmap);
    }

    None
}
