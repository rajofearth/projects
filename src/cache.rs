//! Disk cache for the scan and the per-project inspection.
//!
//! On startup the app loads the last scan straight from this file, so the window shows real
//! data at once. The background scan then recomputes a cheap fingerprint for every folder and
//! only walks, asks git, and inspects the ones that actually changed. Everything else is
//! carried over byte-for-byte from the cache.
//!
//! The file lives beside the templates in `%APPDATA%\projects` (or `%USERPROFILE%` when
//! `APPDATA` is not set). A version and the projects root are stored with it; a mismatch is
//! treated as a cold start rather than feeding old data to a different folder.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::detect::{Commit, Info};
use crate::inspect::Inspector;
use crate::model::Project;

const VERSION: u32 = 1;

/// What the scanner needs from the cache: the last fingerprint and row of each folder, by
/// folder name. Shared with the scan thread for the whole run.
pub type ScanCache = Arc<HashMap<String, (u64, Project)>>;

// ---- tiny, stable fingerprint -------------------------------------------------

/// FNV-1a. Used instead of `DefaultHasher` so a fingerprint written by one run means the same
/// thing in the next one; a std hash change would otherwise force a pointless full rescan.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }

    fn finish(self) -> u64 {
        self.0
    }
}

fn mtime(p: &Path) -> u64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn hash_dir_entries(h: &mut Fnv, dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        h.bytes(e.file_name().to_string_lossy().as_bytes());
        h.u64(mtime(&e.path()));
    }
}

/// Cheap "did this folder change" signal: the folder and its immediate children, the manifests
/// descriptions come from, and the git refs the history facts come from. A handful of stats
/// per project instead of a walk over every file. A commit, a new/deleted file, a manifest or
/// README edit, or staging changes all move it; a content-only edit of a tracked file is caught
/// by git's dirty count the next time the folder is rescanned for any other reason, and Ctrl+R
/// always forces a full pass.
pub fn fingerprint(dir: &Path) -> u64 {
    let mut h = Fnv::new();
    h.u64(mtime(dir));
    hash_dir_entries(&mut h, dir);
    for name in [
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "requirements.txt",
        "setup.py",
        "README.md",
        "go.mod",
        "pubspec.yaml",
    ] {
        h.u64(mtime(&dir.join(name)));
    }
    let git = dir.join(".git");
    for name in ["HEAD", "index", "packed-refs"] {
        h.u64(mtime(&git.join(name)));
    }
    hash_dir_entries(&mut h, &git.join("refs/heads"));
    h.finish()
}

/// Build the in-session map from disk, used after a scan so the next (auto) rescan compares
/// against the numbers just measured rather than the ones loaded at startup.
pub fn scan_map(root: &str, projects: &[Project]) -> ScanCache {
    Arc::new(
        projects
            .iter()
            .map(|p| {
                let fp = fingerprint(&Path::new(root).join(&p.name));
                (p.name.clone(), (fp, p.clone()))
            })
            .collect(),
    )
}

// ---- serialisable mirrors of the in-memory types -----------------------------

