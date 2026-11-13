//! Data model shared by the scanner, the fixture and the UI.


/// Commit activity buckets from git history, shared by the calendar heatmap and the
/// weekday-hour heatmap. Empty when there is no git history.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Activity {
    /// Commits per day, oldest first: index 0 is 363 days ago, index 363 is today.
    pub per_day: Vec<u32>,
    /// Commits by weekday and local hour: index `weekday * 24 + hour`, weekday 0 = Monday.
    pub by_hour: Vec<u32>,
}

impl Activity {
    pub fn total(&self) -> u64 {
        self.per_day.iter().map(|&n| n as u64).sum()
    }
}

/// One folder under the projects root.
#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub cat: &'static str,
    pub stack: String,
    /// Bytes on disk, split into buckets.
    pub bytes: u64,
    pub files: u64,
    pub loc: u64,
    /// Bytes that are real source / data (not build junk).
    pub src: u64,
    pub nm: u64,
    pub target: u64,
    pub other: u64,
    /// Lines per extension, hand-written code only.
    pub langs: Vec<(String, u64)>,
    pub git: bool,
    pub commits: u64,
    /// Days since the last commit; `None` when there is no git history.
    pub days: Option<u64>,
    pub dirty: u64,
    /// Per-day and weekday-hour commit buckets; `None` without git history.
    pub activity: Option<Activity>,
}

/// A scan event, sent from the scanner thread to the UI thread.
#[derive(Debug)]
pub enum ScanEvent {
    Start { total: usize },
    Project(Box<Project>),
    Done { seconds: f64 },
    Failed(String),
}

// Category palette, matched to the mock dashboard.

// Language colours, matched to GitHub's linguistic palette.

// ---- formatting helpers ----

pub fn fmt(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(b as char);
    }
    out
}

pub fn gib(b: u64) -> f64 {
    b as f64 / 1073741824.0
}

/// 1.2 GiB / 340 MiB style size.
pub fn gb(b: u64) -> String {
    let g = gib(b);
    if g >= 1.0 {
        format!("{g:.1} GiB")
    } else {
        format!("{:.0} MiB", b as f64 / 1048576.0)
    }
}

/// Truncate a name to roughly fit a width, the way the mock does.
pub fn fit(name: &str, max_chars: usize) -> String {
    if name.chars().count() <= max_chars {
        name.to_string()
    } else {
        let cut: String = name.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
