//! The background scanner: walks every top-level folder under the projects root in parallel,
//! classifies bytes into source vs build junk, counts hand-written lines per language, and asks
//! git for the facts. Progress streams to the UI thread as [`ScanEvent`]s.
//!
//! The walk is a hand-rolled `FindFirstFileW` traversal (Windows) with a shared work queue:
//! directory metadata comes straight from the directory enumeration, buckets are inherited
//! down the tree instead of re-derived per file, and nothing is allocated per entry. Git facts
//! come from the `git` CLI, run concurrently with the walk. Scan threads stay at normal
//! priority: BELOW_NORMAL starved the scan whenever the user was actively using the machine
//! (a 4 s scan stretched past 3 minutes), while frame CPU is ~0.5 ms against a 16 ms budget.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::model::{Project, ScanEvent};

/// Extensions we count as hand-written code.
const CODE_EXT: &[&str] = &[
    "rs", "ts", "tsx", "js", "mjs", "cjs", "jsx", "py", "c", "cc", "cpp", "cxx", "h", "hh", "hpp",
    "kt", "kts", "swift", "go", "java", "php", "rb", "html", "css", "scss", "vue", "svelte", "sh",
    "sql", "toml", "dart", "cs", "lua", "zig",
];

/// Directories whose bytes are regenerable build output (other than node_modules / target).
const BUILD_DIRS: &[&str] = &[
    "dist",
    "build",
    "out",
    ".next",
    ".turbo",
    ".cache",
    "__pycache__",
    ".venv",
    "venv",
    "coverage",
    ".expo",
    ".output",
    ".svelte-kit",
    ".parcel-cache",
    "site-packages",
    ".gradle",
];

/// Max bytes of a source file we will read to count lines.
const MAX_READ: u64 = 262_144;

/// Bucket flags, inherited down the tree: a file under any `node_modules` is junk no matter
/// how deep. Bit 1 junk (node_modules), bit 2 target/deriveddata, bit 4 other build output.
type Bucket = u8;
const JUNK: Bucket = 1;
const TARGET: Bucket = 2;
const OTHER: Bucket = 4;

/// Case-insensitive name match, over either a `&str` or a raw `FindFirstFileW` name.
trait CiEq {
    fn eq_ci(&self, other: &str) -> bool;
}

impl CiEq for str {
    fn eq_ci(&self, other: &str) -> bool {
        self.eq_ignore_ascii_case(other)
    }
}

impl CiEq for [u16] {
    fn eq_ci(&self, other: &str) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .zip(other.as_bytes())
                .all(|(&a, &b)| a == b as u16 || a == (b.to_ascii_uppercase()) as u16)
    }
}

/// One bucket function for both walkers: the Windows one matches raw wide names without
/// allocating, the fallback matches `&str`s.
fn bucket_of(name: &(impl CiEq + ?Sized)) -> Bucket {
    if name.eq_ci("node_modules") {
        return JUNK;
    }
    let mut b = 0;
    if name.eq_ci("target") || name.eq_ci("deriveddata") {
        b |= TARGET;
    }
    if name.eq_ci(".git") || BUILD_DIRS.iter().any(|d| name.eq_ci(d)) {
        b |= OTHER;
    }
    b
}

/// Which language a countable file name maps to, or None when it isn't hand-written code.
/// Lock files and source maps never pass the extension gate; minified bundles carry a `js`
/// extension, so they need their own exclusion.
fn lang_of(name: &str) -> Option<&'static str> {
    if ends_with_ci(name, ".min.js") {
        return None;
    }
    let (dot, ext) = name.rfind('.').map(|i| (i, &name[i + 1..]))?;
    if dot == 0 {
        return None;
    }
    CODE_EXT
        .iter()
        .find(|e| ext.eq_ignore_ascii_case(e))
        .map(|e| canonical_lang(e))
}

