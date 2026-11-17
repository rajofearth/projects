//! Installing, and how this process decides what it is.
//!
//! One exe does everything. Downloaded and opened, it shows the install window, copies itself
//! into a per-user folder, adds a Start menu shortcut and an entry under Settings, Apps, then
//! starts the installed copy. Started from that folder, it is the app, and it updates itself.
//! `--uninstall` (what Settings, Apps runs) removes it again.
//!
//! Nothing here needs admin rights: the folder is the user's, the registry key is under
//! HKEY_CURRENT_USER, and the shortcut is in the user's own Start menu.

pub mod sys;
pub mod ui;

use std::path::{Path, PathBuf};

use crate::update::version::Version;

pub const APP_NAME: &str = "Projects";
pub const EXE_NAME: &str = "Projects.exe";
/// A file in the install folder that marks it as ours: uninstall and reinstall touch a folder
/// only when it is empty or carries this.
const MARKER: &str = ".projects-install";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Projects";
const PUBLISHER: &str = "rajofearth";

/// What this process is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Running from its install folder: the app, with updates.
    Installed(PathBuf),
    /// Running from anywhere else on purpose (a dev build, a headless run): the app, with
    /// updates off, and why.
    Portable(String),
    /// Opened from a download: the install window.
    Setup,
}

/// The copy recorded in the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Existing {
    pub dir: PathBuf,
    pub version: Option<Version>,
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `%LOCALAPPDATA%\Programs\Projects`, where per-user apps install.
pub fn default_dir() -> PathBuf {
    env_dir("LOCALAPPDATA")
        .or_else(|| env_dir("USERPROFILE").map(|p| p.join("AppData").join("Local")))
        .unwrap_or_else(|| PathBuf::from(r"C:\"))
        .join("Programs")
        .join(APP_NAME)
}

/// `%LOCALAPPDATA%\Projects`: downloaded updates and the update records.
pub fn data_dir() -> Option<PathBuf> {
    env_dir("LOCALAPPDATA").map(|p| p.join(APP_NAME))
}

fn start_menu_link() -> Option<PathBuf> {
    env_dir("APPDATA").map(|p| p.join(r"Microsoft\Windows\Start Menu\Programs").join(format!("{APP_NAME}.lnk")))
}

pub fn existing() -> Option<Existing> {
    let dir = PathBuf::from(sys::reg_read(UNINSTALL_KEY, "InstallLocation")?);
    if !dir.join(EXE_NAME).is_file() {
        return None;
    }
    let version = sys::reg_read(UNINSTALL_KEY, "DisplayVersion").and_then(|v| Version::parse(&v));
    Some(Existing { dir, version })
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        p.to_string_lossy().trim_end_matches('\\').to_lowercase()
    };
    norm(a) == norm(b)
}

/// Decides the mode from how the process was started. `args` excludes the program name.
pub fn mode(args: &[String]) -> Mode {
    if std::env::var_os("PROJECTS_SHOW_INSTALLER").is_some() {
        return Mode::Setup;
    }
    if cfg!(debug_assertions) {
        return Mode::Portable("Updates are off in debug builds".into());
    }
    if std::env::var_os("PROJECTS_PORTABLE").is_some() {
        return Mode::Portable("Updates are off because this copy isn't installed".into());
    }
    let Ok(exe) = std::env::current_exe() else {
        return Mode::Portable("Updates are off: this copy can't find its own file".into());
    };
    if exe.components().any(|c| c.as_os_str().eq_ignore_ascii_case("target")) {
        return Mode::Portable("Updates are off for builds run from source".into());
    }
    if !args.is_empty() {
        // Rune's headless and scene runs (`--scene demo`, `--snap`) never install anything.
        return Mode::Portable("Updates are off for command-line runs".into());
    }
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let installed_here = existing().is_some_and(|e| same_path(&e.dir, &dir)) || dir.join(MARKER).is_file();
    if installed_here { Mode::Installed(exe) } else { Mode::Setup }
}

/// A folder we may install into: missing, empty, or one of ours.
fn usable(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Err(_) => !dir.exists(),
        Ok(mut entries) => entries.next().is_none() || dir.join(MARKER).is_file(),
    }
}

/// Folders only an administrator can write to.
fn needs_admin(dir: &Path) -> bool {
    ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "SystemRoot"]
        .iter()
        .filter_map(|v| env_dir(v))
        .any(|root| dir.to_string_lossy().to_lowercase().starts_with(&root.to_string_lossy().to_lowercase()))
}

/// The folder an install into `chosen` uses. A folder that holds other files gets a `Projects`
/// folder inside it, so installing (and later uninstalling) never mixes with someone's files.
pub fn resolve_target(chosen: &Path) -> Result<PathBuf, String> {
    if !chosen.is_absolute() || chosen.parent().is_none() {
        return Err("Choose a full folder path, like C:\\Users\\you\\Apps".into());
    }
    if needs_admin(chosen) {
        return Err("That folder needs admin rights. Choose one in your user folder.".into());
    }
    let target = if usable(chosen) { chosen.to_path_buf() } else { chosen.join(APP_NAME) };
    if !usable(&target) {
        return Err(format!("{} already has other files in it. Choose an empty folder.", target.display()));
    }
    Ok(target)
}

