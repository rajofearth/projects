//! Self-updating from GitHub releases.
//!
//! An installed copy checks the latest release six seconds after it starts and every four
//! hours after that, on a background thread. A newer release downloads into
//! `%LOCALAPPDATA%\Projects\updates`, and is kept only if its signature (see `verify`) and
//! checksum match. The user then restarts from the sidebar or Settings; if they don't, the next
//! launch installs it before the window opens.
//!
//! Installing swaps the exe in place (`install::place_exe`), starts the new version, and leaves
//! the old exe watching it from its `.old` name. If the new version exits before it finishes
//! starting, the watcher puts the old exe back, starts it, and that version is never offered
//! again. Every state change goes through `machine`, which has no I/O.

pub mod feed;
pub mod machine;
pub mod verify;
pub mod version;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

use rune::Waker;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::install::{self, Mode};
use feed::Release;
use machine::{Next, Staged, UpdateState};
use verify::SigFile;
use version::Version;

const FIRST_CHECK_SECS: f64 = 6.0;
const CHECK_EVERY_SECS: f64 = 4.0 * 3600.0;
/// A failed check or download is tried again after two minutes, then ten, so a connection that
/// isn't up yet when Projects opens recovers on its own; after that the four-hour gap returns.
const RETRY_SECS: [f64; 2] = [120.0, 600.0];
/// How long the old exe waits for the new one to finish starting.
const WATCH_SECS: u64 = 90;

fn updates_dir() -> Option<PathBuf> {
    install::data_dir().map(|d| d.join("updates"))
}

// ---- files on disk -------------------------------------------------------------

/// `pending.json`: the verified download waiting for a restart.
#[derive(Serialize, Deserialize)]
struct Pending {
    version: String,
    sha256: String,
    sig: String,
    #[serde(default)]
    notes: Vec<String>,
}

/// `install.json`: written just before the new exe starts, removed by it once it is up.
#[derive(Serialize, Deserialize)]
struct Record {
    from: String,
    to: String,
}

/// `failed.json`: a version that didn't start.
#[derive(Serialize, Deserialize)]
struct Failed {
    version: String,
    from: String,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Writes through a temporary file and a rename, so a crash never leaves half a file.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| format!("couldn't save {} ({e})", path.display()))
}

fn staged_exe(dir: &Path, version: Version) -> PathBuf {
    dir.join(version.to_string()).join(install::EXE_NAME)
}

fn load_staged(dir: &Path) -> Option<Staged> {
    let p: Pending = read_json(&dir.join("pending.json"))?;
    let version = Version::parse(&p.version)?;
    let exe = staged_exe(dir, version);
    exe.is_file().then_some(Staged { version, exe, sha256: p.sha256, sig: p.sig, notes: p.notes })
}

fn load_bad(dir: &Path) -> Option<Version> {
    read_json::<Failed>(&dir.join("failed.json")).and_then(|f| Version::parse(&f.version))
}

/// Removes every downloaded version and the pending record.
fn clear_staged(dir: &Path) {
    let _ = std::fs::remove_file(dir.join("pending.json"));
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("couldn't read the update ({e})"))?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).map_err(|e| format!("couldn't read the update ({e})"))?;
    Ok(verify::to_hex(&h.finalize()))
}

/// Checks a staged exe again right before it runs: its signature, its version and its bytes.
fn verify_staged(s: &Staged) -> Result<(), String> {
    let sig = SigFile::parse(&s.sig)?;
    if Version::parse(&sig.version) != Some(s.version) || sig.sha256 != s.sha256 {
        return Err("the saved update doesn't match its signature".into());
    }
    sig.verify(&verify::PUBLIC_KEY)?;
    if sha256_file(&s.exe)? != s.sha256 {
        return Err("the saved update changed on disk".into());
    }
    Ok(())
}

// ---- network work (background threads) ------------------------------------------