fn ends_with_ci(name: &str, suffix: &str) -> bool {
    name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

#[derive(Clone, Default)]
struct Agg {
    bytes: u64,
    files: u64,
    loc: u64,
    src: u64,
    nm: u64,
    target: u64,
    other: u64,
    langs: HashMap<&'static str, u64>,
}

impl Agg {
    fn merge(&mut self, other: &mut Agg) {
        self.bytes += other.bytes;
        self.files += other.files;
        self.loc += other.loc;
        self.src += other.src;
        self.nm += other.nm;
        self.target += other.target;
        self.other += other.other;
        for (lang, n) in other.langs.drain() {
            *self.langs.entry(lang).or_insert(0) += n;
        }
    }

    /// One file. `name` is the file's own name; `bucket` is the directory's inherited bucket.
    /// `abs` is passed only for files that might get line-counted, so the walker can skip
    /// building paths for the other ~400k entries.
    #[cfg(not(windows))]
    fn add(&mut self, name: &str, len: u64, bucket: Bucket, abs: Option<&Path>) {
        let bucket = bucket | bucket_of(name);
        let lang = if bucket & JUNK == 0 && len > 0 && len <= MAX_READ {
            lang_of(name)
        } else {
            None
        };
        self.add_classified(len, bucket, lang, abs);
    }

    /// The accounting core: bucket decides where the bytes land; `lang` + `abs` opt into a
    /// line count.
    fn add_classified(
        &mut self,
        len: u64,
        bucket: Bucket,
        lang: Option<&'static str>,
        abs: Option<&Path>,
    ) {
        self.bytes += len;
        self.files += 1;
        // Bit tests, not an exact match: buckets OR together down the tree, and junk wins
        // over whatever else the path picked up.
        if bucket & JUNK != 0 {
            self.nm += len;
        } else if bucket & TARGET != 0 {
            self.target += len;
        } else if bucket & OTHER != 0 {
            self.other += len;
        } else {
            self.src += len;
            if let (Some(lang), Some(abs)) = (lang, abs) {
                let lines = count_lines(abs);
                if lines > 0 {
                    *self.langs.entry(lang).or_insert(0) += lines;
                    self.loc += lines;
                }
            }
        }
    }
}

/// `.cc`/`.cpp`/`.cxx`/`.hpp` fold into their display names; JSX is the same language as TS
/// here, so `.tsx`/`.jsx` count as TypeScript too.
fn canonical_lang(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "tsx" | "jsx" => "TypeScript",
        "ts" => "TypeScript",
        "js" | "mjs" | "cjs" => "JavaScript",
        "rs" => "Rust",
        "py" => "Python",
        "cc" | "cpp" | "cxx" => "C++",
        "hpp" | "hh" => "C/C++ headers",
        "h" => "C/C++ headers",
        "c" => "C",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "go" => "Go",
        "java" => "Java",
        "php" => "PHP",
        "rb" => "Ruby",
        "html" => "HTML",
        "css" | "scss" => "CSS",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "sh" => "Shell",
        "sql" => "SQL",
        "toml" => "TOML",
        "dart" => "Dart",
        "cs" => "C#",
        "lua" => "Lua",
        "zig" => "Zig",
        _ => "Other",
    }
}

fn count_lines(path: &Path) -> u64 {
    let Ok(mut f) = fs::File::open(path) else {
        return 0;
    };
    let mut total = 0u64;
    let mut take = MAX_READ;
    let mut chunk = [0u8; 128 * 1024];
    loop {
        let want = chunk.len().min(take as usize);
        match f.read(&mut chunk[..want]) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                total += chunk[..n].iter().filter(|&&b| b == b'\n').count() as u64;
                take -= n as u64;
            }
        }
        if take == 0 {
            break;
        }
    }
    total
}

/// Git facts per project: is a repo, commit count, days since last commit, dirty files.
#[derive(Debug, Clone, Default)]
pub struct GitFacts {
    pub repo: bool,
    pub commits: u64,
    pub days: Option<u64>,
    pub dirty: u64,
}

/// A cached fact set with its refs-mtime validity key.
pub type GitFactsKeyed = (GitFacts, u64);

/// The cross-scan cache, shared between the scanner thread and the UI state.
pub type GitCache = std::sync::Arc<std::sync::Mutex<HashMap<PathBuf, GitFactsKeyed>>>;

