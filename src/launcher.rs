use crate::apps::AppEntry;
use crate::clipboard::ClipboardEntry;
use crate::pass::PassEntry;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

/// A result item shown in the launcher list.
#[derive(Clone, Debug)]
pub enum LaunchItem {
    App(AppEntry),
    Clip(ClipboardEntry),
    Pass(PassEntry),
}

impl LaunchItem {
    /// Primary text shown in the row.
    pub fn display_name(&self) -> String {
        match self {
            LaunchItem::App(a) => a.name.clone(),
            LaunchItem::Clip(c) => c.preview(),
            LaunchItem::Pass(p) => p.name.clone(),
        }
    }

    /// Secondary text shown below the name (subtitle row).
    pub fn subtitle(&self) -> Option<String> {
        match self {
            LaunchItem::App(_) => None,
            LaunchItem::Clip(c) => Some(c.subtitle()),
            LaunchItem::Pass(_) => None,
        }
    }
}

pub struct Launcher {
    matcher: SkimMatcherV2,
}

impl Launcher {
    pub fn new() -> Self {
        Self {
            matcher: SkimMatcherV2::default(),
        }
    }

    /// Filter and rank items by query. Returns indices into `items` in score order.
    pub fn search(&self, query: &str, items: &[LaunchItem]) -> Vec<usize> {
        if query.is_empty() {
            return (0..items.len()).collect();
        }

        let mut scored: Vec<(i64, usize)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                self.matcher
                    .fuzzy_match(&item.display_name(), query)
                    .map(|score| (score, i))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, i)| i).collect()
    }
}

/// Launch an application.
///
/// - macOS: uses `open <path>` with the .app bundle path.
/// - Linux: the path is a .desktop file path; we try `gtk-launch <id>` first,
///   then fall back to parsing the Exec= field and running it directly.
pub fn launch_app(path: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }

    #[cfg(target_os = "linux")]
    {
        launch_app_linux(path);
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
    }
}

#[cfg(target_os = "linux")]
fn launch_app_linux(desktop_path: &str) {
    use std::path::Path;

    // gtk-launch accepts either the basename of the .desktop file (without
    // the .desktop suffix) or the full absolute path on newer versions.
    // Try basename first (most portable).
    let p = Path::new(desktop_path);
    let basename = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");

    if !basename.is_empty() {
        let status = std::process::Command::new("gtk-launch")
            .arg(basename)
            .spawn();
        if status.is_ok() {
            return;
        }
    }

    // Fallback: parse Exec= from the .desktop file and run the command directly.
    if let Ok(content) = std::fs::read_to_string(desktop_path) {
        let mut exec_line: Option<String> = None;
        let mut in_entry = false;
        for line in content.lines() {
            let line = line.trim();
            if line == "[Desktop Entry]" {
                in_entry = true;
                continue;
            }
            if line.starts_with('[') {
                if in_entry {
                    break;
                }
                continue;
            }
            if !in_entry {
                continue;
            }
            if let Some(val) = line.strip_prefix("Exec=") {
                exec_line = Some(val.to_string());
                break;
            }
        }

        if let Some(exec) = exec_line {
            // Strip field codes (%u, %U, %f, %F, %i, %c, %k …)
            let cleaned: String = exec
                .split_whitespace()
                .filter(|tok| !tok.starts_with('%'))
                .collect::<Vec<_>>()
                .join(" ");

            if !cleaned.is_empty() {
                let mut parts = cleaned.split_whitespace();
                if let Some(bin) = parts.next() {
                    let args: Vec<&str> = parts.collect();
                    let _ = std::process::Command::new(bin).args(args).spawn();
                }
            }
        }
    }
}

/// Copy text to the system clipboard.
pub fn paste_text(text: &str) {
    crate::clipboard::write_clipboard(text);
}
