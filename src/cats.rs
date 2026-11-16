//! The fix-storage vocabulary applied to a folder of projects: what is on disk splits into
//! four categories (source, dependencies, build output, everything else), and every item gets
//! a verdict — regenerates, stale, git history or yours — with a one-line reason.
//!
//! Ported from the upstream interface's `categories.ts` and `classify.ts` (MIT), with this
//! app's data in place of a Mac's: no Photos library or Trash here, but the same rules and the
//! same words.

use crate::model::Project;

pub type Rgb = [u8; 3];

/// The four buckets every byte under the projects root lands in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Cat {
    Source,
    Deps,
    Build,
    Other,
}

/// safe:   regenerates on its own; clearing loses nothing.
/// review: yours to decide; shown, never cleared for you.
/// keep:   your work; never offered for clearing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Safe,
    Review,
    Keep,
}

pub struct Category {
    pub id: Cat,
    pub name: &'static str,
    pub color: Rgb,
    pub tier: Tier,
    pub blurb: &'static str,
}

/// Order is the order of the segments in the meter and of the legend, row by row.
pub const CATEGORIES: [Category; 4] = [
    Category {
        id: Cat::Source,
        name: "Source",
        color: [34, 211, 238],
        tier: Tier::Keep,
        blurb: "Hand-written code and the files beside it",
    },
    Category {
        id: Cat::Deps,
        name: "Dependencies",
        color: [34, 197, 94],
        tier: Tier::Safe,
        blurb: "node_modules. npm, pnpm or bun install brings it back.",
    },
    Category {
        id: Cat::Build,
        name: "Build output",
        color: [249, 115, 22],
        tier: Tier::Safe,
        blurb: "target, dist and caches. Rebuilds from source.",
    },
    Category {
        id: Cat::Other,
        name: "Other",
        color: [139, 92, 246],
        tier: Tier::Review,
        blurb: ".git history, media and everything else a project keeps",
    },
];

impl Cat {
    pub fn category(self) -> &'static Category {
        CATEGORIES
            .iter()
            .find(|c| c.id == self)
            .unwrap_or(&CATEGORIES[0])
    }

    pub fn color(self) -> Rgb {
        self.category().color
    }

    pub fn name(self) -> &'static str {
        self.category().name
    }

    /// A short stable key for this category, for element keys and tooltips.
    pub fn label(self) -> &'static str {
        match self {
            Cat::Source => "source",
            Cat::Deps => "deps",
            Cat::Build => "build",
            Cat::Other => "other",
        }
    }

    /// Bytes of one project that fall in this category.
    pub fn bytes(self, p: &Project) -> u64 {
        match self {
            Cat::Source => p.src,
            Cat::Deps => p.nm,
            Cat::Build => p.target,
            Cat::Other => p.other,
        }
    }
}

pub fn tier_note(tier: Tier) -> &'static str {
    match tier {
        Tier::Safe => "safe to clear",
        Tier::Review => "review first",
        Tier::Keep => "",
    }
}

// ---- labels -----------------------------------------------------------------

/// regenerates: a command brings it back; removing it loses nothing.
/// stale:       nothing has touched it in a long time; your call.
/// yours:       your data, in use; shown for context only.
/// git history: git owns it; removing it loses your commits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Regenerates,
    Stale,
    Yours,
    GitHistory,
}

impl Verdict {
    /// What the row says about the folder, in the reader's terms. Each tag is the state
    /// the bytes are in, not a judgement about them: a regenerate folder is one whose
    /// contents come back from a command, stale is one nobody has touched, and git history
    /// is one only git reads.
    pub fn tag(self) -> &'static str {
        match self {
            Verdict::Regenerates => "regenerates",
            Verdict::Stale => "stale",
            Verdict::Yours => "",
            Verdict::GitHistory => "git history",
        }
    }

    pub fn color(self) -> Rgb {
        match self {
            Verdict::Regenerates => [74, 222, 128],
            Verdict::Stale => [251, 191, 36],
            Verdict::Yours => [120, 120, 120],
            Verdict::GitHistory => [124, 138, 160],
        }
    }
}