/// Cheap validity key for cached git facts: commits and last-commit time only change when
/// refs or the index do. Dirty files are recomputed every scan (they change constantly).
fn git_cache_key(dir: &Path) -> u64 {
    let git = dir.join(".git");
    let mut key = 0u64;
    for p in [git.join("index"), git.join("HEAD"), git.join("packed-refs")] {
        key = key.max(mtime_nanos(&p));
    }
    if let Ok(rd) = fs::read_dir(git.join("refs").join("heads")) {
        for e in rd.flatten() {
            key = key.max(mtime_nanos(&e.path()));
        }
    }
    key
}

fn mtime_nanos(p: &Path) -> u64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Drop the calling thread to below-normal CPU priority, so the scan's workers always
/// yield to everything the user is running — music, other apps, and this app's own UI —
/// while still making normal progress. Combined with the modest worker counts, the scan
/// stops being the thing that makes the machine lag. No-op off Windows.
#[cfg(windows)]
fn enter_background_priority() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[cfg(not(windows))]
fn enter_background_priority() {}

fn git_output(dir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    // --no-optional-locks: concurrent `git status` must never fight over index.lock.
    cmd.args([
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
    ])
    .args(args)
    .current_dir(dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // BELOW_NORMAL: the scan's git work yields CPU to everything the user is running.
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    // A hung git (a stale index.lock, a credential prompt) must not stall the scan
    // forever: poll with a deadline and kill the child when it passes. Stdin is null so
    // a prompt cannot block on the terminal, and the pipes carry the output we parse.
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + GIT_TIMEOUT;
    let finished = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    let out = match finished {
        // Collect whatever the exited child wrote.
        Some(_) => child.wait_with_output().ok()?,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// How long one git call may take before it is killed and treated as "no facts".
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Git facts via the git CLI. `cache` short-circuits the two history calls when refs
/// haven't moved since the last scan; dirty is always recomputed.
fn git_facts(dir: &Path, cache: &mut HashMap<PathBuf, GitFactsKeyed>) -> GitFacts {
    if !dir.join(".git").exists() {
        return GitFacts::default();
    }
    let key = git_cache_key(dir);
    if let Some((facts, k)) = cache.get(dir) {
        if *k == key {
            return facts.clone();
        }
    }
    let mut facts = git_history(dir);
    facts.dirty = git_output(dir, &["status", "--porcelain"])
        .map(|s| s.lines().count() as u64)
        .unwrap_or(0);
    cache.insert(dir.to_path_buf(), (facts.clone(), key));
    facts
}

/// Git facts via two cheap calls: the commit count and the last commit's age. The
/// cache in `git_facts` short-circuits both when refs haven't moved.
fn git_history(dir: &Path) -> GitFacts {
    let mut facts = GitFacts {
        repo: true,
        ..Default::default()
    };
    if let Some(out) = git_output(dir, &["rev-list", "--count", "HEAD"]) {
        facts.commits = out.lines().next().and_then(|l| l.parse().ok()).unwrap_or(0);
    }
    if let Some(out) = git_output(dir, &["log", "-1", "--format=%cI"]) {
        // ISO 8601: YYYY-MM-DDTHH:MM:SS...
        if out.len() >= 10 {
            let (y, m, d) = (
                out[0..4].parse::<i64>().unwrap_or(0),
                out[5..7].parse::<i64>().unwrap_or(0),
                out[8..10].parse::<i64>().unwrap_or(0),
            );
            if y > 0 && (1..=12).contains(&m) && (1..=31).contains(&d) {
                let then = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|e| e.as_secs() as i64)
                    .unwrap_or(0);
                // Days since, via the civil date difference against today in UTC.
                let now_days = then.div_euclid(86_400);
                let then_days = {
                    // days_from_civil (Hinnant)
                    let yy = if m <= 2 { y - 1 } else { y };
                    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
                    let yoe = yy - era * 400;
                    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
                    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
                    era * 146_097 + doe - 719_468
                };
                facts.days = Some((now_days - then_days).max(0) as u64);
            }
        }
    }
    facts
}

/// Category and stack string, inferred from marker files and the language mix.
fn classify(dir: &Path, agg: &Agg) -> (&'static str, String) {
    let has = |name: &str| dir.join(name).exists();
    let read = |name: &str| fs::read_to_string(dir.join(name)).unwrap_or_default();

    if has("Cargo.toml") {
        return ("Native Desktop", "Rust".into());
    }
    if has("package.json") {
        let pkg = read("package.json").to_lowercase();
        let mut stack: Vec<&str> = Vec::new();
        for (needle, label) in [
            ("\"next\"", "Next.js"),
            ("\"react\"", "React"),
            ("\"vue\"", "Vue"),
            ("\"svelte\"", "Svelte"),
            ("\"astro\"", "Astro"),
            ("\"expo\"", "Expo"),
            ("\"react-native\"", "React Native"),
            ("\"electron\"", "Electron"),
            ("\"convex\"", "Convex"),
            ("\"prisma\"", "Prisma"),
            ("\"tailwindcss\"", "Tailwind"),
            ("\"typescript\"", "TypeScript"),
        ] {
            if pkg.contains(needle) && !stack.contains(&label) {
                stack.push(label);
            }
        }
        let cat = if pkg.contains("\"expo\"") || pkg.contains("\"react-native\"") {
            "Mobile"
        } else if pkg.contains("\"electron\"") {
            "Desktop App"
        } else {
            "Web App"
        };
        let stack = if stack.is_empty() {
            "JavaScript".to_string()
        } else {
            stack.join(", ")
        };
        return (cat, stack);
    }
    if has("pyproject.toml") || has("requirements.txt") || has("setup.py") {
        let text = read("pyproject.toml") + &read("requirements.txt");
        let lower = text.to_lowercase();
        let ml = ["torch", "ultralytics", "transformers", "onnx", "tensorflow"]
            .iter()
            .any(|n| lower.contains(n));
        let stack = if ml { "Python, PyTorch" } else { "Python" };
        return (if ml { "AI / ML" } else { "AI Tooling" }, stack.into());
    }
    // Fall back on the dominant language.
    let mut best: Option<(&'static str, u64)> = None;
    for (lang, lines) in &agg.langs {
        if best.is_none_or(|b| *lines > b.1) {
            best = Some((lang, *lines));
        }
    }
    match best.map(|(l, _)| l) {
        Some("Kotlin") => ("Mobile", "Kotlin".into()),
        Some("PHP") => ("Client Site", "PHP".into()),
        Some("Swift") => ("Mobile", "Swift".into()),
        Some("Python") => ("AI / ML", "Python".into()),
        Some("Rust") => ("Native Desktop", "Rust".into()),
        Some("HTML") | Some("CSS") => ("Web App", "HTML, CSS".into()),
        Some(l) => ("Web App", l.to_string()),
        None => ("Other", "—".into()),
    }
}

/// Top-level entries of the root that are folders.
pub fn list_projects(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .filter(|p| {
                    // Dot folders are tooling and dependency checkouts, not projects.
                    p.file_name()
                        .map(|n| !n.to_string_lossy().starts_with('.'))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

/// Full scan of `root`. The walk and git run concurrently; results stream once both finish.
/// `git_cache` carries facts between scans so rescans skip the history work. Runs on its own
/// thread.
pub fn scan(
    root: PathBuf,
    tx: Sender<ScanEvent>,
    git_cache: GitCache,
    wake: impl Fn() + Send + Sync + 'static,
) {
    let started = Instant::now();
    let dirs = list_projects(&root);
    if dirs.is_empty() {
        let _ = tx.send(ScanEvent::Failed(format!(
            "no folders under {}",
            root.display()
        )));
        wake();
        return;
    }
    let _ = tx.send(ScanEvent::Start { total: dirs.len() });
    wake();

    std::thread::scope(|scope| {
        // Git facts: repos round-robined over 8 workers.
        let git_results: std::sync::Arc<std::sync::Mutex<Vec<Option<GitFacts>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(
                (0..dirs.len()).map(|_| None).collect(),
            ));
        let git_handle = {
            let git_results = git_results.clone();
            let cache = git_cache.clone();
            let dirs = dirs.clone();
            scope.spawn(move || {
                let repos: Vec<(usize, PathBuf)> = dirs
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| d.join(".git").exists())
                    .map(|(i, d)| (i, d.clone()))
                    .collect();
                let workers = 4.min(repos.len().max(1));
                let locals: Vec<(Vec<(usize, GitFacts)>, HashMap<PathBuf, GitFactsKeyed>)> =
                    std::thread::scope(|s| {
                        let mut handles = Vec::new();
                        for w in 0..workers {
                            let mine: Vec<(usize, PathBuf)> = repos
                                .iter()
                                .enumerate()
                                .filter(|(j, _)| j % workers == w)
                                .map(|(_, (i, d))| (*i, d.clone()))
                                .collect();
                            let cache = cache.clone();
                            handles.push(s.spawn(move || {
                                enter_background_priority();
                                // Seed from the shared cache so rescans skip the history
                                // work; entries written this scan land in `local` and are
                                // merged back when the workers join.
                                let mut local: HashMap<PathBuf, GitFactsKeyed> =
                                    cache.lock().unwrap().clone();
                                let mut out: Vec<(usize, GitFacts)> =
                                    Vec::with_capacity(mine.len());
                                for (i, d) in mine {
                                    out.push((i, git_facts(&d, &mut local)));
                                }
                                (out, local)
                            }));
                        }
                        handles.into_iter().map(|h| h.join().unwrap()).collect()
                    });
                let mut results = git_results.lock().unwrap();
                let mut cache = cache.lock().unwrap();
                for (out, local) in locals {
                    for (i, facts) in out {
                        results[i] = Some(facts);
                    }
                    cache.extend(local);
                }
            })
        };
        // A row per project, the moment its walk finishes. Git runs alongside, so a row
        // usually lands without history facts and gets them patched in once git catches up:
        // the window fills while the scan runs instead of after it.
        let send_row = |i: usize, agg: &Agg, facts: Option<GitFacts>| {
            let dir = &dirs[i];
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let (cat, stack) = classify(dir, agg);
            let mut langs: Vec<(String, u64)> =
                agg.langs.iter().map(|(l, n)| (l.to_string(), *n)).collect();
            langs.sort_by(|a, b| b.1.cmp(&a.1));
            langs.truncate(8);
            let facts = facts.unwrap_or_default();
            let project = Project {
                name,
                cat,
                stack,
                bytes: agg.bytes,
                files: agg.files,
                loc: agg.loc,
                src: agg.src,
                nm: agg.nm,
                target: agg.target,
                other: agg.other,
                langs,
                git: facts.repo,
                commits: facts.commits,
                days: facts.days,
                dirty: facts.dirty,
            };
            let _ = tx.send(ScanEvent::Project(Box::new(project)));
            wake();
        };
        // Which rows were sent before git knew anything about them.
        let waiting: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
        let on_project = |i: usize, agg: Agg| {
            let facts = git_results.lock().unwrap()[i].clone();
            if facts.is_none() {
                waiting.lock().unwrap().push(i);
            }
            send_row(i, &agg, facts);
        };
        // The walk, on its own threads, concurrent with git; join git before reading its
        // results, or the patch below would race the writers.
        let aggs = walk_all(&dirs, &on_project);
        git_handle.join().unwrap();
        let git_all = std::sync::Arc::try_unwrap(git_results)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_else(|a| std::mem::take(&mut a.lock().unwrap()));
        // Rows that landed before their history was read get it now, in place.
        for i in waiting.into_inner().unwrap() {
            send_row(i, &aggs[i], git_all[i].clone());
        }
    });
    let _ = tx.send(ScanEvent::Done {
        seconds: started.elapsed().as_secs_f64(),
    });
    wake();
}

/// Walk every project folder concurrently, one aggregate per project. Windows uses a
/// hand-rolled `FindFirstFileW` traversal: sizes arrive with the directory enumeration (no
/// per-file stat), buckets are inherited down the tree, and nothing allocates per entry.
#[cfg(windows)]
fn walk_all(dirs: &[PathBuf], on_project: &(dyn Fn(usize, Agg) + Send + Sync)) -> Vec<Agg> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstFileExW, FindNextFileW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FIND_FIRST_EX_LARGE_FETCH, WIN32_FIND_DATAW,
    };

    struct Task {
        proj: usize,
        dir: PathBuf,
        bucket: Bucket,
    }

    /// Shared directory work queue. `pending` counts pushed-but-unfinished tasks, so
    /// workers know when the whole walk is done; `waiters` (under the same lock) counts
    /// sleeping workers, so pushes wake exactly as many workers as there is new work.
    struct Queue {
        tasks: Mutex<(Vec<Task>, usize)>,
        cv: Condvar,
        pending: AtomicUsize,
        /// Directories still outstanding per project: 0 means that project is fully measured.
        per_proj: Mutex<Vec<usize>>,
    }

    impl Queue {
        fn push(&self, t: Task) {
            self.pending.fetch_add(1, Ordering::Release);
            self.per_proj.lock().unwrap()[t.proj] += 1;
            let mut g = self.tasks.lock().unwrap();
            g.0.push(t);
            if g.1 > 0 {
                self.cv.notify_one();
            }
        }
        fn pop(&self) -> Option<Task> {
            let mut g = self.tasks.lock().unwrap();
            loop {
                if let Some(t) = g.0.pop() {
                    return Some(t);
                }
                if self.pending.load(Ordering::Acquire) == 0 {
                    return None;
                }
                g.1 += 1;
                g = self.cv.wait(g).unwrap();
                g.1 -= 1;
            }
        }
        /// One directory done: true when it was the last one of its project.
        fn finished_one(&self, proj: usize) -> bool {
            let left = {
                let mut counts = self.per_proj.lock().unwrap();
                counts[proj] -= 1;
                counts[proj]
            };
            self.pending.fetch_sub(1, Ordering::Release);
            // Only the end of the walk needs a broadcast; new work arriving is covered by
            // push's own notify.
            if self.pending.load(Ordering::Acquire) == 0 {
                self.cv.notify_all();
            }
            left == 0
        }
    }

    fn wide_ext(w: &[u16]) -> Option<&[u16]> {
        let i = w.iter().rposition(|&c| c == b'.' as u16)?;
        if i == 0 {
            None
        } else {
            Some(&w[i + 1..])
        }
    }

    let aggs: Arc<Mutex<Vec<Agg>>> = Arc::new(Mutex::new(
        (0..dirs.len()).map(|_| Agg::default()).collect(),
    ));
    let queue = Arc::new(Queue {
        tasks: Mutex::new((Vec::new(), 0)),
        cv: Condvar::new(),
        pending: AtomicUsize::new(0),
        per_proj: Mutex::new(vec![0; dirs.len()]),
    });
    // `\\?\` lets paths run past the 260-char limit (node_modules gets deep), but only
    // accepts fully-qualified paths: canonicalize first (it returns the prefix on Windows).
    for (i, d) in dirs.iter().enumerate() {
        let abs = fs::canonicalize(d).unwrap_or_else(|_| d.clone());
        let dir = if abs.as_os_str().to_string_lossy().starts_with("\\\\?\\") {
            abs
        } else {
            PathBuf::from(format!("\\\\\\?\\{}", abs.display()))
        };
        queue.push(Task {
            proj: i,
            dir,
            bucket: 0,
        });
    }

    let workers = 24;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue = queue.clone();
            let aggs = aggs.clone();
            scope.spawn(move || {
                enter_background_priority();
                let mut local: HashMap<usize, Agg> = HashMap::new();
                while let Some(task) = queue.pop() {
                    let agg = local.entry(task.proj).or_default();
                    let mut pattern = task.dir.as_os_str().encode_wide().collect::<Vec<u16>>();
                    pattern.extend([b'\\' as u16, b'*' as u16, 0]);
                    let mut fd: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
                    unsafe {
                        let handle = FindFirstFileExW(
                            pattern.as_ptr(),
                            1, // FindExInfoBasic: skips the DOS 8.3 name, faster
                            &mut fd as *mut WIN32_FIND_DATAW as *mut core::ffi::c_void,
                            0, // FindExSearchNameMatch
                            std::ptr::null(),
                            FIND_FIRST_EX_LARGE_FETCH, // bigger kernel fetch buffer
                        );
                        if handle != INVALID_HANDLE_VALUE {
                            loop {
                                let full = &fd.cFileName;
                                let n = full.iter().position(|&c| c == 0).unwrap_or(full.len());
                                let name = &full[..n];
                                let is_dot = name.first() == Some(&(b'.' as u16))
                                    && (name.len() == 1
                                        || (name.len() == 2 && name[1] == b'.' as u16));
                                if !is_dot {
                                    let len =
                                        ((fd.nFileSizeHigh as u64) << 32) | fd.nFileSizeLow as u64;
                                    let attrs = fd.dwFileAttributes;
                                    if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
                                        // Junctions and symlinks: never descend (cycles), never count.
                                        if attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                                            queue.push(Task {
                                                proj: task.proj,
                                                dir: task.dir.join(OsString::from_wide(name)),
                                                bucket: task.bucket | bucket_of(name),
                                            });
                                        }
                                    } else {
                                        let bucket = task.bucket | bucket_of(name);
                                        // Only real line-count candidates pay for a name and
                                        // path allocation; the other ~400k entries never do.
                                        let candidate = len > 0
                                            && len <= MAX_READ
                                            && wide_ext(name).is_some_and(|ext| {
                                                CODE_EXT.iter().any(|e| ext.eq_ci(e))
                                            });
                                        let lang = candidate.then(|| {
                                            lang_of(
                                                OsString::from_wide(name)
                                                    .to_string_lossy()
                                                    .as_ref(),
                                            )
                                        });
                                        let abs = candidate
                                            .then(|| task.dir.join(OsString::from_wide(name)));
                                        agg.add_classified(
                                            len,
                                            bucket,
                                            lang.flatten(),
                                            abs.as_deref(),
                                        );
                                    }
                                }
                                if FindNextFileW(handle, &mut fd) == 0 {
                                    break;
                                }
                            }
                            FindClose(handle);
                        }
                    }
                    // This project's tree is fully enumerated: hand its numbers over now,
                    // so the window fills row by row instead of after the whole walk.
                    if queue.finished_one(task.proj) {
                        let mut mine = local.remove(&task.proj).unwrap_or_default();
                        {
                            let mut all = aggs.lock().unwrap();
                            all[task.proj].merge(&mut mine);
                            let snapshot = all[task.proj].clone();
                            drop(all);
                            on_project(task.proj, snapshot);
                        }
                    }
                }
                let mut all = aggs.lock().unwrap();
                for (i, mut a) in local {
                    all[i].merge(&mut a);
                }
            });
        }
    });
    Arc::try_unwrap(aggs)
        .map(|m| m.into_inner().unwrap())
        .unwrap_or_else(|a| std::mem::take(&mut a.lock().unwrap()))
}

