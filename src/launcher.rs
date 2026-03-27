use crate::apps::AppEntry;
use crate::clipboard::ClipboardEntry;
use crate::frecency::FrecencyStore;
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

    /// Filter and rank items by query, boosted by frecency.
    /// Returns indices into `items` in ranked order.
    ///
    /// - Empty query: sort purely by frecency score (most-used first).
    /// - Non-empty query: fuzzy-match as before, but add a frecency bonus so
    ///   frequently-used items rise within ties (bonus is capped so a poor
    ///   fuzzy match never beats a good one).
    pub fn search(&self, query: &str, items: &[LaunchItem], frecency: &FrecencyStore) -> Vec<usize> {
        let keys: Vec<&str> = items
            .iter()
            .map(|i| match i {
                LaunchItem::App(a)  => a.name.as_str(),
                LaunchItem::Pass(p) => p.name.as_str(),
                LaunchItem::Clip(_) => "", // clipboard items not frecency-tracked
            })
            .collect();

        let frec_scores = frecency.scores(&keys);

        if query.is_empty() {
            // Sort by frecency descending; stable so equal scores keep
            // their original (alphabetical) order.
            let mut indices: Vec<usize> = (0..items.len()).collect();
            indices.sort_by(|&a, &b| {
                frec_scores[b]
                    .partial_cmp(&frec_scores[a])
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            return indices;
        }

        // Bonus = frecency_score * FRECENCY_WEIGHT added to the fuzzy score.
        // SkimMatcher scores are roughly 0–500; a bonus up to ~50 lets
        // frequent items rise within ties without displacing strong fuzzy hits.
        const FRECENCY_WEIGHT: f64 = 20.0;

        let mut scored: Vec<(i64, usize)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                self.matcher
                    .fuzzy_match(&item.display_name(), query)
                    .map(|score| {
                        let bonus = (frec_scores[i] * FRECENCY_WEIGHT) as i64;
                        (score + bonus, i)
                    })
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, i)| i).collect()
    }
}

/// Launch an app by its .app bundle path using the `open` command.
pub fn launch_app(path: &str) {
    let _ = std::process::Command::new("open").arg(path).spawn();
}

/// Run a shell command in the background.
pub fn run_shell_command(cmd: &str) {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return;
    }
    let _ = std::process::Command::new("sh")
        .args(["-lc", cmd])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Copy text to clipboard only (no auto-paste).
pub fn paste_text(text: &str) {
    use std::io::Write;

    if let Ok(mut child) = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

/// Copy PNG image bytes to clipboard.
pub fn paste_image_png(png_bytes: &[u8]) {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG};
    use objc2_foundation::NSData;

    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let data = NSData::with_bytes(png_bytes);
        let _ = pb.setData_forType(Some(&data), NSPasteboardTypePNG);
    }
}
