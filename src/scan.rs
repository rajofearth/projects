//! The background scanner: walks every top-level folder under the projects root in parallel,
//! classifies bytes into source vs build junk, counts hand-written lines per language, and asks
//! git for the facts. Progress streams to the UI thread as [`ScanEvent`]s.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;
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

#[derive(Default)]
struct Agg {
    bytes: u64,
    files: u64,
    loc: u64,
    src: u64,
    nm: u64,
    target: u64,
    other: u64,
    langs: HashMap<String, u64>,
}

impl Agg {
    fn add_file(&mut self, rel: &Path, abs: &Path, len: u64) {
        self.bytes += len;
        self.files += 1;

        let lower: Vec<String> = rel
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .map(|s| s.to_ascii_lowercase())
            .collect();
        let bucket_junk = lower.iter().any(|c| c == "node_modules");
        let bucket_target = lower.iter().any(|c| c == "target" || c == "deriveddata");
        let bucket_other = lower
            .iter()
            .any(|c| BUILD_DIRS.contains(&c.as_str()) || c == ".git");

        if bucket_junk {
            self.nm += len;
            return;
        }
        if bucket_target {
            self.target += len;
            return;
        }
        if bucket_other {
            self.other += len;
            return;
        }
        self.src += len;

        // Hand-written lines: known code extension, small enough, not a lock file.
        let name = abs.file_name().map(|s| s.to_string_lossy().to_lowercase());
        let Some(name) = name else { return };
        if JUNK_NAMES.contains(&name.as_str()) || name.ends_with(".min.js") || name.ends_with(".map")
        {
            return;
        }
        let ext = name.rsplit('.').next().unwrap_or("");
        if !CODE_EXT.contains(&ext) || len > MAX_READ || len == 0 {
            return;
        }
        let lines = count_lines(abs);
        if lines > 0 {
            *self.langs.entry(canonical_lang(ext, &name).to_string()).or_insert(0) += lines;
            self.loc += lines;
        }
    }
}

/// `.cc`/`.cpp`/`.cxx`/`.hpp` fold into their display names; `.d.ts` stays TypeScript.
fn canonical_lang(ext: &str, name: &str) -> &'static str {
    match ext {
        "tsx" | "jsx" => "TSX",
        "ts" => "TypeScript",
        "js" | "mjs" | "cjs" => "JavaScript",
        "rs" => "Rust",
        "py" => "Python",
        "cc" | "cpp" | "cxx" => "C++",
        "hpp" | "hh" => "C/C++ headers",
        "h" => {
            if name.ends_with(".h") && name.contains("mm") {
                "C"
            } else {
                "C"
            }
        }
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
    let mut chunk = [0u8; 64 * 1024];
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

/// Walk one project folder with `ignore`, in parallel. Nothing is filtered: hidden files
/// (`.git` history) and ignored files (`node_modules`, `target`) are real bytes on disk, and
/// line counting is gated by extension and size anyway, so junk never gets read.
fn walk_project(dir: &Path) -> Agg {
    let agg = std::sync::Mutex::new(Agg::default());
    let mut builder = ignore::WalkBuilder::new(dir);
    builder.hidden(false).git_ignore(false).git_global(false).ignore(false).threads(4);
    builder.build_parallel().run(|| {
        let agg = &agg;
        Box::new(move |entry| {
            let Ok(entry) = entry else { return ignore::WalkState::Continue };
            if entry.file_type().is_some_and(|t| t.is_file()) {
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let abs = entry.path().to_path_buf();
                let rel = abs.strip_prefix(dir).unwrap_or(&abs).to_path_buf();
                agg.lock().unwrap().add_file(&rel, &abs, len);
            }
            ignore::WalkState::Continue
        })
    });
    agg.into_inner().unwrap()
}

fn git_output(dir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
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

fn git_facts(dir: &Path) -> (bool, u64, Option<u64>, u64) {
    if !dir.join(".git").exists() {
        return (false, 0, None, 0);
    }
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
    let dirty = git_output(dir, &["status", "--porcelain"])
        .map(|s| s.lines().count() as u64)
        .unwrap_or(0);
    (true, commits, days, dirty)
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
    let mut best: Option<(&String, u64)> = None;
    for (lang, lines) in &agg.langs {
        if best.is_none_or(|b| *lines > b.1) {
            best = Some((lang, *lines));
        }
    }
    match best.map(|(l, _)| l.as_str()) {
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

/// Full scan of `root`, streaming one event per finished project. Runs on its own thread.
pub fn scan(root: PathBuf, tx: Sender<ScanEvent>, wake: impl Fn() + Send + Sync + 'static) {
    let started = Instant::now();
    let dirs = list_projects(&root);
    if dirs.is_empty() {
        let _ = tx.send(ScanEvent::Failed(format!("no folders under {}", root.display())));
        wake();
        return;
    }
    let _ = tx.send(ScanEvent::Start { total: dirs.len() });
    wake();

    let total = dirs.len();
    let _done = 0usize;
    let chunks = 8.min(total);
    let per_chunk = total.div_ceil(chunks);
    std::thread::scope(|scope| {
        for group in dirs.chunks(per_chunk) {
            let tx = tx.clone();
            let wake = &wake;
            scope.spawn(move || {
                for dir in group {
                    let name = dir
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let agg = walk_project(dir);
                    let (git, commits, days, dirty) = git_facts(dir);
                    let (cat, stack) = classify(dir, &agg);
                    let mut langs: Vec<(String, u64)> = agg.langs.into_iter().collect();
                    langs.sort_by(|a, b| b.1.cmp(&a.1));
                    langs.truncate(8);
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
                        git,
                        commits,
                        days,
                        dirty,
                    };
                    let _ = tx.send(ScanEvent::Project(Box::new(project)));
                    wake();
                }
            });
        }
    });
    let _ = tx.send(ScanEvent::Done {
        seconds: started.elapsed().as_secs_f64(),
    });
    wake();
}
