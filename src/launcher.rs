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

/// Launch an app by its .app bundle path using the `open` command.
pub fn launch_app(path: &str) {
    let _ = std::process::Command::new("open").arg(path).spawn();
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
