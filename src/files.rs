use std::path::{Path, PathBuf};

/// Where a file entry came from — local directory scan or git-tracked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Entry from the current directory listing.
    Local,
    /// Entry from `git ls-files` (tracked across the whole repo).
    Git,
}

/// A single entry in a directory listing.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    /// Modification time as Unix timestamp (seconds since epoch).
    pub modified: i64,
    /// Owner username.
    pub owner: String,
    /// Where this entry came from.
    pub source: Source,
}

/// State for the single-pane file explorer.
#[derive(Clone, Debug)]
pub struct Pane {
    pub cwd: PathBuf,
    pub entries: Vec<FileEntry>,
    pub selected: usize,
}

impl Pane {
    pub fn new(start: &Path) -> Self {
        let mut p = Pane {
            cwd: start.to_path_buf(),
            entries: Vec::new(),
            selected: 0,
        };
        p.scan();
        p
    }

    /// Re-read the current directory. Directories first (sorted), then files (sorted).
    /// If the directory is inside a git repo, also includes git-tracked files from the
    /// entire repository (with `Source::Git`), excluding entries already present locally.
    pub fn scan(&mut self) {
        use std::collections::HashSet;
        use std::os::unix::fs::MetadataExt;
        self.entries.clear();
        let rd = match std::fs::read_dir(&self.cwd) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut local_paths: HashSet<PathBuf> = HashSet::new();
        for entry in rd.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip hidden files
            if name.starts_with('.') {
                continue;
            }
            let modified = meta.mtime();
            let owner = uid_to_name(meta.uid());
            let path = entry.path();
            local_paths.insert(path.clone());
            let fe = FileEntry {
                name,
                path,
                is_dir: meta.is_dir(),
                size: meta.len(),
                modified,
                owner,
                source: Source::Local,
            };
            if fe.is_dir {
                dirs.push(fe);
            } else {
                files.push(fe);
            }
        }
        dirs.sort_by(|a, b| b.modified.cmp(&a.modified));
        files.sort_by(|a, b| b.modified.cmp(&a.modified));
        self.entries.extend(dirs);
        self.entries.extend(files);

        // If inside a git repo, add tracked files not already in the local listing.
        if let Some(git_root) = find_git_root(&self.cwd) {
            let mut git_dirs: Vec<FileEntry> = Vec::new();
            let mut git_files: Vec<FileEntry> = Vec::new();
            let mut seen_dirs: HashSet<PathBuf> = HashSet::new();

            for path in git_tracked_files(&git_root) {
                // Skip files that are already in the local listing.
                if local_paths.contains(&path) {
                    continue;
                }
                // Skip hidden paths (any component starting with '.')
                if path
                    .components()
                    .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
                {
                    continue;
                }
                let meta = path.metadata().ok();
                let is_dir = path.is_dir();
                let name = if let Ok(rel) = path.strip_prefix(&self.cwd) {
                    rel.to_string_lossy().to_string()
                } else if let Ok(rel) = path.strip_prefix(&git_root) {
                    rel.to_string_lossy().to_string()
                } else {
                    path.to_string_lossy().to_string()
                };
                let fe = FileEntry {
                    name,
                    path: path.clone(),
                    is_dir,
                    size: meta.as_ref().map_or(0, |m| m.len()),
                    modified: meta.as_ref().map_or(0, |m| {
                        use std::os::unix::fs::MetadataExt;
                        m.mtime()
                    }),
                    owner: meta.as_ref().map_or_else(
                        || String::from("?"),
                        |m| {
                            use std::os::unix::fs::MetadataExt;
                            uid_to_name(m.uid())
                        },
                    ),
                    source: Source::Git,
                };
                if is_dir {
                    if !seen_dirs.contains(&path) {
                        seen_dirs.insert(path);
                        git_dirs.push(fe);
                    }
                } else {
                    // Also insert parent directories of git files as git dirs
                    // if they're not already in the local listing.
                    if let Some(parent) = path.parent() {
                        if parent != git_root
                            && parent != self.cwd
                            && !local_paths.contains(parent)
                            && !seen_dirs.contains(parent)
                        {
                            let dir_name = if let Ok(rel) = parent.strip_prefix(&self.cwd) {
                                rel.to_string_lossy().to_string()
                            } else if let Ok(rel) = parent.strip_prefix(&git_root) {
                                rel.to_string_lossy().to_string()
                            } else {
                                parent.to_string_lossy().to_string()
                            };
                            let dir_meta = parent.metadata().ok();
                            seen_dirs.insert(parent.to_path_buf());
                            git_dirs.push(FileEntry {
                                name: dir_name,
                                path: parent.to_path_buf(),
                                is_dir: true,
                                size: 0,
                                modified: dir_meta.as_ref().map_or(0, |m| {
                                    use std::os::unix::fs::MetadataExt;
                                    m.mtime()
                                }),
                                owner: dir_meta.as_ref().map_or_else(
                                    || String::from("?"),
                                    |m| {
                                        use std::os::unix::fs::MetadataExt;
                                        uid_to_name(m.uid())
                                    },
                                ),
                                source: Source::Git,
                            });
                        }
                    }
                    git_files.push(fe);
                }
            }
            git_dirs.sort_by(|a, b| b.modified.cmp(&a.modified));
            git_files.sort_by(|a, b| b.modified.cmp(&a.modified));
            self.entries.extend(git_dirs);
            self.entries.extend(git_files);
        }