/// The steps the install window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Copy,
    Shortcut,
    Register,
}

impl Step {
    pub fn label(self) -> &'static str {
        match self {
            Step::Copy => "Copying Projects.exe",
            Step::Shortcut => "Adding the Start menu shortcut",
            Step::Register => "Adding Projects to Settings, Apps",
        }
    }
}

/// Puts `source` at `target`, even when `target` is running: Windows lets a running exe be
/// renamed, so the old one moves aside to `.old` and the new one takes its name. Fails
/// without changing anything when the copy can't be made; puts the old exe back if the final
/// rename fails. Returns where the old exe went.
pub fn place_exe(source: &Path, target: &Path) -> Result<Option<PathBuf>, String> {
    let new = target.with_extension("exe.new");
    std::fs::copy(source, &new).map_err(|e| denied(e, "copy Projects.exe"))?;
    let old = if target.exists() {
        let old = (0..10)
            .map(|i| target.with_extension(if i == 0 { "exe.old".to_string() } else { format!("exe.old{i}") }))
            .find(|p| !p.exists() || std::fs::remove_file(p).is_ok())
            .ok_or("too many old copies are still running")?;
        if let Err(e) = std::fs::rename(target, &old) {
            let _ = std::fs::remove_file(&new);
            return Err(denied(e, "replace the old Projects.exe"));
        }
        Some(old)
    } else {
        None
    };
    if let Err(e) = std::fs::rename(&new, target) {
        if let Some(old) = &old {
            let _ = std::fs::rename(old, target);
        }
        let _ = std::fs::remove_file(&new);
        return Err(denied(e, "move the new Projects.exe into place"));
    }
    Ok(old)
}

fn denied(e: std::io::Error, what: &str) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        format!("Couldn't {what}: Windows denied access. Choose a folder in your user folder.")
    } else {
        format!("Couldn't {what} ({e})")
    }
}