fn check() -> Result<Option<Release>, String> {
    let json = crate::net::get_text(&feed::latest_url(), "application/vnd.github+json", 1024 * 1024)?;
    feed::parse(&json).map(Some)
}

/// Downloads `rel`, checking the signature before the exe and the exe against the signature.
fn download(rel: &Release, dir: &Path, progress: impl Fn(u64, u64)) -> Result<Staged, String> {
    let sig_text = crate::net::get_text(&rel.sig_url, "application/octet-stream", 4096)?;
    let sig = SigFile::parse(&sig_text)?;
    if Version::parse(&sig.version) != Some(rel.version) {
        return Err(format!("the signature is for {}, not {}", sig.version, rel.version));
    }
    sig.verify(&verify::PUBLIC_KEY)?;

    let exe = staged_exe(dir, rel.version);
    let folder = exe.parent().ok_or("no update folder")?;
    std::fs::create_dir_all(folder).map_err(|e| format!("couldn't create {} ({e})", folder.display()))?;
    let part = exe.with_extension("exe.part");
    let mut file = std::fs::File::create(&part).map_err(|e| format!("couldn't save the update ({e})"))?;
    let mut hash = Sha256::new();
    let mut done: u64 = 0;
    let mut reported: u64 = 0;
    let fetched = crate::net::get(&rel.exe_url, "application/octet-stream", rel.exe_size, |chunk, _| {
        file.write_all(chunk).map_err(|e| format!("couldn't save the update ({e})"))?;
        hash.update(chunk);
        done += chunk.len() as u64;
        if done - reported >= 256 * 1024 || done == rel.exe_size {
            reported = done;
            progress(done, rel.exe_size);
        }
        Ok(())
    });
    let finished = fetched.and_then(|_| file.sync_all().map_err(|e| format!("couldn't save the update ({e})")));
    drop(file);
    let checked = finished.and_then(|_| {
        if done != rel.exe_size {
            return Err("the download ended early".to_string());
        }
        if verify::to_hex(&hash.finalize()) != sig.sha256 {
            return Err("the download doesn't match its signature".to_string());
        }
        Ok(())
    });
    if let Err(e) = checked {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &exe).map_err(|e| format!("couldn't save the update ({e})"))?;
    // Older downloads go; only this one stays pending.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().is_dir() && e.path() != folder {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let staged = Staged { version: rel.version, exe, sha256: sig.sha256.clone(), sig: sig_text, notes: rel.notes.clone() };
    write_json(
        &dir.join("pending.json"),
        &Pending { version: staged.version.to_string(), sha256: staged.sha256.clone(), sig: staged.sig.clone(), notes: staged.notes.clone() },
    )?;
    Ok(staged)
}

// ---- installing ----------------------------------------------------------------------

/// Swaps `staged` in at `target`, starts it, and leaves the old exe watching it.
fn apply(staged: &Staged, target: &Path, dir: &Path) -> Result<(), String> {
    verify_staged(staged)?;
    // Another installer may have put a newer version in place while this one ran.
    if install::existing().and_then(|e| e.version).is_some_and(|v| v >= staged.version) {
        return Err(format!("a version at least as new as {} is already installed", staged.version));
    }
    let from = Version::current();
    write_json(&dir.join("install.json"), &Record { from: from.to_string(), to: staged.version.to_string() })?;
    // The copy beside the target is checked again, so the bytes verified are the bytes that run.
    let check = |copy: &Path| {
        if sha256_file(copy)? == staged.sha256 { Ok(()) } else { Err("the update changed while it was being installed".to_string()) }
    };
    let old = match install::place_exe_checked(&staged.exe, target, check) {
        Ok(Some(old)) => old,
        Ok(None) => {
            let _ = std::fs::remove_file(dir.join("install.json"));
            return Err("the installed Projects.exe was missing".into());
        }
        Err(e) => {
            let _ = std::fs::remove_file(dir.join("install.json"));
            return Err(e);
        }
    };
    let pid = match install::launch(target) {
        Ok(pid) => pid,
        Err(e) => {
            // Put the old exe back so the next start still works.
            let _ = std::fs::rename(target, target.with_extension("exe.failed"));
            let _ = std::fs::rename(&old, target);
            let _ = std::fs::remove_file(dir.join("install.json"));
            return Err(e);
        }
    };
    let mut watcher = std::process::Command::new(&old);
    watcher.arg("--update-watch").arg(pid.to_string()).arg(target).arg(staged.version.to_string());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        watcher.creation_flags(CREATE_NO_WINDOW);
    }
    // Without a watcher the update still stands; it just can't roll back.
    let _ = watcher.spawn();
    Ok(())
}

