//! User-defined project templates: named recipes for creating a new project,
//! each a sequence of shell commands run in (or beside) the new project folder.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::Mutex;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Template {
    pub id: String,
    pub name: String,
    /// Short stack label, e.g. "nextjs", "rust", "folder".
    pub kind: String,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub run: String,
    /// true: run in the parent folder (for generators that create the folder
    /// themselves, like create-next-app); false: run inside the new project folder.
    pub in_parent: bool,
}

/// Progress events streamed from [`run`]'s worker thread.
pub enum Event {
    /// One line of step output (stdout or stderr).
    Line(String),
    /// A step is about to start (index into `steps`).
    StepStarted(usize),
    /// The run finished: the project path on success, or the error.
    Done(std::io::Result<PathBuf>),
}

/// Where the template list is stored. Overridable in tests so they never touch
/// the real `%APPDATA%` file.
static PATH_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// `%APPDATA%\projects\templates.json`.
pub fn path() -> PathBuf {
    if let Some(p) = PATH_OVERRIDE.lock().unwrap().as_ref() {
        return p.clone();
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("projects").join("templates.json")
}

/// Load the template list. Missing or corrupt file: return the defaults and
/// (best-effort) write them out.
pub fn load() -> Vec<Template> {
    match std::fs::read(path()) {
        Ok(bytes) => match serde_json::from_slice::<Vec<Template>>(&bytes) {
            Ok(list) => return list,
            Err(_) => {}
        },
        Err(_) => {}
    }
    let defaults = defaults();
    let _ = save(&defaults);
    defaults
}

/// Persist the template list, creating the storage directory if needed.
pub fn save(list: &[Template]) -> std::io::Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(list).map_err(std::io::Error::other)?;
    std::fs::write(p, json)
}

/// The built-in template list.
pub fn defaults() -> Vec<Template> {
    vec![
        Template {
            id: "empty-folder".into(),
            name: "Empty folder".into(),
            kind: "folder".into(),
            steps: vec![],
        },
        Template {
            id: "nextjs".into(),
            name: "Next.js".into(),
            kind: "nextjs".into(),
            steps: vec![Step {
                run: "npx --yes create-next-app@latest {name} --ts --app --eslint --tailwind --src-dir --use-npm --yes".into(),
                in_parent: true,
            }],
        },
        Template {
            id: "vite-react".into(),
            name: "Vite + React".into(),
            kind: "vite".into(),
            steps: vec![
                Step {
                    run: "npm create vite@latest {name} -- --template react-ts".into(),
                    in_parent: true,
                },
                Step {
                    run: "npm install".into(),
                    in_parent: false,
                },
            ],
        },
        Template {
            id: "rust-bin".into(),
            name: "Rust binary".into(),
            kind: "rust".into(),
            steps: vec![Step {
                run: "cargo new {name}".into(),
                in_parent: true,
            }],
        },
        Template {
            id: "rust-lib".into(),
            name: "Rust library".into(),
            kind: "rust".into(),
            steps: vec![Step {
                run: "cargo new --lib {name}".into(),
                in_parent: true,
            }],
        },
        Template {
            id: "python-uv".into(),
            name: "Python (uv)".into(),
            kind: "python".into(),
            steps: vec![Step {
                run: "uv init {name}".into(),
                in_parent: true,
            }],
        },
        Template {
            id: "expo".into(),
            name: "Expo app".into(),
            kind: "expo".into(),
            steps: vec![Step {
                run: "npx --yes create-expo-app@latest {name}".into(),
                in_parent: true,
            }],
        },
    ]
}

/// Run a template: validate the name, create the folder if no step does it,
/// then execute each step via `cmd /C`, streaming output lines through `tx`.
/// Returns immediately; the final [`Event::Done`] carries the project path or
/// the error.
pub fn run(t: &Template, parent: &Path, name: &str, git_init: bool, tx: Sender<Event>) {
    let t = t.clone();
    let parent = parent.to_path_buf();
    let name = name.to_string();
    std::thread::spawn(move || {
        let result = execute(&t, &parent, &name, git_init, &tx);
        let _ = tx.send(Event::Done(result));
    });
}

fn execute(
    t: &Template,
    parent: &Path,
    name: &str,
    git_init: bool,
    tx: &Sender<Event>,
) -> std::io::Result<PathBuf> {
    if let Err(msg) = validate_name(name) {
        return Err(std::io::Error::new(ErrorKind::InvalidInput, msg));
    }
    let abs_parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    let project = abs_parent.join(name);
    if project.symlink_metadata().is_ok() {
        return Err(std::io::Error::new(
            ErrorKind::AlreadyExists,
            format!("target already exists: {}", project.display()),
        ));
    }
    // If the first step doesn't create the folder itself, we must.
    let create_dir = t.steps.first().is_none_or(|s| !s.in_parent);
    if create_dir {
        std::fs::create_dir_all(&project)?;
    }
    for (i, step) in t.steps.iter().enumerate() {
        let _ = tx.send(Event::StepStarted(i));
        let cwd = if step.in_parent {
            abs_parent.clone()
        } else {
            project.clone()
        };
        let cmd_text = subst(&step.run, name, &project);
        run_step(&cmd_text, &cwd, tx)
            .map_err(|e| annotate_step(i, &step.run, e))?;
    }
    if git_init && !project.join(".git").exists() {
        run_git_init(&project)?;
    }
    Ok(project)
}

fn annotate_step(i: usize, raw: &str, e: std::io::Error) -> std::io::Error {
    let msg = if e.to_string().starts_with("step ") {
        e.to_string()
    } else {
        format!("step {} (`{}`) failed: {}", i, raw, e)
    };
    std::io::Error::other(msg)
}

