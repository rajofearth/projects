//! Opening a project folder in the user's tools: detected editors and terminals, and
//! creating a new project folder. Launches are detached; nothing waits on the child.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// An editor or IDE found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub id: &'static str,
    pub name: &'static str,
}

/// An app that was detected, with the executable (or `.cmd` shim) that launches it.
struct Found {
    app: App,
    launcher: PathBuf,
}

/// Where to look for an app: CLI names on PATH first, then known install paths
/// relative to `%LOCALAPPDATA%`.
struct Spec {
    id: &'static str,
    name: &'static str,
    clis: &'static [&'static str],
    files: &'static [&'static str],
}

const SPECS: &[Spec] = &[
    Spec {
        id: "vscode",
        name: "VS Code",
        clis: &["code"],
        files: &[r"Programs\Microsoft VS Code\bin\code.cmd"],
    },
    Spec {
        id: "cursor",
        name: "Cursor",
        clis: &["cursor"],
        files: &[r"Programs\cursor\resources\app\bin\cursor.cmd"],
    },
    Spec {
        id: "zed",
        name: "Zed",
        clis: &["zed"],
        files: &[r"Programs\Zed\bin\zed.exe", r"Programs\Zed\Zed.exe"],
    },
    Spec {
        id: "windsurf",
        name: "Windsurf",
        clis: &["windsurf"],
        files: &[r"Programs\Windsurf\bin\windsurf.cmd"],
    },
    Spec {
        id: "t3code",
        name: "T3 Code",
        clis: &["t3code"],
        files: &[
            r"Programs\t3code\T3 Code.exe",
            r"Programs\T3 Code\T3 Code.exe",
            r"Programs\t3code\t3code.exe",
        ],
    },
    Spec {
        id: "synara",
        name: "Synara",
        clis: &["synara"],
        files: &[
            r"Programs\synara\Synara.exe",
            r"Programs\Synara\Synara.exe",
            r"Programs\synara\synara.exe",
        ],
    },
    Spec {
        id: "idea",
        name: "IntelliJ IDEA",
        clis: &["idea"],
        files: &[r"JetBrains\Toolbox\scripts\idea.cmd"],
    },
    Spec {
        id: "webstorm",
        name: "WebStorm",
        clis: &["webstorm"],
        files: &[r"JetBrains\Toolbox\scripts\webstorm.cmd"],
    },
];

static INSTALLED: OnceLock<Vec<Found>> = OnceLock::new();

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// Apps detected on this machine. Detection runs once; later calls return the cached list.
pub fn installed() -> Vec<App> {
    detected().iter().map(|f| f.app.clone()).collect()
}

/// Open `path` in the detected app `app_id`, detached.
pub fn open_in(app_id: &str, path: &Path) -> io::Result<()> {
    require_dir(path)?;
    let found = detected()
        .iter()
        .find(|f| f.app.id == app_id)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("app not detected: {app_id}"))
        })?;
    let mut cmd = if is_shim(&found.launcher) {
        shim_command(&found.launcher, path)
    } else {
        let mut c = Command::new(&found.launcher);
        c.arg(path);
        c
    };
    spawn_detached(&mut cmd, 0)
}

/// Open a terminal at `path`: Windows Terminal (`wt -d`) when present, otherwise `cmd`.
pub fn open_terminal(path: &Path) -> io::Result<()> {
    require_dir(path)?;
    if find_on_path("wt").is_some() {
        let mut cmd = Command::new("wt");
        cmd.arg("-d").arg(path);
        return spawn_detached(&mut cmd, 0);
    }
    let mut cmd = Command::new("cmd");
    cmd.current_dir(path);
    #[cfg(windows)]
    let flags = CREATE_NEW_CONSOLE;
    #[cfg(not(windows))]
    let flags = 0;
    spawn_detached(&mut cmd, flags)
}

/// Create `parent\name` as a new folder, optionally running `git init` in it.
///
/// Fails if `name` is not a valid Windows folder name, if `parent` is not a folder, or if
/// the target already exists. If `git init` fails, the folder is left in place.
pub fn create_project(parent: &Path, name: &str, git_init: bool) -> io::Result<PathBuf> {
    validate_name(name)?;
    require_dir(parent)?;
    let target = parent.join(name);
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("already exists: {}", target.display()),
        ));
    }
    fs::create_dir(&target)?;
    if git_init {
        let mut cmd = Command::new("git");
        cmd.arg("init").current_dir(&target);
        hide_window(&mut cmd);
        let status = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "git init failed in {}",
                target.display()
            )));
        }
    }
    Ok(target)
}

// ---- detection ----

fn detected() -> &'static [Found] {
    INSTALLED.get_or_init(detect)
}