/// Called from `main` before any window: installs a download the user didn't restart for.
/// True when the new version has been started and this process should exit.
pub fn apply_pending_at_launch(exe: &Path) -> bool {
    let Some(dir) = updates_dir() else { return false };
    let Some(staged) = load_staged(&dir) else { return false };
    if staged.version <= Version::current() || Some(staged.version) == load_bad(&dir) {
        return false;
    }
    match apply(&staged, exe, &dir) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("update {} not installed: {e}", staged.version);
            clear_staged(&dir);
            false
        }
    }
}

/// `--update-watch <pid> <target> <version>`: runs from the old exe after an update. Waits for
/// the new process to finish starting (it removes `install.json`). If it exits first, restores
/// the old exe, records the version as failed, and starts the old exe again.
pub fn watch(args: &[String]) {
    let (Some(pid), Some(target), Some(version)) =
        (args.first().and_then(|p| p.parse::<u32>().ok()), args.get(1).map(PathBuf::from), args.get(2))
    else {
        return;
    };
    let Some(dir) = updates_dir() else { return };
    let record = dir.join("install.json");
    // Act only on the update this exe just handed over: the target is the exe beside it, and
    // the version is the one the record names.
    let Ok(me) = std::env::current_exe() else { return };
    let beside = me.parent().map(|d| d.join(install::EXE_NAME));
    let expected = read_json::<Record>(&record).map(|r| r.to);
    if beside.as_deref() != Some(target.as_path()) || expected.as_deref() != Some(version.as_str()) {
        return;
    }
    // One handle for the whole wait, so a reused process id can't stand in for the new exe.
    // The new exe can exit before this process starts. If it never removed the record, it failed.
    let child = install::sys::Process::open(pid);
    let start = std::time::Instant::now();
    let failed = loop {
        let Some(child) = &child else { break record.exists() };
        if !record.exists() {
            break false;
        }
        if child.wait(250) {
            break record.exists();
        }
        if start.elapsed().as_secs() >= WATCH_SECS {
            // Still running but slow to start: leave it be.
            break false;
        }
    };
    if !failed {
        return;
    }
    let _ = std::fs::remove_file(target.with_extension("exe.failed"));
    if std::fs::rename(&target, target.with_extension("exe.failed")).is_ok() && std::fs::rename(&me, &target).is_ok() {
        let from = read_json::<Record>(&record).map(|r| r.from).unwrap_or_default();
        let _ = write_json(&dir.join("failed.json"), &Failed { version: version.clone(), from });
        let _ = std::fs::remove_file(&record);
        clear_staged(&dir);
        let _ = install::launch(&target);
    }
}

/// What the app learned about the last update when it started.
pub enum Outcome {
    Updated(Version),
    RolledBack(Version),
}