#[derive(Debug)]
pub struct Label {
    pub verdict: Verdict,
    /// Short, lowercase reason, e.g. "npm install brings it back".
    pub reason: String,
}

impl Label {
    /// The verdict as a sentence opener, so a reason can follow it: "rebuildable · npm
    /// install brings it back". Yours has nothing to add, so it returns an empty string and
    /// callers skip the whole clause.
    pub fn tag_line(&self) -> &'static str {
        match self.verdict {
            Verdict::Regenerates => "rebuildable",
            Verdict::Stale => "not touched in a while",
            Verdict::Yours => "",
            Verdict::GitHistory => "git history",
        }
    }
}

/// The bytes come back from a command: installing or building restores this folder.
fn regen(reason: impl Into<String>) -> Label {
    Label {
        verdict: Verdict::Regenerates,
        reason: reason.into(),
    }
}

/// Nobody has touched this in a long time, and nothing here needs it to be current.
fn stale(reason: impl Into<String>) -> Label {
    Label {
        verdict: Verdict::Stale,
        reason: reason.into(),
    }
}

fn yours(reason: impl Into<String>) -> Label {
    Label {
        verdict: Verdict::Yours,
        reason: reason.into(),
    }
}

/// Build caches that rebuild themselves, one thing as far as the reader is concerned.
const BUILD_CACHES: &[&str] = &[
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".turbo",
    ".parcel-cache",
    ".svelte-kit",
    ".angular",
    ".expo",
    "coverage",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".gradle",
    ".dart_tool",
];

/// Folders that are one thing as far as the user is concerned: a culprit search stops at
/// them instead of digging into their internals.
pub fn is_whole(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "node_modules"
        || lower == ".git"
        || lower == ".venv"
        || lower == "venv"
        || BUILD_CACHES.contains(&lower.as_str())
        || [".app", ".bundle", ".framework", ".appx"]
            .iter()
            .any(|ext| lower.ends_with(ext))
}

/// Compact age for a column: "today", "3d", "7mo", "2y".
pub fn short_age(days: u64) -> String {
    if days < 1 {
        return "today".to_string();
    }
    if days < 30 {
        return format!("{days}d");
    }
    if days < 365 {
        return format!("{}mo", (days as f64 / 30.4) as u64);
    }
    format!("{}y", days / 365)
}

/// "3 days", "7 months", "2 years".
pub fn age(days: u64) -> String {
    if days < 1 {
        return "today".to_string();
    }
    if days < 30 {
        return format!("{days} {}", if days == 1 { "day" } else { "days" });
    }
    let months = (days as f64 / 30.4) as u64;
    if months < 12 {
        return format!("{months} {}", if months == 1 { "month" } else { "months" });
    }
    let years = days / 365;
    format!("{years} {}", if years == 1 { "year" } else { "years" })
}

/// What a folder or file inside a project probably is, from its name and when git last moved.
pub fn classify(name: &str, dir: bool, days: Option<u64>) -> Label {
    let lower = name.to_ascii_lowercase();
    let touched = match days {
        Some(d) if d > 30 => format!(" · not touched in {}", age(d)),
        _ => String::new(),
    };
    if lower == "node_modules" {
        return regen(format!("npm install brings it back{touched}"));
    }
    if lower == ".git" {
        return Label {
            verdict: Verdict::GitHistory,
            reason: "git manages this · removing it loses your commits".to_string(),
        };
    }
    if BUILD_CACHES.contains(&lower.as_str()) && dir {
        return stale(format!("rebuilds from source{touched}"));
    }
    if lower == ".venv" || lower == "venv" || lower == "env" {
        return stale(format!("pip install recreates it{touched}"));
    }
    if lower == "vendor" || lower == "pods" {
        return regen(format!("install brings it back{touched}"));
    }
    if let Some(d) = days {
        if d > 365 {
            return stale(format!("not touched in {}", age(d)));
        }
        if d > 180 {
            return stale(format!("not touched in {}", age(d)));
        }
        return yours(format!("changed {}", if d < 1 { "today".to_string() } else { format!("{} ago", age(d)) }));
    }
    yours("")
}