/// Non-Windows fallback: plain recursive traversal, same accounting.
#[cfg(not(windows))]
fn walk_all(dirs: &[PathBuf]) -> Vec<Agg> {
    fn rec(dir: &Path, proj: usize, bucket: Bucket, aggs: &mut Vec<Agg>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name();
            let name = name.to_string_lossy();
            if ft.is_dir() {
                rec(&e.path(), proj, bucket | bucket_of(name.as_ref()), aggs);
            } else if ft.is_file() {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                aggs[proj].add(&name, len, bucket, Some(&e.path()));
            }
        }
    }
    let mut aggs: Vec<Agg> = (0..dirs.len()).map(|_| Agg::default()).collect();
    for (i, d) in dirs.iter().enumerate() {
        rec(d, i, 0, &mut aggs);
        // Streamed the moment this project's tree is measured, not at the end of the walk.
        on_project(i, aggs[i].clone());
    }
    aggs
}

#[cfg(test)]
mod lang_tests {
    use super::*;

    #[test]
    fn tsx_and_jsx_count_as_typescript() {
        assert_eq!(canonical_lang("tsx"), "TypeScript");
        assert_eq!(canonical_lang("jsx"), "TypeScript");
        assert_eq!(canonical_lang("ts"), "TypeScript");
        assert_eq!(canonical_lang("cc"), "C++");
        assert_eq!(canonical_lang("hpp"), "C/C++ headers");
        assert_eq!(canonical_lang("h"), "C/C++ headers");
    }

    #[test]
    fn lockfiles_minified_bundles_and_dotfiles_never_count() {
        assert_eq!(lang_of("package-lock.json"), None);
        assert_eq!(lang_of("yarn.lock"), None);
        assert_eq!(lang_of("bundle.min.js"), None);
        assert_eq!(lang_of("bundle.js.map"), None);
        assert_eq!(lang_of(".rs"), None, "a dotfile has no extension");
        assert_eq!(lang_of("main.rs"), Some("Rust"));
        assert_eq!(
            lang_of("MAIN.RS"),
            Some("Rust"),
            "extension match ignores case"
        );
    }
}