        self.selected = 0;
    }

    /// Navigate into the selected entry (if it's a directory).
    /// Returns true if navigation happened.
    pub fn enter_selected(&mut self) -> Option<EnterAction> {
        if let Some(entry) = self.entries.get(self.selected) {
            if entry.is_dir {
                self.cwd = entry.path.clone();
                self.scan();
                Some(EnterAction::NavigatedDir)
            } else {
                Some(EnterAction::OpenFile(entry.path.clone()))
            }
        } else {
            None
        }
    }

    /// Go up to the parent directory. Returns true if we moved.
    pub fn go_up(&mut self) -> bool {
        if let Some(parent) = self.cwd.parent() {
            let old_name = self
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().to_string());
            self.cwd = parent.to_path_buf();
            self.scan();
            // Try to select the directory we came from
            if let Some(name) = old_name {
                if let Some(idx) = self.entries.iter().position(|e| e.name == name) {
                    self.selected = idx;
                }
            }
            true
        } else {
            false
        }
    }
}

pub enum EnterAction {
    NavigatedDir,
    OpenFile(PathBuf),
}

/// Drill-down search: when multi-token query doesn't match any current entries
/// directly, find directories whose name matches early tokens and scan inside
/// them for entries matching the remaining tokens.  Returns ephemeral FileEntry
/// results (one level deep only, to stay fast).
/// Scan a single directory and return children matching the given tokens.
/// Used when the user focuses a directory in the file list and types additional
/// search tokens after a space — those tokens filter the children of the focused dir.
pub fn dir_children(dir: &Path, tokens: &[&str]) -> Vec<FileEntry> {
    const MAX_RESULTS: usize = 200;
    const MAX_DEPTH: usize = 6;

    let dir_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut results = Vec::new();

    fn walk(
        current: &Path,
        root: &Path,
        root_name: &str,
        tokens: &[&str],
        results: &mut Vec<FileEntry>,
        depth: usize,
        max_depth: usize,
        max_results: usize,
    ) {
        use std::os::unix::fs::MetadataExt;
        if depth > max_depth || results.len() >= max_results {
            return;
        }
        let rd = match std::fs::read_dir(current) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        for child in rd.flatten() {
            if results.len() >= max_results {
                return;
            }
            let meta = match child.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let name = child.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let child_path = child.path();
            let is_dir = meta.is_dir();

            // Build relative path from the drill root for matching & display
            let rel = child_path
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().to_string())
                .unwrap_or_else(|_| name.clone());
            let rel_lower = rel.to_lowercase();

            if tokens.iter().all(|t| rel_lower.contains(t)) {
                let display_name = format!("{}/{}", root_name, rel);
                results.push(FileEntry {
                    name: display_name,
                    path: child_path.clone(),
                    is_dir,
                    size: meta.len(),
                    modified: meta.mtime(),
                    owner: uid_to_name(meta.uid()),
                    source: Source::Local,
                });
            }

            // Recurse into subdirectories
            if is_dir {
                walk(
                    &child_path,
                    root,
                    root_name,
                    tokens,
                    results,
                    depth + 1,
                    max_depth,
                    max_results,
                );
            }
        }
    }

    walk(
        dir,
        dir,
        &dir_name,
        tokens,
        &mut results,
        0,
        MAX_DEPTH,
        MAX_RESULTS,
    );
    results.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(b.modified.cmp(&a.modified)));
    results
}