fn detect() -> Vec<Found> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    SPECS
        .iter()
        .filter_map(|spec| {
            let launcher = spec
                .clis
                .iter()
                .find_map(|cli| find_on_path(cli))
                .or_else(|| {
                    let base = local.as_ref()?;
                    spec.files
                        .iter()
                        .map(|f| base.join(f))
                        .find(|p| p.is_file())
                })?;
            Some(Found {
                app: App {
                    id: spec.id,
                    name: spec.name,
                },
                launcher,
            })
        })
        .collect()
}

/// First PATH match for `cli` that can be executed directly. `where` also lists the
/// extensionless POSIX shims that npm-style installers drop, which Windows cannot run,
/// so only `.exe`, `.cmd` and `.bat` hits count.
fn find_on_path(cli: &str) -> Option<PathBuf> {
    let mut cmd = Command::new("where.exe");
    cmd.arg(cli);
    hide_window(&mut cmd);
    let out = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| PathBuf::from(l.trim()))
        .find(|p| is_runnable(p))
}

fn is_runnable(p: &Path) -> bool {
    matches!(ext_lower(p).as_deref(), Some("exe" | "cmd" | "bat")) && p.is_file()
}

fn is_shim(p: &Path) -> bool {
    matches!(ext_lower(p).as_deref(), Some("cmd" | "bat"))
}

fn ext_lower(p: &Path) -> Option<String> {
    p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

// ---- process helpers ----

/// `.cmd` and `.bat` files only run through `cmd`. `/S /C ""exe" "folder""` makes cmd
/// strip exactly the outer quotes, so paths with spaces survive.
fn shim_command(launcher: &Path, folder: &Path) -> Command {
    let mut cmd = Command::new("cmd");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(format!(
            "/S /C \"\"{}\" \"{}\"\"",
            launcher.display(),
            folder.display()
        ));
    }
    #[cfg(not(windows))]
    {
        cmd.arg("/C").arg(launcher).arg(folder);
    }
    cmd
}

/// Spawn without waiting and without a console window, with `extra_flags` added.
/// The child is dropped, which does not kill it.
fn spawn_detached(cmd: &mut Command, extra_flags: u32) -> io::Result<()> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | extra_flags);
    }
    #[cfg(not(windows))]
    let _ = extra_flags;
    cmd.spawn().map(drop)
}

fn hide_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

fn require_dir(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("not a folder: {}", path.display()),
        ))
    }
}

// ---- name validation ----

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Checks a folder name against Windows rules: no reserved characters or control
/// characters, no trailing dot or space, not `.`/`..`, not a reserved device name, and at
/// most 255 characters.
fn validate_name(name: &str) -> io::Result<()> {
    let bad = |why: &str| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid project name {name:?}: {why}"),
        )
    };
    if name.trim().is_empty() {
        return Err(bad("empty"));
    }
    if name != name.trim() {
        return Err(bad("leading or trailing spaces"));
    }
    if name.ends_with('.') {
        return Err(bad("ends with a dot"));
    }
    if name.chars().count() > 255 {
        return Err(bad("longer than 255 characters"));
    }
    if name
        .chars()
        .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return Err(bad("contains a character Windows does not allow"));
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return Err(bad("reserved device name"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        for name in ["my-app", "Project 2", "a.b", "naïve", "CONSOLE", "com10", "_x"] {
            assert!(validate_name(name).is_ok(), "should accept {name:?}");
        }
    }

    #[test]
    fn rejects_bad_names() {
        let long = "x".repeat(256);
        let bad = [
            "", "   ", " lead", "trail ", "x.", ".", "..", "a/b", "a\\b", "a:b", "a*b", "a?b",
            "a|b", "a\"b", "a<b", "a\u{7}b", "CON", "nul.txt", "com1", "LPT9.md", long.as_str(),
        ];
        for name in bad {
            let err = validate_name(name).expect_err(&format!("should reject {name:?}"));
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{name:?}");
        }
    }

    #[test]
    fn create_project_makes_folder_and_rejects_duplicates() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let parent = std::env::temp_dir().join(format!(
            "launch-test-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&parent).unwrap();

        let made = create_project(&parent, "demo", false).unwrap();
        assert!(made.is_dir());
        assert_eq!(made, parent.join("demo"));

        let dup = create_project(&parent, "demo", false).unwrap_err();
        assert_eq!(dup.kind(), io::ErrorKind::AlreadyExists);

        let bad = create_project(&parent, "a/b", false).unwrap_err();
        assert_eq!(bad.kind(), io::ErrorKind::InvalidInput);

        let missing = create_project(&parent.join("nope"), "x", false).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);

        fs::remove_dir_all(&parent).unwrap();
    }
}
