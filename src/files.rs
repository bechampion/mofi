use std::path::{Path, PathBuf};

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
    pub fn scan(&mut self) {
        use std::os::unix::fs::MetadataExt;
        self.entries.clear();
        let rd = match std::fs::read_dir(&self.cwd) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        let mut dirs = Vec::new();
        let mut files = Vec::new();
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
            let fe = FileEntry {
                name,
                path: entry.path(),
                is_dir: meta.is_dir(),
                size: meta.len(),
                modified,
                owner,
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

/// Open a file with the default system handler.
#[cfg(target_os = "linux")]
pub fn open_file(path: &Path) {
    use std::os::unix::process::CommandExt;
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
