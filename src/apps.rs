use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Represents a discovered macOS application bundle.
#[derive(Clone, Debug)]
pub struct AppEntry {
    pub name: String,
    pub path: String,
}

/// Scan common app directories and return all .app bundles found.
/// Scans one level deep so apps inside sub-folders (Utilities, Setapp, etc.)
/// are included, without ever descending inside a .app bundle itself.
pub fn discover_apps() -> Vec<AppEntry> {
    let home = dirs::home_dir().unwrap_or_default();

    let search_dirs = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        home.join("Applications"),
    ];

    let mut seen: HashSet<String> = HashSet::new();
    let mut apps = Vec::new();

    for dir in &search_dirs {
        scan_dir(dir, &mut seen, &mut apps);
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

/// Read one directory level. Any .app found is collected; any plain directory
/// found is scanned one more level down (so Utilities/, Setapp/, etc. work).
fn scan_dir(dir: &Path, seen: &mut HashSet<String>, apps: &mut Vec<AppEntry>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());

        if ext == Some("app") {
            collect(&path, seen, apps);
        } else if path.is_dir() {
            // One level of subdirectory (Utilities, Setapp, etc.)
            if let Ok(sub) = std::fs::read_dir(&path) {
                for sub_entry in sub.flatten() {
                    let sub_path = sub_entry.path();
                    if sub_path.extension().and_then(|e| e.to_str()) == Some("app") {
                        collect(&sub_path, seen, apps);
                    }
                }
            }
        }
    }
}

fn collect(path: &Path, seen: &mut HashSet<String>, apps: &mut Vec<AppEntry>) {
    let path_str = path.to_string_lossy().to_string();
    if !seen.insert(path_str.clone()) {
        return;
    }
    let name = match path.file_stem().and_then(|s| s.to_str()) {
        Some(n) => n.to_string(),
        None => return,
    };
    apps.push(AppEntry { name, path: path_str });
}