fn intern(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[derive(Serialize, Deserialize)]
struct CachedProject {
    name: String,
    cat: String,
    stack: String,
    bytes: u64,
    files: u64,
    loc: u64,
    src: u64,
    nm: u64,
    target: u64,
    other: u64,
    langs: Vec<(String, u64)>,
    git: bool,
    commits: u64,
    days: Option<u64>,
    dirty: u64,
    fingerprint: u64,
}

impl CachedProject {
    fn new(p: &Project, fingerprint: u64) -> Self {
        CachedProject {
            name: p.name.clone(),
            cat: p.cat.to_string(),
            stack: p.stack.clone(),
            bytes: p.bytes,
            files: p.files,
            loc: p.loc,
            src: p.src,
            nm: p.nm,
            target: p.target,
            other: p.other,
            langs: p.langs.iter().map(|(l, n)| (l.clone(), *n)).collect(),
            git: p.git,
            commits: p.commits,
            days: p.days,
            dirty: p.dirty,
            fingerprint,
        }
    }

    fn into_project(self) -> Project {
        Project {
            name: self.name,
            cat: intern(self.cat),
            stack: self.stack,
            bytes: self.bytes,
            files: self.files,
            loc: self.loc,
            src: self.src,
            nm: self.nm,
            target: self.target,
            other: self.other,
            langs: self.langs,
            git: self.git,
            commits: self.commits,
            days: self.days,
            dirty: self.dirty,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CachedCommit {
    hash: String,
    subject: String,
    author: String,
    unix: i64,
}

#[derive(Serialize, Deserialize)]
struct CachedInfo {
    stack: Vec<String>,
    scripts: Vec<(String, String)>,
    package_manager: Option<String>,
    description: Option<String>,
    remote: Option<String>,
    dirty: u32,
    ahead: u32,
    behind: u32,
    commits: Vec<CachedCommit>,
    languages: Vec<(String, f32)>,
}

impl From<&Info> for CachedInfo {
    fn from(i: &Info) -> Self {
        CachedInfo {
            stack: i.stack.iter().map(|s| s.to_string()).collect(),
            scripts: i.scripts.clone(),
            package_manager: i.package_manager.map(|s| s.to_string()),
            description: i.description.clone(),
            remote: i.remote.clone(),
            dirty: i.dirty,
            ahead: i.ahead,
            behind: i.behind,
            commits: i
                .commits
                .iter()
                .map(|c| CachedCommit {
                    hash: c.hash.clone(),
                    subject: c.subject.clone(),
                    author: c.author.clone(),
                    unix: c.unix,
                })
                .collect(),
            languages: i
                .languages
                .iter()
                .map(|(l, f)| (l.to_string(), *f))
                .collect(),
        }
    }
}

impl CachedInfo {
    fn into_info(self) -> Info {
        Info {
            stack: self.stack.into_iter().map(intern).collect(),
            scripts: self.scripts,
            package_manager: self.package_manager.map(intern),
            description: self.description,
            remote: self.remote,
            dirty: self.dirty,
            ahead: self.ahead,
            behind: self.behind,
            commits: self
                .commits
                .into_iter()
                .map(|c| Commit {
                    hash: c.hash,
                    subject: c.subject,
                    author: c.author,
                    unix: c.unix,
                })
                .collect(),
            languages: self
                .languages
                .into_iter()
                .map(|(l, f)| (intern(l), f))
                .collect(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    root: String,
    done_at: Option<String>,
    scan_secs: f64,
    projects: Vec<CachedProject>,
    info: Vec<(String, CachedInfo)>,
}

/// The parts of a loaded cache the app needs.
pub struct Loaded {
    pub projects: Vec<Project>,
    pub scan_map: ScanCache,
    pub info: Vec<(String, Info)>,
    pub done_at: Option<String>,
    pub scan_secs: f64,
}

fn cache_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))?;
    Some(base.join("projects").join("cache.json"))
}

/// Load a cache for this root. Anything unreadable, stale or for another folder is ignored.
pub fn load(root: &str) -> Option<Loaded> {
    load_from(&cache_path()?, root)
}

fn load_from(path: &Path, root: &str) -> Option<Loaded> {
    let text = fs::read_to_string(path).ok()?;
    let cache: CacheFile = serde_json::from_str(&text).ok()?;
    if cache.version != VERSION || !cache.root.eq_ignore_ascii_case(root) {
        return None;
    }
    let mut projects = Vec::with_capacity(cache.projects.len());
    let mut map = HashMap::with_capacity(cache.projects.len());
    for entry in cache.projects {
        let fp = entry.fingerprint;
        let name = entry.name.clone();
        let project = entry.into_project();
        map.insert(name, (fp, project.clone()));
        projects.push(project);
    }
    Some(Loaded {
        projects,
        scan_map: Arc::new(map),
        info: cache.info.into_iter().map(|(n, i)| (n, i.into_info())).collect(),
        done_at: cache.done_at,
        scan_secs: cache.scan_secs,
    })
}

/// Persist the current scan. `Inspector::snapshot` leaves out folders whose facts are still
/// being computed, so a later save fills them in instead of freezing a half-read description.
pub fn save(root: &str, projects: &[Project], inspect: &Inspector, done_at: Option<String>, scan_secs: f64) {
    if let Some(path) = cache_path() {
        save_to(&path, root, projects, inspect, done_at, scan_secs);
    }
}

fn save_to(path: &Path, root: &str, projects: &[Project], inspect: &Inspector, done_at: Option<String>, scan_secs: f64) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let cache = CacheFile {
        version: VERSION,
        root: root.to_string(),
        done_at,
        scan_secs,
        projects: projects
            .iter()
            .map(|p| CachedProject::new(p, fingerprint(&Path::new(root).join(&p.name))))
            .collect(),
        info: inspect
            .snapshot()
            .into_iter()
            .map(|(n, i)| (n, CachedInfo::from(&i)))
            .collect(),
    };
    let Ok(text) = serde_json::to_string(&cache) else { return };
    // Write then rename: a crash mid-write can never leave a truncated cache that a later
    // startup would read as a cold start.
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, text).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> Project {
        Project {
            name: name.into(),
            cat: "Rust",
            stack: "Rust".into(),
            bytes: 10,
            files: 1,
            loc: 2,
            src: 10,
            nm: 0,
            target: 0,
            other: 0,
            langs: vec![("Rust".into(), 2)],
            git: true,
            commits: 3,
            days: Some(1),
            dirty: 0,
        }
    }

    #[test]
    fn a_project_round_trips_through_the_cache_dto() {
        let p = project("demo");
        let cached = CachedProject::new(&p, 42);
        let back = cached.into_project();
        assert_eq!(back.name, p.name);
        assert_eq!(back.cat, p.cat);
        assert_eq!(back.stack, p.stack);
        assert_eq!(back.bytes, p.bytes);
        assert_eq!(back.langs, p.langs);
        assert_eq!(back.commits, p.commits);
    }

    #[test]
    fn a_cache_file_round_trips_projects_and_inspection() {
        let dir = std::env::temp_dir().join(format!("ps-cache-file-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.json");

        let info = Info {
            description: Some("a cached description".into()),
            stack: vec!["Rust"],
            package_manager: Some("cargo"),
            languages: vec![("Rust", 1.0)],
            commits: vec![Commit {
                hash: "abc".into(),
                subject: "work".into(),
                author: "me".into(),
                unix: 42,
            }],
            ..Info::default()
        };
        let mut inspector = Inspector::new();
        inspector.restore(vec![("demo".into(), info)]);
        save_to(
            &path,
            r"P:\Projects",
            &[project("demo")],
            &inspector,
            Some("today".into()),
            1.5,
        );

        let loaded = load_from(&path, r"p:\projects").expect("the cache loads for the same root");
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].name, "demo");
        assert_eq!(loaded.projects[0].cat, "Rust");
        assert_eq!(loaded.done_at.as_deref(), Some("today"));
        assert_eq!(loaded.scan_secs, 1.5);
        let (_, back) = &loaded.info[0];
        assert_eq!(back.description.as_deref(), Some("a cached description"));
        assert_eq!(back.stack, ["Rust"]);
        assert_eq!(back.package_manager, Some("cargo"));
        assert_eq!(back.languages[0].0, "Rust");
        assert_eq!(back.commits[0].hash, "abc");
        assert!(
            load_from(&path, r"D:\Other").is_none(),
            "a cache for another root is ignored"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fingerprint_changes_when_a_file_is_added() {
        let dir = std::env::temp_dir().join(format!("ps-cache-fp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let before = fingerprint(&dir);
        fs::write(dir.join("new.txt"), "hi").unwrap();
        let after = fingerprint(&dir);
        assert_ne!(before, after, "a new file moves the fingerprint");
        fs::remove_dir_all(&dir).ok();
    }
}