/// Called once the window is up: tells the watcher this version started, cleans up after the
/// update, and keeps the uninstall entry's version current.
fn confirm_started(exe: &Path, dir: &Path) -> Option<Outcome> {
    let current = Version::current();
    let record = dir.join("install.json");
    let mut outcome = None;
    if let Some(r) = read_json::<Record>(&record) {
        if Version::parse(&r.to) == Some(current) {
            let _ = std::fs::remove_file(&record);
            clear_staged(dir);
            let _ = std::fs::remove_file(dir.join("failed.json"));
            outcome = Some(Outcome::Updated(current));
        } else {
            // Left by an install that never started: nothing waits on it.
            let _ = std::fs::remove_file(&record);
        }
    }
    if outcome.is_none()
        && let Some(f) = read_json::<Failed>(&dir.join("failed.json")).filter(|f| Version::parse(&f.from) == Some(current)) {
        if let Some(v) = Version::parse(&f.version) {
            outcome = Some(Outcome::RolledBack(v));
        }
        // Said once; the version stays skipped through `bad`.
        let _ = write_json(&dir.join("failed.json"), &Failed { version: f.version, from: String::new() });
    }
    if let Some(folder) = exe.parent() {
        install::remove_old_copies(folder);
    }
    if install::existing().is_some_and(|e| e.version != Some(current)) {
        let _ = install::register(exe, current);
    }
    outcome
}

// ---- the service the UI owns ----------------------------------------------------------

enum Msg {
    Checked(Result<Option<Release>, String>),
    Progress(u64, u64),
    Downloaded(Result<Staged, String>),
}

pub struct Updater {
    pub state: UpdateState,
    exe: Option<PathBuf>,
    dir: Option<PathBuf>,
    rx: Option<Receiver<Msg>>,
    next_check: f64,
    /// Checks or downloads that failed since the last good one; picks the retry gap.
    failures: u8,
    cleaned: bool,
}

/// What `drain` found worth telling the user.
#[derive(Default)]
pub struct Drained {
    pub changed: bool,
    pub ready: Option<Version>,
}

impl Updater {
    pub fn new(mode: &Mode) -> Updater {
        let mut state = UpdateState::new(Version::current());
        let (exe, dir) = match mode {
            Mode::Installed(exe) => (Some(exe.clone()), updates_dir()),
            Mode::Portable(why) => {
                state.disabled = Some(why.clone());
                (None, None)
            }
            Mode::Setup => {
                state.disabled = Some("Updates start once Projects is installed".into());
                (None, None)
            }
        };
        if exe.is_some() && dir.is_none() {
            state.disabled = Some("Updates are off: LOCALAPPDATA isn't set".into());
        }
        if let Some(d) = &dir {
            state.bad = load_bad(d);
            if let Some(s) = load_staged(d)
                && !state.restore(s)
            {
                clear_staged(d);
            }
        }
        Updater { state, exe, dir, rx: None, next_check: FIRST_CHECK_SECS, failures: 0, cleaned: false }
    }

    /// The fixture's updater: off, with a reason that reads naturally in screenshots.
    pub fn demo() -> Updater {
        Updater::new(&Mode::Portable("Updates are off in the demo".into()))
    }

    /// Runs once the window is up.
    pub fn started(&mut self) -> Option<Outcome> {
        let (exe, dir) = (self.exe.as_ref()?, self.dir.as_ref()?);
        std::fs::create_dir_all(dir).ok()?;
        let outcome = confirm_started(exe, dir);
        if let Some(Outcome::Updated(_)) = outcome {
            self.state.ready = None;
        }
        outcome
    }

    /// Starts a scheduled check when one is due; true when the state changed.
    pub fn tick(&mut self, clock: f64, auto: bool, waker: Option<&Waker>) -> bool {
        if !self.cleaned && clock > 120.0 {
            // The watcher has exited by now, so the old exe it ran from can go.
            self.cleaned = true;
            if let Some(folder) = self.exe.as_ref().and_then(|e| e.parent()) {
                install::remove_old_copies(folder);
            }
        }
        if !auto || clock < self.next_check || !self.state.can_check() {
            return false;
        }
        self.check_now(waker)
    }