/// What a whole project probably is, from how it is built and when it was last committed to.
pub fn classify_project(p: &Project, cat: Cat) -> Label {
    match cat {
        Cat::Deps => regen("npm install brings it back"),
        Cat::Build => stale("rebuilds from source"),
        Cat::Source => match p.days {
            Some(d) if d > 365 => stale(format!("no commit in {}", age(d))),
            _ => yours(""),
        },
        Cat::Other => Label {
            verdict: Verdict::GitHistory,
            reason: "git history and media".to_string(),
        },
    }
}

/// A project's row keeps the verdict of its month-long silence, not of the category it is
/// listed under: the "All" view ranks whole projects.
pub fn classify_row(p: &Project) -> Label {
    let rebuildable = p.nm + p.target + p.other.min(p.nm + p.target);
    if let Some(d) = p.days.filter(|d| *d > 365) {
        return stale(format!("no commit in {}", age(d)));
    }
    if p.bytes > 0 && rebuildable * 3 >= p.bytes * 2 {
        return regen("install and build bring it back");
    }
    yours("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(nm: u64, target: u64, other: u64, src: u64, days: Option<u64>) -> Project {
        Project {
            name: "p".into(),
            cat: "Web App",
            stack: String::new(),
            bytes: nm + target + other + src,
            files: 0,
            loc: 0,
            src,
            nm,
            target,
            other,
            langs: Vec::new(),
            git: days.is_some(),
            commits: 0,
            days,
            dirty: 0,
        }
    }

    #[test]
    fn node_modules_regenerates_and_build_output_is_stale() {
        assert_eq!(classify("node_modules", true, None).verdict, Verdict::Regenerates);
        assert_eq!(classify("target", true, None).verdict, Verdict::Stale);
        assert_eq!(classify("dist", true, None).verdict, Verdict::Stale);
        assert_eq!(classify("src", true, None).verdict, Verdict::Yours);
    }

    #[test]
    fn git_history_says_what_it_costs() {
        let label = classify(".git", true, None);
        assert_eq!(label.verdict, Verdict::GitHistory);
        assert!(label.reason.contains("commits"), "{}", label.reason);
    }

    #[test]
    fn a_year_of_silence_makes_a_folder_stale() {
        let label = classify("notes", true, Some(400));
        assert_eq!(label.verdict, Verdict::Stale);
        assert!(label.reason.contains("1 year"), "{}", label.reason);
    }

    #[test]
    fn short_ages_read_like_the_original() {
        assert_eq!(short_age(0), "today");
        assert_eq!(short_age(3), "3d");
        assert_eq!(short_age(200), "6mo");
        assert_eq!(short_age(800), "2y");
    }

    #[test]
    fn a_project_mostly_of_dependencies_regenerates() {
        let p = project(900, 100, 0, 10, Some(12));
        assert_eq!(classify_row(&p).verdict, Verdict::Regenerates);
        let own = project(10, 10, 10, 900, Some(12));
        assert_eq!(classify_row(&own).verdict, Verdict::Yours);
        let stale = project(0, 0, 0, 900, Some(500));
        assert_eq!(classify_row(&stale).verdict, Verdict::Stale);
    }

    #[test]
    fn the_four_categories_partition_a_project() {
        let p = project(40, 30, 20, 10, None);
        let sum: u64 = CATEGORIES.iter().map(|c| c.id.bytes(&p)).sum();
        assert_eq!(sum, p.bytes, "every byte lands in exactly one category");
    }
}