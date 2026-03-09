use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ── Persistence path ──────────────────────────────────────────────────────────

fn store_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share")
        })
        .join("mofi")
        .join("frecency.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Frecency score ────────────────────────────────────────────────────────────
//
// For each past launch timestamp t, contribute:
//
//   weight(t) = 1.0 / age_hours.max(1.0)
//
// where age_hours = (now - t) / 3600.
//
// This gives recent launches much higher weight (last hour → 1.0/1 = 1.0,
// last day → ~0.04, last week → ~0.006) — classic frecency decay.
// We keep at most MAX_TIMESTAMPS per key and discard entries older than
// MAX_AGE_SECS to bound memory and keep scores meaningful.

const MAX_TIMESTAMPS: usize = 100;
const MAX_AGE_SECS: u64 = 90 * 24 * 3600; // 90 days

fn compute_score(timestamps: &[u64], now: u64) -> f64 {
    timestamps
        .iter()
        .filter(|&&t| now.saturating_sub(t) <= MAX_AGE_SECS)
        .map(|&t| {
            let age_secs = now.saturating_sub(t) as f64;
            let age_hours = (age_secs / 3600.0).max(1.0);
            1.0 / age_hours
        })
        .sum()
}

// ── Store ─────────────────────────────────────────────────────────────────────

/// Persistent frecency store.  Keyed by item display name (app name or pass
/// entry path).  Clipboard items are deliberately excluded — their identity
/// changes every paste so frecency tracking doesn't apply.
#[derive(Default, Serialize, Deserialize)]
pub struct FrecencyStore {
    /// Map from item key → list of launch timestamps (unix seconds), newest last.
    data: HashMap<String, Vec<u64>>,
}

impl FrecencyStore {
    /// Load from disk.  Returns an empty store on any error (first run, etc.).
    pub fn load() -> Self {
        let path = store_path();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Self::default(),
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// Persist to disk.  Silently ignores errors (frecency is best-effort).
    pub fn save(&self) {
        let path = store_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec(self) {
            let _ = std::fs::write(&path, json);
        }
    }

    /// Record a launch of item `key` at the current time, then save.
    pub fn record(&mut self, key: &str) {
        let now = now_secs();
        let entry = self.data.entry(key.to_string()).or_default();
        entry.push(now);
        // Prune to MAX_TIMESTAMPS most-recent entries.
        if entry.len() > MAX_TIMESTAMPS {
            let drain_count = entry.len() - MAX_TIMESTAMPS;
            entry.drain(..drain_count);
        }
        self.save();
    }

    /// Return the frecency score for `key`.  Returns 0.0 for unknown keys.
    #[allow(dead_code)]
    pub fn score(&self, key: &str) -> f64 {
        let now = now_secs();
        self.data
            .get(key)
            .map(|ts| compute_score(ts, now))
            .unwrap_or(0.0)
    }

    /// Return scores for a slice of keys in one pass (avoids repeated now() calls).
    pub fn scores(&self, keys: &[&str]) -> Vec<f64> {
        let now = now_secs();
        keys.iter()
            .map(|k| {
                self.data
                    .get(*k)
                    .map(|ts| compute_score(ts, now))
                    .unwrap_or(0.0)
            })
            .collect()
    }
}