    /// Checks right away (the Check button); true when a check started.
    pub fn check_now(&mut self, waker: Option<&Waker>) -> bool {
        if !self.state.check_started() {
            return false;
        }
        let tx = self.channel();
        let waker = waker.cloned();
        std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                let _ = tx.send(Msg::Checked(check()));
                if let Some(w) = waker {
                    w();
                }
            })
            .is_ok()
    }

    fn channel(&mut self) -> Sender<Msg> {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        tx
    }

    fn start_download(&mut self, rel: Release, waker: Option<&Waker>) {
        let Some(dir) = self.dir.clone() else { return };
        let tx = self.channel();
        let waker = waker.cloned();
        let _ = std::thread::Builder::new().name("update-download".into()).spawn(move || {
            let wake = || {
                if let Some(w) = &waker {
                    w();
                }
            };
            let progress_tx = tx.clone();
            let result = download(&rel, &dir, |done, total| {
                let _ = progress_tx.send(Msg::Progress(done, total));
                wake();
            });
            let _ = tx.send(Msg::Downloaded(result));
            wake();
        });
    }

    /// Sets when the next automatic check is due: the regular gap after a good result, a short
    /// one after a failure, so a dropped connection doesn't wait four hours for a retry.
    fn schedule_after(&mut self, clock: f64, ok: bool) {
        if ok {
            self.failures = 0;
            self.next_check = clock + CHECK_EVERY_SECS;
            return;
        }
        let gap = RETRY_SECS.get(self.failures as usize).copied().unwrap_or(CHECK_EVERY_SECS);
        self.failures = self.failures.saturating_add(1);
        self.next_check = clock + gap;
    }

    pub fn drain(&mut self, clock: f64, waker: Option<&Waker>) -> Drained {
        let mut out = Drained::default();
        let now = crate::saved::now_unix();
        while let Some(rx) = &self.rx {
            let msg = match rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.rx = None;
                    if self.state.busy() {
                        self.state.check_finished(Err("the update thread stopped".into()), now);
                        self.schedule_after(clock, false);
                        out.changed = true;
                    }
                    break;
                }
            };
            out.changed = true;
            match msg {
                Msg::Checked(r) => {
                    self.rx = None;
                    self.schedule_after(clock, r.is_ok());
                    if let Next::Download(rel) = self.state.check_finished(r, now) {
                        self.start_download(rel, waker);
                    }
                }
                Msg::Progress(done, total) => self.state.progress(done, total),
                Msg::Downloaded(r) => {
                    self.rx = None;
                    self.schedule_after(clock, r.is_ok());
                    if let Next::Announce(v) = self.state.download_finished(r) {
                        out.ready = Some(v);
                    }
                }
            }
        }
        out
    }

    /// Installs the downloaded update and starts it. On Ok the caller quits.
    pub fn install_now(&mut self) -> Result<(), String> {
        let (Some(exe), Some(dir)) = (self.exe.clone(), self.dir.clone()) else {
            return Err("this copy isn't installed".into());
        };
        let staged = self.state.ready.clone().ok_or("no update is downloaded")?;
        apply(&staged, &exe, &dir).inspect_err(|_| {
            clear_staged(&dir);
            self.state.ready = None;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_retry_soon_then_return_to_the_regular_gap() {
        let mut u = Updater::new(&Mode::Portable("test".into()));
        u.schedule_after(100.0, false);
        assert_eq!(u.next_check, 100.0 + RETRY_SECS[0]);
        u.schedule_after(200.0, false);
        assert_eq!(u.next_check, 200.0 + RETRY_SECS[1]);
        u.schedule_after(900.0, false);
        assert_eq!(u.next_check, 900.0 + CHECK_EVERY_SECS, "the short gaps are spent");
        u.schedule_after(1000.0, true);
        assert_eq!(u.next_check, 1000.0 + CHECK_EVERY_SECS);
        u.schedule_after(1000.0, false);
        assert_eq!(u.next_check, 1000.0 + RETRY_SECS[0], "a good check resets the backoff");
    }
}
