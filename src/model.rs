//! Data model shared by the scanner, the fixture and the UI.

use rune::Color;

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

impl Project {
    /// The bucket a byte count belongs to, for the composition chart.
    pub fn build_junk(&self) -> u64 {
        self.nm + self.target + self.other
    }
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
pub const C_WEB: &str = "#4f8cff";
pub const C_AI: &str = "#a855f7";
pub const C_NATIVE: &str = "#22c55e";
pub const C_MOBILE: &str = "#f59e0b";
pub const C_CLIENT: &str = "#06b6d4";
pub const C_PORTFOLIO: &str = "#ec4899";
pub const C_CLONE: &str = "#ef4444";
pub const C_LEARN: &str = "#64748b";
pub const C_EXP: &str = "#8b5cf6";
pub const C_TOOL: &str = "#14b8a6";
pub const C_EXT: &str = "#eab308";
pub const C_DOCS: &str = "#78716c";
pub const C_DESK: &str = "#34d399";
pub const C_UTIL: &str = "#475569";

pub fn cat_color(cat: &str) -> Color {
    let hex = match cat {
        "Web App" => C_WEB,
        "AI / ML" => C_AI,
        "Native Desktop" => C_NATIVE,
        "Mobile" => C_MOBILE,
        "Client Site" => C_CLIENT,
        "Portfolio" => C_PORTFOLIO,
        "Clone / Fork" => C_CLONE,
        "Learning" => C_LEARN,
        "Experiment" => C_EXP,
        "AI Tooling" => C_TOOL,
        "Browser Ext" => C_EXT,
        "Docs / Research" => C_DOCS,
        "Desktop App" => C_DESK,
        "Tooling" => C_UTIL,
        _ => C_LEARN,
    };
    rune::color::hex(hex).unwrap_or(Color { r: 0.5, g: 0.5, b: 0.5, a: 1.0 })
}

// Language colours, matched to GitHub's linguistic palette.
fn lang_color(name: &str) -> Color {
    let hex = match name {
        "TSX" => "#4f8cff",
        "TypeScript" => "#3178c6",
        "C++" => "#f34b7d",
        "C" => "#94a3b8",
        "JavaScript" => "#f1e05a",
        "Python" => "#3572A5",
        "Rust" => "#dea584",
        "Kotlin" => "#a97bff",
        "HTML" => "#e34c26",
        "CSS" => "#8b5cf6",
        "PHP" => "#4F5D95",
        "Swift" => "#F05138",
        "Go" => "#00ADD8",
        "C#" => "#178600",
        "Java" => "#b07219",
        "Shell" => "#89e051",
        "Vue" => "#41b883",
        "Markdown" => "#78716c",
        _ => "#64748b",
    };
    rune::color::hex(hex).unwrap_or(Color { r: 0.5, g: 0.5, b: 0.5, a: 1.0 })
}

pub fn lang_color_or(name: &str) -> Color {
    lang_color(name)
}

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
