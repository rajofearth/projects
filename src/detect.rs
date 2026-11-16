//! Cheap project detection: what a folder is built with, and what git says about it.
//!
//! `stack` reads manifests only (no git, no directory walk) so the cards can label a row
//! while a scan is still running. `inspect` does the full, blocking pass for the detail
//! view: git facts plus a shallow, bounded language walk. Every operation degrades to
//! defaults on error; a half-written manifest or a missing git never panics.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Everything the card/detail view needs to know about one project folder.
#[derive(Clone, Debug, Default)]
pub struct Info {
    /// Most specific first, e.g. `["Next.js", "React", "TypeScript", "Tailwind"]`.
    pub stack: Vec<&'static str>,
    /// `package.json` scripts (`name`, `command`); Cargo contributes build/run/test.
    pub scripts: Vec<(String, String)>,
    /// `"npm"`, `"pnpm"`, `"yarn"`, `"bun"`, `"cargo"`, `"uv"`, `"pip"`, `"go"`.
    pub package_manager: Option<&'static str>,
    /// Manifest description, else the first prose paragraph of README.md (≤ 200 chars).
    pub description: Option<String>,
    /// `origin` normalised to `https://host/owner/repo`.
    pub remote: Option<String>,
    /// `git status --porcelain` line count.
    pub dirty: u32,
    /// Commits ahead/behind the upstream (0 when there is no upstream).
    pub ahead: u32,
    pub behind: u32,
    /// Last 8 commits, newest first.
    pub commits: Vec<Commit>,
    /// Top 5 languages by file bytes as fractions of the counted bytes.
    pub languages: Vec<(&'static str, f32)>,
}

/// One commit from `git log`.
#[derive(Clone, Debug)]
pub struct Commit {
    pub hash: String,
    pub subject: String,
    pub author: String,
    pub unix: i64,
}

// ---- the manifest pass (shared by `stack` and `inspect`) ---------------------

#[derive(Default)]
struct Flags {
    next: bool,
    react: bool,
    react_native: bool,
    expo: bool,
    vite: bool,
    svelte: bool,
    vue: bool,
    astro: bool,
    electron: bool,
    tauri: bool,
    tailwind: bool,
    typescript: bool,
    rust: bool,
    python: bool,
    go: bool,
    java: bool,
    cpp: bool,
    csharp: bool,
    flutter: bool,
}

#[derive(Default)]
struct Manifest {
    flags: Flags,
    scripts: Vec<(String, String)>,
    package_manager: Option<&'static str>,
    description: Option<String>,
}

/// Compose the stack most-specific-first, with special care for the two examples that
/// rule out a single flat sort: an Expo app names React Native but not plain React, and
/// a Tauri app names Rust immediately after Tauri (its Cargo manifest is the primary one).
fn compose_stack(f: &Flags) -> Vec<&'static str> {
    let mut out = Vec::new();
    // Frameworks / app shells.
    if f.next {
        out.push("Next.js");
    }
    if f.expo {
        out.push("Expo");
    }
    if f.tauri {
        out.push("Tauri");
    }
    if f.electron {
        out.push("Electron");
    }
    if f.astro {
        out.push("Astro");
    }
    if f.svelte {
        out.push("Svelte");
    }
    if f.vue {
        out.push("Vue");
    }
    if f.vite {
        out.push("Vite");
    }
    // Rust sits next to Tauri when both are present.
    if f.tauri && f.rust {
        out.push("Rust");
    }
    if f.react_native {
        out.push("React Native");
    }
    if f.flutter {
        out.push("Flutter");
    }
    if f.rust && !f.tauri {
        out.push("Rust");
    }
    // React is implied by React Native, so it isn't repeated.
    if f.react && !f.react_native {
        out.push("React");
    }
    if f.typescript {
        out.push("TypeScript");
    }
    if f.python {
        out.push("Python");
    }
    if f.go {
        out.push("Go");
    }
    if f.java {
        out.push("Java");
    }
    if f.cpp {
        out.push("C++");
    }
    if f.csharp {
        out.push("C#");
    }
    if f.tailwind {
        out.push("Tailwind");
    }
    out
}

