//! The background scanner: walks every top-level folder under the projects root in parallel,
//! classifies bytes into source vs build junk, counts hand-written lines per language, and asks
//! git for the facts. Progress streams to the UI thread as [`ScanEvent`]s.
//!
//! Two speed decisions live here:
//! - The walk is a hand-rolled `FindFirstFileW` traversal (Windows) with a shared work queue:
//!   directory metadata comes straight from the directory enumeration, buckets are inherited
//!   down the tree instead of re-derived per file, and nothing is allocated per entry.
//! - Git facts come from libgit2 in-process (`git2`): spawning `git.exe` costs ~170 ms each on
//!   Windows, which made 350 spawns the slowest part of the whole scan.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::process::Command;
use std::time::Instant;

use crate::model::{Project, ScanEvent};

/// Extensions we count as hand-written code.
const CODE_EXT: &[&str] = &[
    "rs", "ts", "tsx", "js", "mjs", "cjs", "jsx", "py", "c", "cc", "cpp", "cxx", "h", "hh", "hpp",
    "kt", "kts", "swift", "go", "java", "php", "rb", "html", "css", "scss", "vue", "svelte", "sh",
    "sql", "toml", "dart", "cs", "lua", "zig",
];

/// Files never worth counting as hand-written lines.
const JUNK_NAMES: &[&str] = &[
    "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "cargo.lock", "bun.lockb", "bun.lock",
];

/// Directories whose bytes are regenerable build output (other than node_modules / target).
const BUILD_DIRS: &[&str] = &[
    "dist", "build", "out", ".next", ".turbo", ".cache", "__pycache__", ".venv", "venv",
    "coverage", ".expo", ".output", ".svelte-kit", ".parcel-cache", "site-packages", ".gradle",
];

/// Max bytes of a source file we will read to count lines.
const MAX_READ: u64 = 262_144;

/// Bucket flags, inherited down the tree: a file under any `node_modules` is junk no matter
/// how deep. Bit 1 junk (node_modules), bit 2 target/deriveddata, bit 4 other build output.
type Bucket = u8;
const JUNK: Bucket = 1;
const TARGET: Bucket = 2;
const OTHER: Bucket = 4;

#[cfg(not(windows))]
#[cfg(not(windows))]
fn bucket_of(name: &str) -> Bucket {
    if name.eq_ignore_ascii_case("node_modules") {
        return JUNK;
    }
    let mut b = 0;
    if name.eq_ignore_ascii_case("target") || name.eq_ignore_ascii_case("deriveddata") {
        b |= TARGET;
    }
    if name.eq_ignore_ascii_case(".git") || BUILD_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d)) {
        b |= OTHER;
    }
    b
}

