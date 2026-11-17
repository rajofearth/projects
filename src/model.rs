//! Data model shared by the scanner, the fixture and the UI.

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