/// Returns true if the file has a supported image extension.
#[allow(dead_code)]
pub fn is_image(entry: &FileEntry) -> bool {
    if entry.is_dir {
        return false;
    }
    let name = entry.name.to_lowercase();
    if let Some(ext) = name.rsplit('.').next() {
        matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg")
    } else {
        false
    }
}

/// Pick a Nerd Font glyph for a file entry.
pub fn glyph_for_file(entry: &FileEntry) -> &'static str {
    if entry.is_dir {
        if entry.source == Source::Git {
            return "\u{E5FD}"; // nf-custom-folder_github
        }
        return "\u{F07B}"; // nf-fa-folder
    }
    let name = entry.name.to_lowercase();
    // By extension
    if let Some(ext) = name.rsplit('.').next() {
        match ext {
            "rs" => return "\u{E7A8}",                           // nf-dev-rust
            "py" => return "\u{E73C}",                           // nf-dev-python
            "js" => return "\u{E74E}",                           // nf-dev-javascript
            "ts" => return "\u{E628}",                           // nf-seti-typescript
            "jsx" | "tsx" => return "\u{E7BA}",                  // nf-dev-react
            "go" => return "\u{E627}",                           // nf-seti-go
            "c" | "h" => return "\u{E61E}",                      // nf-seti-c
            "cpp" | "cc" | "cxx" | "hpp" => return "\u{E61D}",   // nf-seti-cpp
            "java" => return "\u{E738}",                         // nf-dev-java
            "rb" => return "\u{E739}",                           // nf-dev-ruby
            "sh" | "bash" | "zsh" | "fish" => return "\u{F489}", // nf-md-console_line
            "toml" | "yaml" | "yml" | "json" | "xml" | "ini" | "cfg" => return "\u{E615}", // nf-seti-config
            "md" | "txt" | "rst" => return "\u{F0219}", // nf-md-file_document
            "pdf" => return "\u{F1C1}",                 // nf-fa-file_pdf_o
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "svg" | "webp" => return "\u{F03E}", // nf-fa-image
            "mp3" | "wav" | "flac" | "ogg" | "m4a" => return "\u{F001}", // nf-fa-music
            "mp4" | "mkv" | "avi" | "mov" | "webm" => return "\u{F008}", // nf-fa-film
            "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" => return "\u{F1C6}", // nf-fa-file_archive_o
            "lock" => return "\u{F023}",                                             // nf-fa-lock
            "log" => return "\u{F0331}", // nf-md-file_chart
            "css" | "scss" | "sass" | "less" => return "\u{E749}", // nf-dev-css3
            "html" | "htm" => return "\u{E736}", // nf-dev-html5
            "sql" | "db" | "sqlite" => return "\u{F1C0}", // nf-fa-database
            _ => {}
        }
    }
    // Special names
    if name == "makefile" || name == "cmakelists.txt" {
        return "\u{E673}"; // nf-seti-makefile
    }
    if name == "dockerfile" {
        return "\u{E7B0}"; // nf-dev-docker
    }
    if name.starts_with("license") {
        return "\u{F0219}"; // nf-md-file_document
    }
    "\u{F15B}" // nf-fa-file  (generic)
}

/// Format a file size for display.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} K", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} M", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} G", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Format a Unix timestamp as "Jan 05 14:30" (or "Jan 05  2024" if older than ~6 months).
pub fn format_time(epoch: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    // Convert epoch to broken-down local time via libc.
    let t = epoch as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };

    let mon = MONTHS[tm.tm_mon as usize % 12];
    let day = tm.tm_mday;

    // If modified within the last ~6 months, show time; otherwise show year.
    let now = unsafe { libc::time(std::ptr::null_mut()) } as i64;
    let six_months = 180 * 24 * 3600;
    if (now - epoch).abs() < six_months {
        format!("{} {:2} {:02}:{:02}", mon, day, tm.tm_hour, tm.tm_min)
    } else {
        format!("{} {:2}  {}", mon, day, 1900 + tm.tm_year)
    }
}

/// Look up a username from a UID via getpwuid. Falls back to the numeric UID.
fn uid_to_name(uid: u32) -> String {
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return uid.to_string();
    }
    let cstr = unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) };
    cstr.to_string_lossy().into_owned()
}