fn apply_deps(deps: &[String], f: &mut Flags) {
    for d in deps {
        match d.as_str() {
            "next" => f.next = true,
            "react" => f.react = true,
            "react-native" => f.react_native = true,
            "expo" => f.expo = true,
            "vite" => f.vite = true,
            "svelte" | "@sveltejs/kit" => f.svelte = true,
            "vue" | "nuxt" => f.vue = true,
            "astro" => f.astro = true,
            "electron" => f.electron = true,
            "@tauri-apps/api" | "@tauri-apps/cli" => f.tauri = true,
            "tailwindcss" => f.tailwind = true,
            "typescript" => f.typescript = true,
            _ => {}
        }
    }
}

/// Dependency names from one package.json, if it exists and parses.
fn pkg_deps(dir: &Path, out: &mut Vec<String>) {
    let text = match fs::read_to_string(dir.join("package.json")) {
        Ok(t) => t,
        Err(_) => return,
    };
    let value = match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) => v,
        Err(_) => return,
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return,
    };
    for key in ["dependencies", "devDependencies"] {
        if let Some(deps) = obj.get(key).and_then(|v| v.as_object()) {
            out.extend(deps.keys().cloned());
        }
    }
}

/// The package.json at the root, plus the immediate package.json of every `apps/*` and
/// `packages/*` child (the usual monorepo layouts).
fn all_deps(path: &Path) -> Vec<String> {
    let mut deps = Vec::new();
    pkg_deps(path, &mut deps);
    for group in ["apps", "packages"] {
        let root = path.join(group);
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    pkg_deps(&entry.path(), &mut deps);
                }
            }
        }
    }
    deps
}

/// `description = "..." / '...'` in the `[section]` that follows the given header.
fn toml_description(text: &str, section: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_section = t.trim_start_matches('[').starts_with(section);
            continue;
        }
        if !in_section || !t.starts_with("description") {
            continue;
        }
        let rest = t["description".len()..].trim_start();
        if !rest.starts_with('=') {
            continue;
        }
        let val = rest[1..].trim();
        if val.len() >= 2 {
            let q = val.as_bytes()[0] as char;
            if (q == '"' || q == '\'') && val.as_bytes()[val.len() - 1] as char == q {
                return Some(val[1..val.len() - 1].to_string());
            }
        }
    }
    None
}

fn read_manifests(path: &Path) -> Manifest {
    let mut m = Manifest::default();

    // package.json: flags, scripts, description.
    if let Ok(text) = fs::read_to_string(path.join("package.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(obj) = v.as_object() {
                if let Some(d) = obj.get("description").and_then(|x| x.as_str()) {
                    if !d.trim().is_empty() {
                        m.description = Some(d.trim().to_string());
                    }
                }
                if let Some(scripts) = obj.get("scripts").and_then(|x| x.as_object()) {
                    for (name, cmd) in scripts {
                        if let Some(c) = cmd.as_str() {
                            m.scripts.push((name.clone(), c.to_string()));
                        }
                    }
                }
            }
        }
    }

    // Cargo.toml: Rust, Tauri, description, and the three standard cargo scripts.
    if let Ok(text) = fs::read_to_string(path.join("Cargo.toml")) {
        m.flags.rust = true;
        if text.lines().any(|l| l.trim().starts_with("tauri")) {
            m.flags.tauri = true;
        }
        if m.description.is_none() {
            m.description = toml_description(&text, "package");
        }
        m.scripts.push(("build".into(), "cargo build".into()));
        m.scripts.push(("run".into(), "cargo run".into()));
        m.scripts.push(("test".into(), "cargo test".into()));
    }

    if path.join("pyproject.toml").is_file() || path.join("requirements.txt").is_file() {
        m.flags.python = true;
    }
    if m.description.is_none() {
        if let Ok(text) = fs::read_to_string(path.join("pyproject.toml")) {
            m.description = toml_description(&text, "project");
        }
    }
    if path.join("go.mod").is_file() {
        m.flags.go = true;
    }
    if path.join("pom.xml").is_file() || path.join("build.gradle").is_file() {
        m.flags.java = true;
    }
    if path.join("CMakeLists.txt").is_file() {
        m.flags.cpp = true;
    }
    if path.join("pubspec.yaml").is_file() {
        m.flags.flutter = true;
        if m.description.is_none() {
            if let Ok(text) = fs::read_to_string(path.join("pubspec.yaml")) {
                m.description = text.lines().find_map(|l| {
                    let t = l.trim();
                    t.strip_prefix("description:").map(|v| v.trim().to_string())
                });
            }
        }
    }
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            if entry
                .path()
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("csproj"))
            {
                m.flags.csharp = true;
                break;
            }
        }
    }

    apply_deps(&all_deps(path), &mut m.flags);

    // Lockfiles pick the package manager. Most specific (and most deliberate) first.
    let lockfiles: [(&str, &str); 8] = [
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lockb", "bun"),
        ("bun.lock", "bun"),
        ("package-lock.json", "npm"),
        ("Cargo.lock", "cargo"),
        ("uv.lock", "uv"),
        ("go.sum", "go"),
    ];
    m.package_manager = lockfiles
        .iter()
        .find(|(name, _)| path.join(name).is_file())
        .map(|(_, pm)| *pm);
    if m.package_manager.is_none() {
        // No lockfile yet: name the manager from the primary manifest so a freshly
        // created project still reports one.
        m.package_manager = if path.join("Cargo.toml").is_file() {
            Some("cargo")
        } else if path.join("go.mod").is_file() {
            Some("go")
        } else if path.join("requirements.txt").is_file() || path.join("pyproject.toml").is_file() {
            Some("pip")
        } else if path.join("package.json").is_file() {
            Some("npm")
        } else {
            None
        };
    }

    m
}