/// Deletes exes an update or reinstall set aside. Ones still running stay until next time.
pub fn remove_old_copies(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if name.starts_with("projects.exe.old") || name == "projects.exe.new" || name == "projects.exe.failed" {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Writes the uninstall entry Settings, Apps lists, for `exe` at `version`.
pub fn register(exe: &Path, version: Version) -> Result<(), String> {
    let dir = exe.parent().ok_or("the exe has no folder")?;
    let quoted = format!("\"{}\"", exe.display());
    let kb = std::fs::metadata(exe).map(|m| (m.len() / 1024) as u32).unwrap_or(0);
    let version = version.to_string();
    let (dir_s, icon, uninstall) = (dir.display().to_string(), format!("{},0", exe.display()), format!("{quoted} --uninstall"));
    let url = format!("https://github.com/{}", crate::update::feed::REPO);
    use sys::Value::{Number, Text};
    sys::reg_write(
        UNINSTALL_KEY,
        &[
            ("DisplayName", Text(APP_NAME)),
            ("DisplayVersion", Text(&version)),
            ("Publisher", Text(PUBLISHER)),
            ("InstallLocation", Text(&dir_s)),
            ("DisplayIcon", Text(&icon)),
            ("UninstallString", Text(&uninstall)),
            ("URLInfoAbout", Text(&url)),
            ("EstimatedSize", Number(kb)),
            ("NoModify", Number(1)),
            ("NoRepair", Number(1)),
        ],
    )
}

/// Installs this exe into `dir`, calling `step` as each part starts. Returns the installed exe.
pub fn install(dir: &Path, mut step: impl FnMut(Step)) -> Result<PathBuf, String> {
    let source = std::env::current_exe().map_err(|e| format!("Couldn't find this file ({e})"))?;
    step(Step::Copy);
    std::fs::create_dir_all(dir).map_err(|e| denied(e, "create the folder"))?;
    std::fs::write(dir.join(MARKER), format!("{APP_NAME} {}\n", env!("CARGO_PKG_VERSION"))).map_err(|e| denied(e, "write to the folder"))?;
    let exe = dir.join(EXE_NAME);
    if !same_path(&source, &exe) {
        place_exe(&source, &exe)?;
    }
    remove_old_copies(dir);
    step(Step::Shortcut);
    if let Some(lnk) = start_menu_link() {
        sys::shortcut(&exe, &lnk)?;
    }
    step(Step::Register);
    let previous = existing();
    register(&exe, Version::current())?;
    // An earlier install in another folder: its exe and marker go; anything else stays.
    if let Some(prev) = previous.filter(|p| !same_path(&p.dir, dir)) {
        if prev.dir.join(MARKER).is_file() {
            let _ = std::fs::remove_file(prev.dir.join(EXE_NAME));
            let _ = std::fs::remove_file(prev.dir.join(MARKER));
            remove_old_copies(&prev.dir);
            let _ = std::fs::remove_dir(&prev.dir);
        }
    }
    Ok(exe)
}

/// Starts `exe` as a separate process, in its own folder.
pub fn launch(exe: &Path) -> Result<u32, String> {
    let mut cmd = std::process::Command::new(exe);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    cmd.env_remove("PROJECTS_SHOW_INSTALLER");
    cmd.spawn().map(|c| c.id()).map_err(|e| format!("Couldn't start Projects ({e})"))
}

/// `--uninstall`: asks, then removes the shortcut, the uninstall entry and the downloaded
/// updates, and deletes the exe once this process has exited. Settings and templates in
/// `%APPDATA%\projects` stay, so a reinstall picks up where it left off.
pub fn uninstall() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent().map(Path::to_path_buf) else { return };
    if !sys::ask(
        "Uninstall Projects",
        "Remove Projects from this PC?\n\nYour settings and templates stay, so reinstalling picks up where you left off.",
    ) {
        return;
    }
    if let Some(lnk) = start_menu_link() {
        let _ = std::fs::remove_file(lnk);
    }
    sys::reg_delete(UNINSTALL_KEY);
    if let Some(data) = data_dir() {
        let _ = std::fs::remove_dir_all(data.join("updates"));
        let _ = std::fs::remove_dir(&data);
    }
    remove_old_copies(&dir);
    delete_after_exit(&exe, &dir);
}

/// A running exe can't delete itself, so a hidden `cmd` waits two seconds for this process to
/// exit, deletes the exe and the marker, and removes the folder only if that left it empty.
#[cfg(windows)]
fn delete_after_exit(exe: &Path, dir: &Path) {
    use std::os::windows::process::CommandExt;
    let (exe_s, marker_s, dir_s) = (exe.display().to_string(), dir.join(MARKER).display().to_string(), dir.display().to_string());
    // cmd expands % and treats ^ specially even inside quotes; such a path is left on disk.
    if [&exe_s, &marker_s, &dir_s].iter().any(|s| s.contains(['%', '^', '"', '!'])) {
        sys::tell("Uninstall Projects", &format!("Projects was removed. Delete {dir_s} to finish."), false);
        return;
    }
    let script = format!("ping -n 3 127.0.0.1 >nul & del /f /q \"{exe_s}\" \"{marker_s}\" & rd \"{dir_s}\"");
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("cmd.exe")
        .raw_arg("/d /c")
        .raw_arg(format!("\"{script}\""))
        .current_dir(std::env::temp_dir())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(not(windows))]
fn delete_after_exit(_: &Path, _: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("projects-install-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_busy_folder_gets_a_projects_folder_inside_it() {
        let root = temp("resolve");
        std::fs::write(root.join("notes.txt"), "mine").unwrap();
        assert_eq!(resolve_target(&root).unwrap(), root.join(APP_NAME));
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(resolve_target(&empty).unwrap(), empty);
        let ours = root.join("ours");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::write(ours.join("x"), "").unwrap();
        std::fs::write(ours.join(MARKER), "").unwrap();
        assert_eq!(resolve_target(&ours).unwrap(), ours);
        assert!(resolve_target(Path::new("relative\\dir")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_full_of_someone_elses_files_twice_over_is_refused() {
        let root = temp("refuse");
        std::fs::write(root.join("a"), "").unwrap();
        std::fs::create_dir_all(root.join(APP_NAME)).unwrap();
        std::fs::write(root.join(APP_NAME).join("b"), "").unwrap();
        assert!(resolve_target(&root).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn placing_an_exe_moves_the_old_one_aside() {
        let root = temp("place");
        let (src, target) = (root.join("new.bin"), root.join(EXE_NAME));
        std::fs::write(&src, "v2").unwrap();
        std::fs::write(&target, "v1").unwrap();
        let old = place_exe(&src, &target).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "v1");
        remove_old_copies(&root);
        assert!(!old.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn placing_into_an_empty_folder_needs_no_old_copy() {
        let root = temp("fresh");
        let src = root.join("new.bin");
        std::fs::write(&src, "v1").unwrap();
        assert_eq!(place_exe(&src, &root.join(EXE_NAME)).unwrap(), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Installs this test binary for real: the registry entry, a shortcut and the files, with
    /// APPDATA pointed at a temp folder, then removes it all. `cargo test -- --ignored install`.
    #[test]
    #[ignore]
    fn installs_registers_and_adds_a_shortcut() {
        let root = temp("real");
        let appdata = root.join("roaming");
        unsafe { std::env::set_var("APPDATA", &appdata) };
        let dir = resolve_target(&root.join("Programs")).unwrap();
        let mut steps = Vec::new();
        let exe = install(&dir, |s| steps.push(s)).unwrap();
        assert_eq!(steps, vec![Step::Copy, Step::Shortcut, Step::Register]);
        assert!(exe.is_file() && dir.join(MARKER).is_file());
        assert!(start_menu_link().unwrap().is_file(), "shortcut");
        let found = existing().expect("registry entry");
        assert!(same_path(&found.dir, &dir));
        assert_eq!(found.version, Some(Version::current()));
        assert!(sys::reg_read(UNINSTALL_KEY, "UninstallString").unwrap().ends_with("--uninstall"));
        sys::reg_delete(UNINSTALL_KEY);
        assert!(existing().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