/// Find the root of the git repository containing `dir`, if any.
pub fn find_git_root(dir: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        return None;
    }
    Some(PathBuf::from(root))
}

/// Get all tracked files in the git repo at `git_root` as absolute paths.
fn git_tracked_files(git_root: &Path) -> Vec<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "--full-name"])
        .current_dir(git_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|line| git_root.join(line))
            .collect(),
        _ => Vec::new(),
    }
}

/// Returns true if the file extension indicates a text/code file that should open in nvim.
fn is_text_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    // Special filenames
    if matches!(
        name.as_str(),
        "makefile" | "cmakelists.txt" | "dockerfile" | "justfile" | "rakefile" | "gemfile"
    ) || name.starts_with("license")
        || name.starts_with("readme")
        || name.starts_with(".env")
    {
        return true;
    }
    if let Some(ext) = name.rsplit('.').next() {
        matches!(
            ext,
            "rs" | "py"
                | "js"
                | "ts"
                | "jsx"
                | "tsx"
                | "go"
                | "c"
                | "h"
                | "cpp"
                | "cc"
                | "cxx"
                | "hpp"
                | "java"
                | "rb"
                | "sh"
                | "bash"
                | "zsh"
                | "fish"
                | "toml"
                | "yaml"
                | "yml"
                | "json"
                | "xml"
                | "ini"
                | "cfg"
                | "md"
                | "txt"
                | "rst"
                | "css"
                | "scss"
                | "sass"
                | "less"
                | "html"
                | "htm"
                | "sql"
                | "lua"
                | "vim"
                | "el"
                | "clj"
                | "ex"
                | "exs"
                | "erl"
                | "hs"
                | "ml"
                | "mli"
                | "nix"
                | "hcl"
                | "proto"
                | "graphql"
                | "gql"
                | "svelte"
                | "vue"
                | "astro"
                | "swift"
                | "kt"
                | "kts"
                | "gradle"
                | "r"
                | "jl"
                | "zig"
                | "dart"
                | "php"
                | "pl"
                | "pm"
                | "t"
                | "rkt"
                | "scm"
                | "lisp"
                | "conf"
                | "env"
                | "lock"
                | "log"
                | "csv"
                | "tsv"
                | "diff"
                | "patch"
                | "rego"
                | "ps1"
                | "psm1"
                | "psd1"
                | "bat"
                | "cmd"
                | "tex"
                | "bib"
                | "org"
                | "adoc"
                | "typ"
                | "jsonc"
                | "json5"
                | "dhall"
                | "cue"
                | "pkl"
                | "starlark"
                | "bzl"
                | "bazel"
                | "tfvars"
                | "sbt"
                | "sc"
                | "scala"
                | "groovy"
                | "m"
                | "mm"
                | "plist"
                | "mk"
                | "mak"
                | "service"
                | "timer"
                | "socket"
                | "desktop"
                | "editorconfig"
                | "gitignore"
                | "gitattributes"
                | "dockerignore"
                | "prettierrc"
                | "eslintrc"
        )
    } else {
        false
    }
}

/// Find the first nvim Unix socket in /run/user/<uid>/.
fn find_nvim_socket() -> Option<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let run_dir = PathBuf::from(format!("/run/user/{}", uid));
    let rd = std::fs::read_dir(&run_dir).ok()?;
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("nvim.") && name.ends_with(".0") {
            return Some(entry.path());
        }
    }
    None
}

/// Open a file with the default system handler.
/// Text files are opened in the existing nvim instance (--remote-tab) if one is running.
#[cfg(target_os = "linux")]
pub fn open_file(path: &Path) {
    use std::os::unix::process::CommandExt;

    // Try opening text files in an existing nvim instance.
    if is_text_file(path) {
        if let Some(socket) = find_nvim_socket() {
            let result = std::process::Command::new("nvim")
                .arg("--server")
                .arg(&socket)
                .arg("--remote-tab")
                .arg(path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .status();
            if let Ok(s) = result {
                if s.success() {
                    return;
                }
            }
        }
    }

    // Fallback: xdg-open
    unsafe {
        let _ = std::process::Command::new("xdg-open")
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .pre_exec(|| {
                libc::setsid();
                Ok(())
            })
            .spawn();
    }
}

#[cfg(target_os = "macos")]
pub fn open_file(path: &Path) {
    let _ = std::process::Command::new("open").arg(path).spawn();
}