/// Fast label pass: manifest reads only, no git and no directory walk.
pub fn stack(path: &Path) -> Vec<&'static str> {
    compose_stack(&read_manifests(path).flags)
}

// ---- README description -----------------------------------------------------

fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Remove one bracketed markdown construct starting at `i` (`!` optional), returning
/// the index just past it. `keep_text` decides whether `[text](url)` keeps its label.
fn consume_link(c: &[char], i: usize, keep_text: bool, out: &mut String) -> usize {
    // i points at '['; find its matching ']'.
    let mut depth = 0usize;
    let mut j = i;
    while j < c.len() {
        if c[j] == '[' {
            depth += 1;
        } else if c[j] == ']' {
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
        j += 1;
    }
    if j >= c.len() {
        out.push('[');
        return i + 1;
    }
    let text: String = c[i + 1..j].iter().collect();
    let k = j + 1;
    if k < c.len() && (c[k] == '(' || c[k] == '[') {
        let open = c[k];
        let close = if open == '(' { ')' } else { ']' };
        let mut d = 0usize;
        let mut end = k + 1;
        while end < c.len() {
            if c[end] == open {
                d += 1;
            } else if c[end] == close {
                if d == 0 {
                    break;
                }
                d -= 1;
            }
            end += 1;
        }
        if keep_text {
            out.push_str(&text);
        }
        return if end < c.len() { end + 1 } else { c.len() };
    }
    if keep_text {
        out.push_str(&text);
    } else {
        out.push('[');
        out.push_str(&text);
        out.push(']');
    }
    j + 1
}

fn strip_markdown(s: &str) -> String {
    let plain = strip_html(s);
    let c: Vec<char> = plain.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '!' && i + 1 < c.len() && c[i + 1] == '[' {
            // An inline image is decoration: drop it entirely.
            i = consume_link(&c, i + 1, false, &mut String::new());
        } else if c[i] == '[' {
            i = consume_link(&c, i, true, &mut out);
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    // Inline emphasis and code markers.
    let out: String = out.chars().filter(|ch| !matches!(ch, '*' | '_' | '~' | '`')).collect();
    collapse_ws(strip_list_marker(&out))
}

/// Remove a single leading markdown list/quote marker, leaving ordinary prose that
/// happens to begin with a digit untouched.
fn strip_list_marker(s: &str) -> &str {
    let t = s.trim_start();
    for marker in ["- ", "* ", "+ ", "> "] {
        if let Some(rest) = t.strip_prefix(marker) {
            return rest;
        }
    }
    let bytes = t.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b' ' {
        return &t[i + 2..];
    }
    t
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            out.push(ch);
            space = false;
        }
    }
    out
}

fn is_non_prose(t: &str) -> bool {
    t.starts_with('#')
        || t.starts_with("```")
        || t.starts_with("![")
        || t.contains("![")
        || t.starts_with("<img")
        || t.starts_with("<!--")
        || t.starts_with("<div")
        || t.starts_with("<p ")
        || t.starts_with("---")
        || t.starts_with("***")
        || t.starts_with("___")
        || t.starts_with('|')
}