fn run_step(cmd_text: &str, cwd: &Path, tx: &Sender<Event>) -> std::io::Result<()> {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(cmd_text);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| std::io::Error::other(format!("could not start step (`{}`): {}", cmd_text, e)))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let tx_out = tx.clone();
    let tx_err = tx.clone();
    let out_reader = std::thread::spawn(move || {
        if let Some(s) = stdout {
            for line in BufReader::new(s).lines() {
                match line {
                    Ok(l) => {
                        if tx_out.send(Event::Line(l)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });
    let err_reader = std::thread::spawn(move || {
        if let Some(s) = stderr {
            for line in BufReader::new(s).lines() {
                match line {
                    Ok(l) => {
                        if tx_err.send(Event::Line(l)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });

    let status = child.wait()?;
    let _ = out_reader.join();
    let _ = err_reader.join();
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(std::io::Error::other(format!(
            "step (`{}`) failed with exit code {}",
            cmd_text, code
        ))),
        None => Err(std::io::Error::other(format!(
            "step (`{}`) was terminated",
            cmd_text
        ))),
    }
}

fn run_git_init(project: &Path) -> std::io::Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("init");
    cmd.current_dir(project);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .map_err(|e| std::io::Error::other(format!("could not start `git init`: {}", e)))?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        let msg = msg.trim();
        let msg = if msg.is_empty() {
            "git init failed"
        } else {
            msg
        };
        return Err(std::io::Error::other(format!("git init failed: {}", msg)));
    }
    Ok(())
}

/// Replace `{name}` and `{path}` (absolute project path) in a command.
fn subst(cmd: &str, name: &str, project: &Path) -> String {
    cmd.replace("{name}", name)
        .replace("{path}", &project.to_string_lossy())
}

/// Windows-invalid filename characters, plus the ones `cmd` treats specially
/// (the name is substituted into `cmd /C` steps).
const INVALID_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*', '&', '^', '%', '!'];

const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("project name is empty".into());
    }
    if name != name.trim() {
        return Err("project name has leading or trailing whitespace".into());
    }
    if let Some(c) = name
        .chars()
        .find(|c| INVALID_CHARS.contains(c) || (*c as u32) < 32)
    {
        return Err(format!("project name contains invalid character {:?}", c));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err("project name must not end with a dot or space".into());
    }
    let base = name.split('.').next().unwrap_or(name);
    if RESERVED_NAMES
        .iter()
        .any(|r| base.eq_ignore_ascii_case(r))
    {
        return Err(format!("{} is a reserved Windows device name", base));
    }
    Ok(())
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn name_validation() {
        assert!(validate_name("my-app").is_ok());
        assert!(validate_name("my app 2").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("a<b").is_err());
        assert!(validate_name("a:b").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\\b").is_err());
        assert!(validate_name("a|b").is_err());
        assert!(validate_name("a?b").is_err());
        assert!(validate_name("a*b").is_err());
        assert!(validate_name("a\"b").is_err());
        assert!(validate_name("trailing.").is_err());
        assert!(validate_name("trailing ").is_err());
        assert!(validate_name("con").is_err());
        assert!(validate_name("CON.txt").is_err());
        assert!(validate_name("lpt3").is_err());
        assert!(validate_name(".env").is_ok());
    }

    #[test]
    fn placeholder_substitution() {
        let project = PathBuf::from("P:\\parent\\demo");
        assert_eq!(
            subst("echo {name} at {path}", "demo", &project),
            "echo demo at P:\\parent\\demo"
        );
        assert_eq!(subst("npm install", "demo", &project), "npm install");
    }

    #[test]
    fn load_save_roundtrip() {
        let dir = temp_dir("roundtrip");
        let file = dir.join("templates.json");
        set_path_override(Some(file.clone()));

        // Missing file: load() writes the defaults and returns them.
        let first = load();
        assert_eq!(first, defaults());
        assert!(file.is_file());

        // Modify + save, then reload: roundtrip preserves the list.
        let mut second = load();
        second[0].name = "Blank".into();
        second.push(Template {
            id: "custom".into(),
            name: "Custom".into(),
            kind: "custom".into(),
            steps: vec![Step {
                run: "echo {name}".into(),
                in_parent: false,
            }],
        });
        save(&second).unwrap();
        assert_eq!(load(), second);

        // Corrupt file: load() recovers with defaults.
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(load(), defaults());

        set_path_override(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_streams_lines_and_finishes() {
        let parent = temp_dir("run");
        let t = Template {
            id: "t".into(),
            name: "Echo test".into(),
            kind: "test".into(),
            steps: vec![Step {
                run: "echo hi".into(),
                in_parent: false,
            }],
        };
        let (tx, rx) = mpsc::channel::<Event>();
        run(&t, &parent, "echo-run-target", false, tx);

        let mut lines = Vec::new();
        let mut done: Option<std::io::Result<PathBuf>> = None;
        let deadline = Duration::from_secs(60);
        loop {
            match rx.recv_timeout(deadline) {
                Ok(Event::Line(l)) => lines.push(l),
                Ok(Event::StepStarted(_)) => {}
                Ok(Event::Done(r)) => {
                    done = Some(r);
                    break;
                }
                Err(_) => panic!("template run hung"),
            }
        }
        assert!(lines.iter().any(|l| l == "hi"), "got {:?}", lines);
        let path = done.expect("no Done event").expect("run failed");
        assert_eq!(path.file_name().unwrap(), "echo-run-target");
        assert!(path.is_dir());
        let _ = std::fs::remove_dir_all(&parent);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "projects-stats-templates-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(test)]
    fn set_path_override(p: Option<PathBuf>) {
        *PATH_OVERRIDE.lock().unwrap() = p;
    }
}