#[cfg(not(windows))]
#[cfg(not(windows))]
fn ends_with_ci(name: &str, suffix: &str) -> bool {
    name.len() >= suffix.len()
        && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

#[derive(Default)]
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
    #[cfg(not(windows))]
    /// `abs` is passed only for files that might get line-counted, so the walker can skip
    /// building paths for the other ~400k entries.
    fn add(&mut self, name: &str, len: u64, bucket: Bucket, abs: Option<&Path>) {
        // A file can carry its own bucket (a file literally named `target`), matching the
        // old per-component behaviour.
        let own = bucket_of(name);
        let bucket = if own & JUNK != 0 { JUNK } else { bucket | own };

        // Hand-written lines: known code extension, small enough, not a lock file. All
        // case-insensitive, so no per-file lowercase allocation.
        let mut lang = None;
        if bucket & JUNK == 0
            && len > 0
            && len <= MAX_READ
            && !JUNK_NAMES.iter().any(|j| name.eq_ignore_ascii_case(j))
            && !ends_with_ci(name, ".min.js")
            && !ends_with_ci(name, ".map")
        {
            if let Some((dot, ext)) = name.rfind('.').map(|i| (i, &name[i + 1..])) {
                if dot > 0 {
                    if let Some(e) = CODE_EXT.iter().find(|e| ext.eq_ignore_ascii_case(e)) {
                        lang = Some(canonical_lang(e, name));
                    }
                }
            }
        }
        self.add_classified(len, bucket, lang, abs);
    }

    /// The accounting core: bucket decides where the bytes land; `lang` + `abs` opt into a
    /// line count.
    fn add_classified(&mut self, len: u64, bucket: Bucket, lang: Option<&'static str>, abs: Option<&Path>) {
        self.bytes += len;
        self.files += 1;
        match bucket {
            JUNK => self.nm += len,
            TARGET => self.target += len,
            OTHER => self.other += len,
            _ => {
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
}

/// `.cc`/`.cpp`/`.cxx`/`.hpp` fold into their display names; JSX is the same language as TS
/// here, so `.tsx`/`.jsx` count as TypeScript too.
fn canonical_lang(ext: &str, _name: &str) -> &'static str {
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
    let Ok(mut f) = fs::File::open(path) else { return 0 };
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

/// Scan threads yield to the UI: below-normal priority means a busy scan never costs the
/// render thread a frame. No-op off Windows.
#[cfg(windows)]
fn lower_priority() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[cfg(not(windows))]
fn lower_priority() {}

/// Git facts per project: is a repo, commit count, days since last commit, dirty files.
pub type GitFacts = (bool, u64, Option<u64>, u64);

/// A cached fact set with its refs-mtime validity key.
pub type GitFactsKeyed = (GitFacts, u64);

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

fn git_output(dir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    // --no-optional-locks: concurrent `git status` must never fight over index.lock.
    cmd.args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false"])
        .args(args)
        .current_dir(dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Git facts via the git CLI. `cache` short-circuits the two history calls when refs
/// haven't moved since the last scan; dirty is always recomputed.
fn git_facts(dir: &Path, cache: &mut HashMap<PathBuf, GitFactsKeyed>) -> GitFacts {
    if !dir.join(".git").exists() {
        return (false, 0, None, 0);
    }
    let key = git_cache_key(dir);
    let (commits, days) = match cache.get(dir) {
        Some(((true, c, d, _), k)) if *k == key => (*c, *d),
        _ => {
            let commits = git_output(dir, &["rev-list", "--count", "HEAD"])
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let days = git_output(dir, &["log", "-1", "--format=%ct"]).map(|s| {
                let t: u64 = s.trim().parse().unwrap_or(0);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                now.saturating_sub(t) / 86_400
            });
            (commits, days)
        }
    };
    let dirty = git_output(dir, &["status", "--porcelain"])
        .map(|s| s.lines().count() as u64)
        .unwrap_or(0);
    let facts = (true, commits, days, dirty);
    cache.insert(dir.to_path_buf(), (facts, key));
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
                    // The rune checkout is a dependency, not a project; skip hidden and known junk.
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
    git_cache: std::sync::Arc<std::sync::Mutex<HashMap<PathBuf, GitFactsKeyed>>>,
    wake: impl Fn() + Send + Sync + 'static,
) {
    let started = Instant::now();
    let dirs = list_projects(&root);
    if dirs.is_empty() {
        let _ = tx.send(ScanEvent::Failed(format!("no folders under {}", root.display())));
        wake();
        return;
    }
    let _ = tx.send(ScanEvent::Start { total: dirs.len() });
    wake();

    lower_priority();
    std::thread::scope(|scope| {
        // Git: in-process libgit2, repos round-robined over one thread per core.
        let git_results: std::sync::Arc<std::sync::Mutex<Vec<Option<GitFacts>>>> =
            std::sync::Arc::new(std::sync::Mutex::new((0..dirs.len()).map(|_| None).collect()));
        let git_handle = {
            let git_results = git_results.clone();
            let cache = git_cache.clone();
            let dirs = dirs.clone();
            scope.spawn(move || {
                lower_priority();
                let repos: Vec<(usize, PathBuf)> = dirs
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| d.join(".git").exists())
                    .map(|(i, d)| (i, d.clone()))
                    .collect();
                let workers = 8.min(repos.len().max(1));
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
                            handles.push(s.spawn(move || {
                                lower_priority();
                                let mut local: HashMap<PathBuf, GitFactsKeyed> = HashMap::new();
                                let mut out: Vec<(usize, GitFacts)> = Vec::with_capacity(mine.len());
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
        // The walk, on its own threads, concurrent with git; join git before draining its
        // results, or the drain below would race the writers.
        let aggs = walk_all(&dirs);
        git_handle.join().unwrap();
        let git_all = std::sync::Arc::try_unwrap(git_results)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_else(|a| std::mem::take(&mut a.lock().unwrap()));
        for (i, dir) in dirs.iter().enumerate() {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let (git, commits, days, dirty) = git_all[i].unwrap_or((false, 0, None, 0));
            let (cat, stack) = classify(dir, &aggs[i]);
            let mut langs: Vec<(String, u64)> =
                aggs[i].langs.iter().map(|(l, n)| (l.to_string(), *n)).collect();
            langs.sort_by(|a, b| b.1.cmp(&a.1));
            langs.truncate(8);
            let project = Project {
                name,
                cat,
                stack,
                bytes: aggs[i].bytes,
                files: aggs[i].files,
                loc: aggs[i].loc,
                src: aggs[i].src,
                nm: aggs[i].nm,
                target: aggs[i].target,
                other: aggs[i].other,
                langs,
                git,
                commits,
                days,
                dirty,
            };
            let _ = tx.send(ScanEvent::Project(Box::new(project)));
            wake();
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
fn walk_all(dirs: &[PathBuf]) -> Vec<Agg> {
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
    }

    impl Queue {
        fn push(&self, t: Task) {
            self.pending.fetch_add(1, Ordering::Release);
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
        fn finished_one(&self) {
            self.pending.fetch_sub(1, Ordering::Release);
            // Only the end of the walk needs a broadcast; new work arriving is covered by
            // push's own notify.
            if self.pending.load(Ordering::Acquire) == 0 {
                self.cv.notify_all();
            }
        }
    }

    fn wide_eq_ci(w: &[u16], s: &str) -> bool {
        w.len() == s.len()
            && w.iter().zip(s.as_bytes()).all(|(&a, &b)| {
                a == b as u16 || a == (b.to_ascii_uppercase()) as u16
            })
    }

    fn wide_ext(w: &[u16]) -> Option<&[u16]> {
        let i = w.iter().rposition(|&c| c == b'.' as u16)?;
        if i == 0 { None } else { Some(&w[i + 1..]) }
    }

    fn wide_ends_ci(w: &[u16], s: &str) -> bool {
        let s = s.as_bytes();
        w.len() >= s.len()
            && w[w.len() - s.len()..]
                .iter()
                .zip(s)
                .all(|(&a, &b)| a == b as u16 || a == (b.to_ascii_uppercase()) as u16)
    }

    fn bucket_of_wide(name: &[u16]) -> Bucket {
        if wide_eq_ci(name, "node_modules") {
            return JUNK;
        }
        let mut b = 0;
        if wide_eq_ci(name, "target") || wide_eq_ci(name, "deriveddata") {
            b |= TARGET;
        }
        if wide_eq_ci(name, ".git") || BUILD_DIRS.iter().any(|d| wide_eq_ci(name, d)) {
            b |= OTHER;
        }
        b
    }

    /// The line-count gate, decided on the raw directory entry so only real candidates pay
    /// for a path allocation.
    fn code_candidate(name: &[u16], len: u64) -> bool {
        if len == 0 || len > MAX_READ {
            return false;
        }
        if JUNK_NAMES.iter().any(|j| wide_eq_ci(name, j))
            || wide_ends_ci(name, ".min.js")
            || wide_ends_ci(name, ".map")
        {
            return false;
        }
        match wide_ext(name) {
            Some(ext) => CODE_EXT.iter().any(|e| wide_eq_ci(ext, e)),
            None => false,
        }
    }

    let aggs: Arc<Mutex<Vec<Agg>>> =
        Arc::new(Mutex::new((0..dirs.len()).map(|_| Agg::default()).collect()));
    let queue = Arc::new(Queue {
        tasks: Mutex::new((Vec::new(), 0)),
        cv: Condvar::new(),
        pending: AtomicUsize::new(0),
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
        queue.push(Task { proj: i, dir, bucket: 0 });
    }

    let workers = 32;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue = queue.clone();
            let aggs = aggs.clone();
            scope.spawn(move || {
                lower_priority();
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
                                            let child_bucket =
                                                if wide_eq_ci(name, "node_modules") {
                                                    JUNK
                                                } else {
                                                    task.bucket | bucket_of_wide(name)
                                                };
                                            let mut child = task.dir.clone();
                                            child.push(OsString::from_wide(name));
                                            queue.push(Task {
                                                proj: task.proj,
                                                dir: child,
                                                bucket: child_bucket,
                                            });
                                        }
                                    } else {
                                        let own = bucket_of_wide(name);
                                        let bucket = if own & JUNK != 0 {
                                            JUNK
                                        } else {
                                            task.bucket | own
                                        };
                                        let candidate = code_candidate(name, len);
                                        let abs = candidate.then(|| {
                                            let mut p = task.dir.clone();
                                            p.push(OsString::from_wide(name));
                                            p
                                        });
                                        let lang = candidate.then(|| {
                                            wide_ext(name)
                                                .and_then(|ext| {
                                                    CODE_EXT.iter().find(|e| wide_eq_ci(ext, e))
                                                })
                                                .map(|e| canonical_lang(e, ""))
                                        });
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
                    queue.finished_one();
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
                rec(&e.path(), proj, bucket | bucket_of(&name), aggs);
            } else if ft.is_file() {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                aggs[proj].add(&name, len, bucket, Some(&e.path()));
            }
        }
    }
    let mut aggs: Vec<Agg> = (0..dirs.len()).map(|_| Agg::default()).collect();
    for (i, d) in dirs.iter().enumerate() {
        rec(d, i, 0, &mut aggs);
    }
    aggs
}