/// First prose paragraph of README.md with headings, badges, images, links and HTML
/// stripped, collapsed to one line and capped at 200 characters.
fn readme_description(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path.join("README.md")).ok()?;
    let mut paragraph: Vec<String> = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if t.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if is_non_prose(t) {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        let clean = strip_markdown(t);
        if clean.is_empty() {
            continue;
        }
        paragraph.push(clean);
    }
    if paragraph.is_empty() {
        return None;
    }
    let joined = collapse_ws(&paragraph.join(" "));
    let capped: String = joined.chars().take(200).collect();
    if capped.is_empty() {
        None
    } else {
        Some(capped)
    }
}

// ---- git --------------------------------------------------------------------

fn git(path: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(path).args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("GIT_PAGER", "cat");
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

/// `git@host:owner/repo.git`, `https://…`, `ssh://…` → `https://host/owner/repo`.
fn normalize_remote(url: &str) -> Option<String> {
    let u = url.trim().trim_end_matches(".git").trim_end_matches('/');
    let host_path = if let Some(r) = u.strip_prefix("git@") {
        r.replacen(':', "/", 1)
    } else if let Some(r) = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")) {
        r.to_string()
    } else if let Some(r) = u.strip_prefix("ssh://") {
        let after_user = r.rsplit('@').next().unwrap_or(r);
        // Drop an optional port (host:22/owner/repo).
        let host = after_user.split('/').next().unwrap_or("");
        let host = host.split(':').next().unwrap_or(host);
        format!("{}{}", host, after_user.strip_prefix(host).unwrap_or(""))
    } else {
        return None;
    };
    // Strip any surviving userinfo and query string.
    let host_path = host_path.rsplit('@').next().unwrap_or(&host_path).to_string();
    let host_path = host_path.split('?').next().unwrap_or("").trim_matches('/');
    let parts: Vec<&str> = host_path.split('/').collect();
    if parts.len() < 3 || parts[0].is_empty() || parts[1].is_empty() || parts[2].is_empty() {
        return None;
    }
    Some(format!("https://{}/{}", parts[0], parts[1..3].join("/")))
}

fn git_info(path: &Path, info: &mut Info) {
    if !path.join(".git").exists() {
        // A worktree's .git is a file; both are handled by git itself, but skip the
        // obvious non-repo case cheaply.
        if git(path, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
            return;
        }
    }
    if let Some(url) = git(path, &["remote", "get-url", "origin"]) {
        info.remote = normalize_remote(&url);
    }
    if let Some(out) = git(path, &["status", "--porcelain"]) {
        info.dirty = out.lines().count() as u32;
    }
    if let Some(out) = git(path, &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"]) {
        let mut nums = out.split_whitespace();
        info.ahead = nums.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        info.behind = nums.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    }
    if let Some(out) = git(path, &[
        "log",
        "-8",
        "--pretty=tformat:%H%x1f%s%x1f%an%x1f%ct",
    ]) {
        for line in out.lines() {
            let f: Vec<&str> = line.split('\x1f').collect();
            if f.len() >= 4 {
                info.commits.push(Commit {
                    hash: f[0].to_string(),
                    subject: f[1].to_string(),
                    author: f[2].to_string(),
                    unix: f[3].parse().unwrap_or(0),
                });
            }
        }
    }
}

// ---- languages --------------------------------------------------------------

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "dist",
    "build",
    ".next",
    "venv",
    ".venv",
    "__pycache__",
];

fn language_of(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "rs" => "Rust",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "cpp" | "cc" | "cxx" | "h" | "hpp" | "hh" => "C++",
        "c" => "C",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "dart" => "Dart",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "css" | "scss" | "less" => "CSS",
        "html" | "htm" => "HTML",
        "md" => "Markdown",
        "json" => "JSON",
        "toml" => "TOML",
        "yaml" | "yml" => "YAML",
        "sh" | "bat" | "ps1" => "Shell",
        "sql" => "SQL",
        _ => return None,
    })
}

fn walk_languages(dir: &Path, depth: u32, files: &mut u32, bytes: &mut HashMap<&'static str, u64>) {
    if depth > 4 || *files >= 3000 {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if *files >= 3000 {
            break;
        }
        let kind = match entry.file_type() {
            Ok(k) => k,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if kind.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) || name.starts_with('.') {
                continue;
            }
            walk_languages(&entry.path(), depth + 1, files, bytes);
        } else if kind.is_file() {
            if let Some(lang) = language_of(&name) {
                if let Ok(md) = entry.metadata() {
                    *bytes.entry(lang).or_insert(0) += md.len();
                    *files += 1;
                }
            }
        }
    }
}

fn languages(path: &Path) -> Vec<(&'static str, f32)> {
    let mut bytes: HashMap<&'static str, u64> = HashMap::new();
    let mut files = 0u32;
    walk_languages(path, 0, &mut files, &mut bytes);
    let total: u64 = bytes.values().sum();
    if total == 0 {
        return Vec::new();
    }
    let mut out: Vec<(&'static str, f32)> = bytes
        .into_iter()
        .map(|(lang, b)| (lang, b as f32 / total as f32))
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.truncate(5);
    out
}

/// Full, blocking inspection of one project folder.
pub fn inspect(path: &Path) -> Info {
    let m = read_manifests(path);
    let description = m.description.clone().or_else(|| readme_description(path));
    let mut info = Info {
        stack: compose_stack(&m.flags),
        scripts: m.scripts,
        package_manager: m.package_manager,
        description,
        languages: languages(path),
        ..Info::default()
    };
    git_info(path, &mut info);
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "projects-detect-{}-{}-{}",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &std::path::Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn nextjs_package_json_gives_a_specific_stack_and_scripts() {
        let dir = temp_dir();
        write(
            &dir,
            "package.json",
            r#"{
              "description": "A Next app",
              "scripts": { "dev": "next dev", "build": "next build" },
              "dependencies": { "next": "14", "react": "18", "react-dom": "18", "tailwindcss": "3" },
              "devDependencies": { "typescript": "5" }
            }"#,
        );
        write(&dir, "package-lock.json", "{}");

        let s = stack(&dir);
        assert_eq!(s.first(), Some(&"Next.js"), "stack: {s:?}");
        assert!(s.contains(&"React"), "stack: {s:?}");
        assert!(s.contains(&"TypeScript"), "stack: {s:?}");
        assert!(s.contains(&"Tailwind"), "stack: {s:?}");

        let info = inspect(&dir);
        assert!(info.scripts.contains(&("dev".into(), "next dev".into())));
        assert!(info.scripts.contains(&("build".into(), "next build".into())));
        assert_eq!(info.package_manager, Some("npm"));
        assert_eq!(info.description.as_deref(), Some("A Next app"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cargo_toml_gives_rust_and_cargo_scripts() {
        let dir = temp_dir();
        write(
            &dir,
            "Cargo.toml",
            "[package]\nname = \"demo\"\ndescription = \"A rust tool\"\n\n[dependencies]\nserde = \"1\"\n",
        );

        assert_eq!(stack(&dir), ["Rust"]);
        let info = inspect(&dir);
        assert_eq!(
            info.scripts,
            [
                ("build".into(), "cargo build".into()),
                ("run".into(), "cargo run".into()),
                ("test".into(), "cargo test".into()),
            ]
        );
        assert_eq!(info.package_manager, Some("cargo"));
        assert_eq!(info.description.as_deref(), Some("A rust tool"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn readme_description_strips_markdown_and_badges() {
        let dir = temp_dir();
        write(
            &dir,
            "README.md",
            "# Title\n\n[![Build](https://img.shields.io/badge/pass)](https://ci.example)\n\nA **cool** tool that does [things](https://example.com) fast. <br>\nStill the same paragraph.\n\n## Next section\n\nOther words.\n",
        );

        let info = inspect(&dir);
        let desc = info.description.unwrap_or_default();
        assert!(!desc.contains('#'), "heading leaked: {desc:?}");
        assert!(!desc.contains('['), "link syntax leaked: {desc:?}");
        assert!(!desc.contains('<'), "html leaked: {desc:?}");
        assert!(!desc.contains("Title") && !desc.contains("Next section"), "headings: {desc:?}");
        assert!(desc.contains("A cool tool that does things fast."), "desc: {desc:?}");
        assert!(desc.len() <= 200, "too long: {}", desc.len());
        fs::remove_dir_all(&dir).ok();
    }
}
