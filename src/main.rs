//! Projects — a manager for a folder of code (projects, storage, usage), built with Rune.
//! `cargo run -p projects-stats` opens the window and starts scanning;
//! `--scene demo` opens the window on the built-in fixture instead of scanning.

// Release builds are GUI programs, so opening Projects shows no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod art;
mod browse;
mod cats;
mod detail;
mod detect;
mod fixture;
mod fmt;
mod home;
mod icons;
mod inspect;
mod install;
mod launch;
mod meter;
mod model;
mod net;
mod newproj;
mod saved;
mod scan;
mod settings_ui;
mod shell;
mod storage_ui;
mod store;
mod templates;
mod templates_ui;
mod ui;
mod update;
mod update_ui;
mod usage;
mod usage_ui;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, OnceLock};

use model::ScanEvent;
use rune::{AppSpec, Waker};
use scan::GitCache;

pub struct State {
    pub root: String,
    pub projects: Vec<model::Project>,
    pub scanning: bool,
    pub done_count: usize,
    pub total: usize,
    pub current: String,
    /// Clock seconds when the last scan finished (None: never).
    pub last_done: Option<f64>,
    /// Wall-clock label of the last finished scan, e.g. "13 Nov 14:32".
    pub done_at: Option<String>,
    pub scan_secs: f64,
    pub error: Option<String>,
    pub rx: Option<Receiver<ScanEvent>>,
    pub waker: Option<Waker>,
    /// The `demo` scene: fixture data, no live scanning.
    pub demo: bool,
    /// Names already delivered by the scan in flight, for upserting rows in place.
    refreshing: std::collections::HashSet<String>,
    /// What the window is showing: the category, the row, the folders opened inside it.
    pub ui: store::Ui,
    /// The dot-matrix meter: its buffer, and the surface it is drawn through.
    pub meter: meter::Meter,
    /// The window's pixel scale, as `Cx::env` reported it, for the meter's buffer.
    pub scale: f32,
    /// Clock seconds of the meter's last frame.
    meter_clock: f64,
    /// Frames per second over the last 2 s window.
    pub fps: f32,
    /// This process's working set, sampled once a second.
    pub ram_bytes: u64,
    fps_frames: u32,
    fps_since: f64,
    last_trim: f64,
    /// Git facts carried between scans, keyed by project path with a refs-mtime validity
    /// key — rescans skip the history walk for repos that haven't moved.
    pub git_cache: GitCache,
    /// Which page the window shows.
    pub page: shell::Page,
    pub home: home::Home,
    pub usage: usage_ui::UsageState,
    /// Editors and tools found on this machine, probed once.
    pub apps: Vec<launch::App>,
    /// Editor icons and project logos, decoded off the UI thread.
    pub art: art::Art,
    /// Pins, archive, recently opened, layout, folders, editor: what survives a restart.
    pub saved: saved::Saved,
    /// `detect` facts per project, read off the UI thread.
    pub inspect: inspect::Inspector,
    pub newp: newproj::NewProject,
    pub tmpl: templates_ui::TemplatesPage,
    pub settings: settings_ui::SettingsPage,
    /// Toasts on screen, and the next toast's id.
    pub notes: Vec<shell::Note>,
    note_seq: u64,
    /// The keyboard shortcut sheet.
    pub shortcuts: bool,
    /// The window's width, for menus opened from a button.
    pub window_w: f32,
    /// Checking for, downloading and installing new versions.
    pub updates: update::Updater,
}

/// What `main` decided this process is, for `State::new`.
static MODE: OnceLock<install::Mode> = OnceLock::new();

const RESCAN_EVERY: f64 = 300.0; // seconds

impl State {
    fn new() -> State {
        let saved = saved::Saved::load(r"P:\Projects");
        let root = saved.root.clone();
        let ui = store::Ui::new(PathBuf::from(&root));
        State {
            root,
            projects: Vec::new(),
            scanning: false,
            done_count: 0,
            total: 0,
            current: String::new(),
            last_done: None,
            done_at: None,
            scan_secs: 0.0,
            error: None,
            rx: None,
            waker: None,
            refreshing: std::collections::HashSet::new(),
            ui,
            meter: meter::Meter::new(),
            scale: 1.0,
            meter_clock: 0.0,
            demo: false,
            fps: 0.0,
            ram_bytes: 0,
            fps_frames: 0,
            fps_since: 0.0,
            last_trim: 0.0,
            git_cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
            page: shell::Page::Home,
            home: home::Home::new(),
            usage: usage_ui::UsageState::new(),
            apps: Vec::new(),
            art: art::Art::default(),
            saved,
            inspect: inspect::Inspector::new(),
            newp: newproj::NewProject::new(),
            tmpl: templates_ui::TemplatesPage::new(),
            settings: settings_ui::SettingsPage::new(),
            notes: Vec::new(),
            note_seq: 0,
            shortcuts: false,
            window_w: 1200.0,
            updates: update::Updater::new(
                MODE.get().unwrap_or(&install::Mode::Portable("Updates are off for this run".into())),
            ),
        }
    }

    pub fn toast(&mut self, text: String) {
        self.note_seq += 1;
        self.notes.push(shell::Note { id: self.note_seq, text, error: false, until: None });
    }

    pub fn toast_error(&mut self, text: String) {
        self.note_seq += 1;
        self.notes.push(shell::Note { id: self.note_seq, text, error: true, until: None });
    }

    /// Shows a page, closing whatever floats over the one being left.
    pub fn go(&mut self, page: shell::Page) {
        self.page = page;
        self.home.menu = None;
        self.home.open_select = None;
        self.shortcuts = false;
    }

    /// A rescan the user asked for (the button, Ctrl+R).
    pub fn rescan(&mut self) {
        if self.demo {
            self.toast("The demo scene has no folder to scan".into());
            return;
        }
        self.inspect.clear();
        self.request_scan();
    }

    /// Shows another projects folder: a new scan, and nothing carried over from the old one.
    pub fn switch_root(&mut self, root: &str) {
        if root.eq_ignore_ascii_case(&self.root) {
            return;
        }
        self.saved.set_root(root);
        self.root = root.to_string();
        self.ui = store::Ui::new(PathBuf::from(root));
        self.projects.clear();
        self.home.selected = None;
        self.home.detail = false;
        self.home.branches.clear();
        self.inspect.clear();
        self.scanning = false;
        self.rx = None;
        self.last_done = None;
        if !self.demo {
            self.request_scan();
        }
        self.toast(format!("Showing {root}"));
    }

    /// The footer's engine line: frame time in µs, resident memory, last scan duration.
    /// Parts unknown are left out, so a fresh window or the demo scene shows no
    /// placeholder dashes.
    pub fn engine_line(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.fps > 0.0 {
            parts.push(format!("{:.0} fps", self.fps));
        }
        if self.ram_bytes > 0 {
            parts.push(format!("{:.0} MB", self.ram_bytes as f64 / 1048576.0));
        }
        if self.scan_secs > 0.0 {
            parts.push(format!("scan {:.1}s", self.scan_secs));
        }
        parts.join(" · ")
    }

    fn load_demo(&mut self) {
        self.demo = true;
        self.updates = update::Updater::demo();
        self.inspect.demo = true;
        self.usage.demo = true;
        self.saved.opened.clear();
        self.saved.pinned.clear();
        self.saved.archived.clear();
        self.scanning = false;
        self.error = None;
        self.projects = fixture::demo_projects();
        self.scan_secs = 0.0;
        self.last_done = Some(0.0);
        for (i, p) in self.projects.iter().enumerate() {
            let branch = match i % 5 {
                0 => "main",
                1 => "dev",
                2 => "main",
                3 => "feat/sync",
                _ => "master",
            };
            self.home.branches.insert(p.name.clone(), if p.git { branch.to_string() } else { String::new() });
        }
    }

    /// Start a scan unless one is already running or demo mode holds the data.
    fn request_scan(&mut self) {
        if self.scanning || self.demo {
            return;
        }
        self.scanning = true;
        self.error = None;
        self.ui.reset();
        self.home.branches.clear();
        // The last scan's data stays on screen until each folder's new numbers land,
        // so a rescan never blanks the dashboard.
        self.refreshing.clear();
        self.done_count = 0;
        self.total = 0;
        self.current.clear();
        let (tx, rx) = channel();
        self.rx = Some(rx);
        // Tests and headless fixture runs have no loop to wake; a no-op waker is fine there.
        let waker: Waker = self.waker.clone().unwrap_or_else(|| Arc::new(|| ()));
        let root = PathBuf::from(&self.root);
        let cache = self.git_cache.clone();
        std::thread::spawn(move || scan::scan(root, tx, cache, move || waker()));
    }
}

impl State {
    /// Drain the scanner's channel into state; true when anything arrived. Each folder's
    /// row upserts in place the moment its numbers land, so the dashboard fills in live.
    /// A scanner that dies without a Done or Failed (a panic) ends the scanning state too,
    /// so the UI can never sit waiting for a scan that will never finish.
    pub fn drain(&mut self, clock: f64) -> bool {
        let mut changed = false;
        let mut dead = false;
        let mut finished = false;
        while let Some(rx) = self.rx.as_ref() {
            match rx.try_recv() {
                Ok(ScanEvent::Start { total }) => {
                    self.total = total;
                    changed = true;
                }
                Ok(ScanEvent::Project(p)) => {
                    self.current = p.name.clone();
                    self.refreshing.insert(p.name.clone());
                    match self.projects.iter_mut().find(|old| old.name == p.name) {
                        // A row arriving again is the same folder with its git history
                        // patched in: the row updates, no new folder is counted.
                        Some(old) => *old = *p,
                        None => {
                            self.projects.push(*p);
                            self.done_count += 1;
                        }
                    }
                    changed = true;
                }
                Ok(ScanEvent::Done { seconds }) => {
                    eprintln!("scan done in {seconds:.1}s");
                    self.scanning = false;
                    // Folders removed or renamed since the last scan never sent a row, so
                    // they would sit on screen for ever. The walk saw every folder that is
                    // still there; anything it did not report is gone.
                    let seen = std::mem::take(&mut self.refreshing);
                    let stale: Vec<String> = self
                        .projects
                        .iter()
                        .filter(|p| !seen.contains(&p.name))
                        .map(|p| p.name.clone())
                        .collect();
                    if !stale.is_empty() {
                        self.projects.retain(|p| seen.contains(&p.name));
                        self.done_count = self.projects.len();
                        self.ui.row = self.ui.row.min(self.projects.len().saturating_sub(1));
                    }
                    self.scan_secs = seconds;
                    self.last_done = Some(clock);
                    self.done_at = Some(wallclock());
                    // The scan's walkers and buffers just peaked the working set; hand
                    // those pages back so the app idles small.
                    trim_ram();
                    self.last_trim = clock;
                    finished = true;
                    changed = true;
                    break;
                }
                Ok(ScanEvent::Failed(e)) => {
                    self.scanning = false;
                    self.error = Some(e);
                    finished = true;
                    changed = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    dead = true;
                    break;
                }
            }
        }
        if finished {
            self.rx = None;
        }
        if dead {
            self.scanning = false;
            self.refreshing.clear();
            self.rx = None;
            if self.error.is_none() && !self.demo {
                self.error = Some("the scan stopped unexpectedly".to_string());
            }
            changed = true;
        }
        changed
    }

    // ---- pointer actions -----------------------------------------------------

    /// Narrows the list to a category, or to everything, back at its top level.
    pub fn pick(&mut self, cat: cats::Cat) {
        self.ui.select(store::Filter::Only(cat));
    }

    /// Shows every project again.
    pub fn filter_to(&mut self, filter: store::Filter) {
        self.ui.select(filter);
    }

    /// Opens the highlighted folder and measures what is inside.
    pub fn open(&mut self) {
        self.ui.open(&self.projects, self.demo);
    }

    /// Goes up one folder.
    pub fn back(&mut self) {
        self.ui.back(&self.projects, self.demo);
    }

    /// Jumps back to a level in the breadcrumb: 0 is the category itself.
    pub fn jump(&mut self, level: usize) {
        self.ui.jump(level, &self.projects, self.demo);
    }

    /// Opens every folder between the row and where its space actually is.
    pub fn go_to_culprit(&mut self) {
        self.ui.go_to_culprit(&self.projects, self.demo);
    }

    /// Shows the highlighted item in Explorer.
    pub fn reveal(&mut self) {
        self.ui.reveal(&self.projects, self.demo);
    }

    /// Shows the biggest folder a command brings back in Explorer: the first thing to clear.
    pub fn reveal_biggest(&mut self) {
        let Some((item, _)) = storage_ui::disk_win(self) else { return };
        if self.demo {
            self.toast(format!("Revealed {}", item.path.display()));
            return;
        }
        match browse::reveal(&item.path) {
            Ok(()) => self.toast(format!("Revealed {}", item.name)),
            Err(e) => self.toast_error(format!("Couldn't reveal {}: {e}", item.name)),
        }
    }

    // ---- the meter -----------------------------------------------------------

    /// The meter's element was laid out at `w` × `h` points.
    pub fn meter_resize(&mut self, w: f32, h: f32) {
        let scale = self.scale;
        self.meter.resize(w, h, ui::CELL, scale);
    }

    /// The segment under a point in the meter's own points.
    pub fn meter_hit(&self, x: f32, y: f32) -> Option<cats::Cat> {
        self.meter.hit(x, y)
    }

    /// The meter's shares, from the scan so far.
    fn meter_model(&self) -> meter::BarModel {
        let total = self.ui.total(&self.projects).max(1);
        let sizes = self.ui.category_sizes(&self.projects);
        meter::BarModel {
            segments: cats::CATEGORIES
                .iter()
                .zip(sizes)
                .map(|(c, bytes)| meter::Segment {
                    id: c.id,
                    color: c.color,
                    fraction: (bytes as f64 / total as f64).min(1.0) as f32,
                })
                .collect(),
            // Nothing is "the" segment under "All"; a filter lights only its own band.
            hover: self.ui.hover.or(match self.ui.stack.is_empty() {
                true => self.ui.filter.cat(),
                false => None,
            }),
            scanning: self.scanning,
        }
    }

    /// Advances the meter, presenting a frame. True while anything still moves.
    pub fn tick_meter(&mut self, clock: f64) -> bool {
        let dt = if self.meter_clock == 0.0 {
            1.0 / 60.0
        } else {
            (clock - self.meter_clock) as f32
        };
        self.meter_clock = clock;
        let model = self.meter_model();
        self.meter.step(dt.max(0.0), &model)
    }
}

fn poll(s: &mut State, clock: f64, tx: &mut rune::Tx) -> bool {
    let mut changed = s.drain(clock);
    // Folders that came back from the workers, and the culprit of the row in view.
    if s.ui.drain() {
        changed = true;
    }
    s.ui.want_culprit(s.demo);
    if s.home.branches.len() < s.projects.len() && !s.scanning && !s.demo {
        let root = PathBuf::from(&s.root);
        s.home.refresh_branches(&root, &s.projects);
        changed = true;
    }
    if newproj::drain(s) {
        changed = true;
    }
    if s.inspect.drain() {
        changed = true;
    }
    // Stack labels from the manifests, a few folders a frame, then every project's full
    // facts on the inspector's thread once the scan has settled.
    if !s.demo {
        let root = PathBuf::from(&s.root);
        let missing: Vec<String> = s
            .projects
            .iter()
            .filter(|p| !s.inspect.has_stack(&p.name))
            .take(24)
            .map(|p| p.name.clone())
            .collect();
        for name in missing {
            let st = detect::stack(&root.join(&name));
            s.inspect.set_stack(&name, st);
            changed = true;
        }
    }
    if !s.scanning {
        let waker = s.waker.clone();
        let root = PathBuf::from(&s.root);
        let names: Vec<String> = s.projects.iter().map(|p| p.name.clone()).collect();
        for name in names {
            if s.inspect.get(&name).is_none() && !s.inspect.pending(&name) {
                let path = root.join(&name);
                s.inspect.want(&name, path, false, waker.as_ref());
                changed |= s.demo;
            }
        }
    }
    if let (Some(name), true) = (s.home.selected.clone(), s.home.detail) {
        let path = PathBuf::from(&s.root).join(&name);
        let waker = s.waker.clone();
        s.inspect.want(&name, path, true, waker.as_ref());
    }
    if let Some(e) = s.saved.error.take() {
        s.toast_error(e);
        changed = true;
    }
    if shell::tick_toasts(s, clock, tx) {
        changed = true;
    }
    if s.page == shell::Page::Usage && !s.usage.loaded && !s.usage.loading {
        let waker = s.waker.clone();
        s.usage.start(waker);
        changed = true;
    }
    if s.usage.drain() {
        changed = true;
    }
    if !s.demo {
        let waker = s.waker.clone();
        if s.updates.tick(clock, s.saved.auto_update, waker.as_ref()) {
            changed = true;
        }
        let drained = s.updates.drain(waker.as_ref());
        if let Some(v) = drained.ready {
            s.toast(format!("Version {v} downloaded. Restart Projects to install it"));
        }
        changed |= drained.changed;
    }
    // The meter keeps drawing while shares ease, the scan runs, or the pointer moves over it.
    if s.tick_meter(clock) {
        changed = true;
    }
    if changed {
        // Data a scan brought is a change no input asked for, which would land at once;
        // ask for the animated kind so rows and figures ease into place.
        tx.animated();
    }

    // Engine gauges: frame rate over a 2 s window (a 1 s window flickers between 59 and
    // 61) and resident memory sampled with it. The footer only commits when a displayed
    // value actually changes, so a steady frame rate costs nothing.
    s.fps_frames += 1;
    let elapsed = clock - s.fps_since;
    if elapsed >= 2.0 {
        let fps = (s.fps_frames as f32 / elapsed as f32).round();
        let frames = s.fps_frames;
        s.fps_frames = 0;
        s.fps_since = clock;
        // Commit whenever the window produced frames. A window that produced none (the
        // runtime parks a window sitting still) keeps the last reading rather than reporting
        // 0 fps, and a window a scan straddled still counts so the gauge recovers afterwards.
        if frames >= 2 {
            let ram = process_ram() / 1048576;
            let fps_shown = s.fps as u32;
            let ram_shown = s.ram_bytes / 1048576;
            if fps != fps_shown as f32 || ram != ram_shown {
                s.fps = fps;
                s.ram_bytes = ram * 1048576;
                changed = true;
            }
        }
    }
    // At rest, hand pages back to Windows every 30 s: the working set creeps up as the
    // GPU driver and allocator touch pages, and a dashboard that is sitting still has no
    // use for them.
    if !s.scanning && clock - s.last_trim >= 30.0 {
        trim_ram();
        s.last_trim = clock;
    }

    let due = match s.last_done {
        None => true,
        Some(t) => s.saved.auto_rescan && clock - t >= RESCAN_EVERY,
    };
    if !s.scanning && !s.demo && due {
        s.request_scan();
        changed = true;
    }
    changed
}

#[cfg(windows)]
fn process_ram() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut pmc = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        PageFaultCount: 0,
        PeakWorkingSetSize: 0,
        WorkingSetSize: 0,
        QuotaPeakPagedPoolUsage: 0,
        QuotaPagedPoolUsage: 0,
        QuotaPeakNonPagedPoolUsage: 0,
        QuotaNonPagedPoolUsage: 0,
        PagefileUsage: 0,
        PeakPagefileUsage: 0,
    };
    unsafe {
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) != 0 {
            pmc.WorkingSetSize as u64
        } else {
            0
        }
    }
}

#[cfg(not(windows))]
fn process_ram() -> u64 {
    0
}

/// Hand the scan's peak pages back to Windows: min = max working set trims to standby.
#[cfg(windows)]
fn trim_ram() {
    use windows_sys::Win32::System::Memory::SetProcessWorkingSetSizeEx;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        SetProcessWorkingSetSizeEx(GetCurrentProcess(), usize::MAX, usize::MAX, 0);
    }
}

#[cfg(not(windows))]
fn trim_ram() {}

/// "13 Nov 14:32" in the device's local time (GetLocalTime on Windows); the non-Windows
/// fallback reads the wall clock as UTC.
fn wallclock() -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut st = unsafe { std::mem::zeroed() };
        unsafe { GetLocalTime(&mut st) };
        return format!(
            "{} {} {:02}:{:02}",
            MONTHS[(st.wMonth.max(1) - 1) as usize % 12],
            st.wDay,
            st.wHour,
            st.wMinute
        );
    }
    #[cfg(not(windows))]
    {
        // Howard Hinnant's civil-from-days.
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let days = secs.div_euclid(86_400);
        let tod = secs.rem_euclid(86_400);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        format!(
            "{} {} {:02}:{:02}",
            MONTHS[(m - 1) as usize],
            d,
            tod / 3600,
            (tod % 3600) / 60
        )
    }
}

fn setup(s: &mut State, waker: Waker) {
    s.art.start(waker.clone());
    s.waker = Some(waker);
    match s.updates.started() {
        Some(update::Outcome::Updated(v)) => s.toast(format!("Updated to {v}")),
        Some(update::Outcome::RolledBack(v)) => {
            s.toast_error(format!("Version {v} didn't start, so Projects stayed on {}", update::version::Version::current()))
        }
        None => {}
    }
    s.apps = launch::installed();
    if !s.demo {
        s.request_scan();
    }
}

fn scene_home(s: &mut State) {
    s.load_demo();
    s.saved.toggle_pin("ply");
    s.saved.toggle_pin("yotes");
    s.saved.mark_opened("messenger");
    s.saved.mark_opened("clai");
    s.saved.mark_opened("camx2");
    s.home.selected = Some("chinchilla".into());
}

fn scene_detail(s: &mut State) {
    scene_home(s);
    s.home.selected = Some("ply".into());
    s.home.detail = true;
}

fn scene_list(s: &mut State) {
    scene_home(s);
    s.saved.layout = saved::Layout::List;
}

fn scene_storage(s: &mut State) {
    s.load_demo();
    s.page = shell::Page::Storage;
}

fn scene_storage_open(s: &mut State) {
    scene_storage(s);
    s.ui.set_row(1);
    s.open();
    s.ui.set_row(0);
}

fn scene_usage(s: &mut State) {
    s.load_demo();
    s.page = shell::Page::Usage;
    // Hold the chart crosshair on a day, so the scene shows the tooltip.
    s.usage.hover = Some(22);
}

fn scene_templates(s: &mut State) {
    s.load_demo();
    s.page = shell::Page::Templates;
    s.tmpl.select(2);
}

fn scene_settings(s: &mut State) {
    s.load_demo();
    s.page = shell::Page::Settings;
    s.apps = vec![
        launch::App { id: "vscode", name: "VS Code", exe: None },
        launch::App { id: "cursor", name: "Cursor", exe: None },
        launch::App { id: "zed", name: "Zed", exe: None },
    ];
    s.saved.roots = vec![s.root.clone(), r"D:\Work".into()];
    s.saved.archived.insert("llama.cpp".into());
}

/// Settings with a downloaded update waiting: the sidebar line and the Updates group.
fn scene_update(s: &mut State) {
    scene_settings(s);
    let st = &mut s.updates.state;
    st.disabled = None;
    st.checked_at = Some(saved::now_unix() - 1500);
    st.ready = Some(update::machine::Staged {
        version: update::version::Version { major: st.current.major, minor: st.current.minor + 1, patch: 0 },
        exe: PathBuf::new(),
        sha256: String::new(),
        sig: String::new(),
        notes: vec![
            "Storage groups node_modules by package manager".into(),
            "Fix a scan stalling on folders without read access".into(),
        ],
    });
}

fn scene_new(s: &mut State) {
    scene_home(s);
    let root = s.root.clone();
    s.newp.open(&root);
    s.newp.name.replace_text("pocket-ledger");
    s.newp.template = 1;
}

fn scene_running(s: &mut State) {
    scene_new(s);
    s.newp.phase = newproj::Phase::Running;
    s.newp.running = s.tmpl.list.get(2).cloned();
    s.newp.step = Some(1);
    s.newp.log = vec![
        "$ npm create vite@latest pocket-ledger -- --template react-ts".into(),
        r"Scaffolding project in P:\Projects\pocket-ledger...".into(),
        "Done. Now run:".into(),
        "  cd pocket-ledger".into(),
        "  npm install".into(),
        "$ npm install".into(),
        "added 214 packages, and audited 215 packages in 9s".into(),
        "48 packages are looking for funding".into(),
    ];
}

fn scene_empty(s: &mut State) {
    s.load_demo();
    s.home.search.replace_text("zzz");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // What Settings, Apps runs.
        Some("--uninstall") => return install::uninstall(),
        // The old exe, watching the new one start after an update.
        Some("--update-watch") => return update::watch(&args[1..]),
        _ => {}
    }
    if !args.is_empty() {
        install::sys::attach_parent_console();
    }
    // The window and taskbar icon (Windows); a no-op elsewhere. The exe's own icon comes from build.rs.
    let _ = rune::set_app_icon(include_bytes!("../assets/icon-256.png").to_vec());
    let mode = install::mode(&args);
    match &mode {
        install::Mode::Setup => return install::ui::run(),
        // An update downloaded last time and not installed then goes in before the window opens.
        install::Mode::Installed(exe) if update::apply_pending_at_launch(exe) => return,
        _ => {}
    }
    let _ = MODE.set(mode);
    rune::run(AppSpec::<State> {
        scenes: &[
            ("demo", scene_home),
            ("demo-detail", scene_detail),
            ("demo-list", scene_list),
            ("demo-storage", scene_storage),
            ("demo-storage-open", scene_storage_open),
            ("demo-usage", scene_usage),
            ("demo-templates", scene_templates),
            ("demo-settings", scene_settings),
            ("demo-update", scene_update),
            ("demo-new", scene_new),
            ("demo-running", scene_running),
            ("demo-empty", scene_empty),
        ],
        setup: Some(setup),
        poll: Some(poll),
        // Everything on screen sets in the mono slot: the platform's monospaced UI font,
        // Cascadia Mono on Windows. Figures line up without any tabular trick.
        families: ("Inter", "ui-monospace"),
        on_key: Some(shell::on_key),
        ..AppSpec::new(
            "Projects",
            (1200.0, 850.0),
            State::new,
            shell::view,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::Runtime;

    #[test]
    fn the_footer_shows_a_live_frame_rate_once_frames_have_run() {
        let mut rt = demo_runtime();
        for i in 1..=140 {
            rt.frame(i as f64 / 60.0);
        }
        let line = rt.state.engine_line();
        assert!(line.contains("fps"), "no frame rate: {line:?}");
        // At 60 fps the window closes several times over 140 frames, so the reading is real
        // rather than a placeholder that never updated.
        assert!(rt.state.fps > 0.0, "the gauge never updated: {line:?}");
    }

    fn demo_runtime() -> Runtime<State> {
        let env = rune::Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut state = State::new();
        state.load_demo();
        state.page = shell::Page::Storage;
        let mut rt = Runtime::new(state, shell::view, (1180.0, 820.0), env);
        rt.poll = Some(poll);
        rt.frame(0.0);
        rt
    }

    #[test]
    fn the_projects_page_is_home_and_lists_every_project() {
        let mut state = State::new();
        state.load_demo();
        let env = rune::Env { system_dark: true, scale: 2.0 };
        let mut rt = Runtime::new(state, shell::view, (1180.0, 820.0), env);
        rt.frame(0.0);
        assert!(rt.find("home").is_some());
        assert!(rt.find("search").is_some());
        assert!(rt.find("nav/usage").is_some());
        let names: Vec<String> = rt.state.projects.iter().map(|p| p.name.clone()).collect();
        for name in &names {
            assert!(rt.find(&format!("card/{name}")).is_some(), "{name} has a card");
        }
        rt.update(|s, _| s.go(shell::Page::Usage));
        rt.frame(0.1);
        assert!(rt.find("usage").is_some());
    }

    fn texts(rt: &Runtime<State>) -> Vec<String> {
        rt.elements().into_iter().filter_map(|e| e.text).collect()
    }

    #[test]
    fn the_page_shows_the_meter_the_categories_and_the_inside_pane() {
        let rt = demo_runtime();
        assert!(rt.find("meter").is_some(), "the meter is laid out");
        assert!(rt.find("categories").is_some(), "the legend is laid out");
        assert!(rt.find("inside").is_some(), "the detail pane is laid out");
        assert!(rt.find("topline").is_some());
        assert!(rt.find("bottom").is_some());
        assert_eq!(rt.state.projects.len(), 16);
        assert!(!rt.state.scanning);
    }

    #[test]
    fn the_meter_sits_below_the_totals_and_fills_the_width() {
        let rt = demo_runtime();
        let (totals, _) = rt.presented("totals").expect("totals laid out");
        let (meter, _) = rt.presented("meter").expect("meter laid out");
        assert!(
            meter[1] >= totals[1] + totals[3] - 0.5,
            "meter below totals: {meter:?} vs {totals:?}"
        );
        assert!(meter[3] > 0.0, "the meter has height: {meter:?}");
        let (page, _) = rt.presented("page").expect("page laid out");
        assert!(
            meter[2] <= page[2] + 0.5,
            "meter {} fits the page {}",
            meter[2],
            page[2]
        );
    }

    #[test]
    fn every_category_has_a_line_with_its_size() {
        let rt = demo_runtime();
        let sizes = rt.state.ui.category_sizes(&rt.state.projects);
        for cat in cats::CATEGORIES {
            let key = format!("cat/{}", cat.id.label());
            assert!(rt.find(&key).is_some(), "{key} is laid out");
        }
        let total: u64 = sizes.iter().sum();
        let summed: u64 = rt.state.projects.iter().map(|p| p.bytes).sum();
        assert_eq!(total, summed, "the categories account for every byte");
    }

    #[test]
    fn the_totals_line_sums_the_disk() {
        let rt = demo_runtime();
        let sizes = rt.state.ui.category_sizes(&rt.state.projects);
        let total: u64 = sizes.iter().sum();
        let want = model::gb(total);
        let line: String = texts(&rt).join(" ");
        assert!(line.contains(&want), "totals show {want}: {line}");
        let rebuildable = model::gb(sizes[1] + sizes[2]);
        assert!(line.contains(&rebuildable), "rebuildable {rebuildable} shows");
    }

    #[test]
    fn no_keyboard_hints_are_on_screen() {
        // The port drops the terminal: actions are controls, not key labels to memorise.
        let rt = demo_runtime();
        let joined: String = texts(&rt).join(" ");
        for hint in [
            "[q]", "[r]", "[g]", "[f]", "[o]", "[c]", "[d]", "⌫", "Tab", "press q",
            "↑↓", "Esc",
        ] {
            assert!(!joined.contains(hint), "no key hint {hint:?} in {joined}");
        }
    }

    #[test]
    fn picking_a_category_by_pointer_changes_the_pane() {
        let mut rt = demo_runtime();
        rt.update(|s, _| s.pick(cats::Cat::Build));
        rt.frame(0.0);
        assert_eq!(rt.state.ui.filter, store::Filter::Only(cats::Cat::Build));
        let entries = rt.state.ui.entries(&rt.state.projects);
        let names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
        let want: Vec<String> = rt
            .state
            .projects
            .iter()
            .filter(|p| p.target > p.src)
            .map(|p| p.name.clone())
            .collect();
        assert!(
            want.iter().all(|w| names.contains(w)),
            "every build-heavy project is listed: {names:?}"
        );
    }

    #[test]
    fn a_row_under_the_pointer_shows_its_breakdown() {
        let rt = demo_runtime();
        let entries = rt.state.ui.entries(&rt.state.projects);
        assert!(!entries.is_empty(), "the demo has rows");
        let big = entries[0].clone();
        let label = rt.state.ui.label(&big, &rt.state.projects);
        assert!(big.size > 0);
        // The biggest row of a category is one of its projects, so it is "yours".
        assert_eq!(label.verdict, cats::Verdict::Yours);
    }

    #[test]
    fn the_detail_card_fits_the_sidebar_column() {
        // The card was moved into the sidebar without being redesigned for its width, and its
        // row ran wider than the column, so the excess drew over the table beside it. A long
        // name is the case that exposes it, so the card is measured with one.
        let mut rt = demo_runtime();
        let parent = rt.state.ui.entries(&rt.state.projects)[0].clone();
        let long = "a-folder-name-long-enough-to-need-the-whole-sidebar-and-more";
        let child = browse::Item::folder(parent.path.join(long), long.to_string(), 9_000);
        rt.update(|s, _| {
            s.ui.stack.push(parent);
            s.ui.children = vec![child.clone()];
            s.ui.listing = false;
            s.ui.row = 0;
            s.ui.culprits.insert(
                child.path.clone(),
                Some(browse::Culprit {
                    chain: vec![
                        child.clone(),
                        browse::Item::folder(child.path.clone(), long.to_string(), 9_000),
                    ],
                    top: Vec::new(),
                    root_size: child.size,
                }),
            );
        });
        rt.frame(0.0);
        let (card, _) = rt.presented("detail").expect("the card is laid out");
        let (column, _) = rt.presented("categories").expect("the column is laid out");
        assert!(
            card[0] + card[2] <= column[0] + column[2] + 0.5,
            "the card ends at {} in a column that ends at {}, so it draws over the table",
            card[0] + card[2],
            column[0] + column[2]
        );
        // The row it describes is still named, so the fit is not achieved by showing nothing.
        assert!(
            rt.elements()
                .into_iter()
                .filter_map(|e| e.text)
                .any(|t| t.contains("a-folder-name-long")),
            "the card still names the row it describes"
        );
    }

    #[test]
    fn the_sidebar_names_the_biggest_folder_worth_clearing() {
        // The reclaim line replaced a filter that hid rows. It has to name a real row from
        // the list and a size, so the offer is something the reader can act on.
        let rt = demo_runtime();
        let (item, label) = rt
            .state
            .ui
            .biggest_reclaimable(&rt.state.projects)
            .expect("the demo has a folder worth clearing");
        assert!(
            label.verdict == cats::Verdict::Regenerates,
            "it is a folder a command brings back: {label:?}"
        );
        assert!(item.size > 0, "and it has bytes to reclaim");
        assert!(
            !label.reason.is_empty(),
            "and the line can say why it is safe"
        );
        // It is also in the list the table shows, so pressing it lands on a real row.
        let listed = rt
            .state
            .ui
            .entries(&rt.state.projects)
            .iter()
            .any(|e| e.path == item.path);
        assert!(listed, "the named folder is one of the rows on screen");
    }

    #[test]
    fn opening_a_project_lists_what_is_inside_and_back_returns() {
        let mut rt = demo_runtime();
        let first = rt.state.ui.entries(&rt.state.projects)[0].clone();
        assert!(first.dir);
        rt.update(|s, _| {
            s.ui.set_row(0);
        });
        rt.update(|s, _| s.open());
        // The worker fills the listing; give it a moment.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline && rt.state.ui.children.is_empty() {
            rt.update(|s, _| {
                s.ui.drain();
            });
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(!rt.state.ui.stack.is_empty(), "the folder opened");
        assert!(
            !rt.state.ui.children.is_empty(),
            "a project folder has something in it"
        );
        rt.update(|s, _| s.back());
        assert!(rt.state.ui.stack.is_empty(), "back returns to the category");
        rt.frame(0.0);
    }

    #[test]
    fn selecting_a_row_never_moves_the_list() {
        let mut rt = demo_runtime();
        let row_rects = |rt: &Runtime<State>| -> Vec<[f32; 4]> {
            let mut v: Vec<(usize, [f32; 4])> = rt
                .elements()
                .into_iter()
                .filter_map(|e| {
                    let k = e.key?;
                    let n = k.strip_prefix("row/")?.parse::<usize>().ok()?;
                    Some((n, e.rect))
                })
                .collect();
            v.sort_by_key(|(n, _)| *n);
            v.into_iter().map(|(_, r)| r).collect()
        };
        let before = row_rects(&rt);
        assert!(!before.is_empty(), "the demo lists rows");
        // Pick a row two down, then one further, and the list must stay put both times.
        for row in [1usize, 2] {
            rt.update(move |s, _| s.ui.set_row(row));
            rt.frame(0.0);
            let after = row_rects(&rt);
            assert_eq!(
                before, after,
                "picking row {row} moved the rows it was picked from"
            );
        }
        // The detail lives in the sidebar, so it never sits between the rows.
        let (detail, _) = rt.presented("details").expect("the details block is laid out");
        let (rows, _) = rt.presented("rows").expect("the rows are laid out");
        assert!(
            detail[0] + detail[2] <= rows[0] + 0.5,
            "the details stay in the left column: {detail:?} vs {rows:?}"
        );
    }

    #[test]
    fn every_header_sits_over_the_column_it_names() {
        let rt = demo_runtime();
        // Find the header by its label and the row cell by the text it shows.
        let at = |want: &str| -> Option<[f32; 4]> {
            rt.elements()
                .into_iter()
                .find(|e| e.text.as_deref().is_some_and(|t| t.starts_with(want)))
                .map(|e| e.rect)
        };
        for header in ["Folder", "Size", "Last commit"] {
            assert!(at(header).is_some(), "header {header} is laid out");
        }
        let h = at("Size").unwrap();
        let sizecells: Vec<f32> = rt
            .elements()
            .into_iter()
            .filter(|e| {
                e.text
                    .as_deref()
                    .is_some_and(|t| t.ends_with(" GiB") || t.ends_with(" MiB"))
            })
            .map(|e| e.rect[0] + e.rect[2])
            .collect();
        assert!(
            sizecells.iter().any(|right| (right - (h[0] + h[2])).abs() < 1.0),
            "a size cell ends where the size header does: header {}, cells {:?}",
            h[0] + h[2],
            sizecells
        );
    }

    #[test]
    fn clicking_a_breadcrumb_walks_back_up_the_path() {
        let mut rt = demo_runtime();
        // Open the first project folder, then a folder inside it.
        // Some demo projects hold only files, so walk the list until one opens onto a folder.
        let rows = rt.state.ui.entries(&rt.state.projects);
        for row in (0..rows.len()).filter(|&i| rows[i].dir) {
            rt.update(move |s, _| {
                s.ui.set_row(row);
                s.open();
            });
            rt.frame(0.0);
            let inner = rt.state.ui.entries(&rt.state.projects);
            if let Some(inner_row) = inner.iter().position(|i| i.dir) {
                rt.update(move |s, _| {
                    s.ui.set_row(inner_row);
                    s.open();
                });
                rt.frame(0.0);
                break;
            }
            rt.update(|s, _| s.back());
            rt.frame(0.0);
        }
        assert_eq!(rt.state.ui.stack.len(), 2, "two folders deep");

        // The crumb for the first folder is a real target under the pointer.
        // Which laid-out elements cover the point, and do any of them take input?
        // Press the first crumb: it must land on that crumb, not on its neighbour.
        // Let the trail's entry animation finish before clicking on it.
        for _ in 0..12 {
            rt.frame(1.0 / 60.0);
        }
        let (r, _) = rt.presented("crumb/1").expect("the first crumb is laid out");
        let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        assert_eq!(
            rt.state.ui.stack.len(),
            1,
            "clicking the first crumb goes back to it"
        );

        // And the category crumb walks all the way out.
        let (root, _) = rt.presented("crumb/0").expect("the category crumb is laid out");
        let (x, y) = (root[0] + root[2] / 2.0, root[1] + root[3] / 2.0);
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        assert!(
            rt.state.ui.stack.is_empty(),
            "the category crumb returns to the category"
        );
    }

    #[test]
    fn there_is_no_back_button_competing_with_the_crumbs() {
        let mut rt = demo_runtime();
        let row = rt
            .state
            .ui
            .entries(&rt.state.projects)
            .iter()
            .position(|i| i.dir)
            .unwrap();
        rt.update(move |s, _| {
            s.ui.set_row(row);
            s.open();
        });
        rt.frame(0.0);
        assert!(rt.find("crumb/1").is_some(), "the trail is there");
        assert!(rt.find("back").is_none(), "and there is no back button");
    }

    #[test]
    fn a_click_on_the_meter_picks_the_segment_under_it() {
        let mut rt = demo_runtime();
        // The app loop drives `poll`; here it is stepped by hand so the meter settles and
        // knows where its segments are.
        for step in 0..20 {
            rt.update(|s, _| {
                s.tick_meter(step as f64 * 0.05);
            });
            rt.frame(1.0 / 60.0);
        }
        let (rect, _) = rt.presented("meter").expect("meter laid out");
        let y = rect[1] + rect[3] - 4.0;
        let mut picked = None;
        for step in 1..8 {
            let x = rect[0] + rect[2] * (step as f32 / 8.0);
            rt.pointer_move(x, y);
            rt.frame(0.0);
            if let Some(cat) = rt.state.ui.hover {
                picked = Some(cat);
                rt.pointer_down(x, y);
                rt.pointer_up(x, y);
                rt.frame(0.0);
                break;
            }
        }
        assert!(picked.is_some(), "some column of the meter is a segment");
        assert_eq!(
            rt.state.ui.filter,
            store::Filter::Only(picked.unwrap()),
            "the click picked what the pointer was over"
        );
    }
}


#[cfg(test)]
mod scan_flow_tests {
    use super::*;

    #[test]
    fn rescan_keeps_the_old_rows_until_new_numbers_land() {
        let mut s = State::new();
        s.load_demo();
        let before = s.projects.len();
        s.scanning = true;
        // A fresh folder's numbers land as one row the moment they arrive, replacing
        // any row of the same name; the old list is never cleared mid-scan. The scratch
        // root is unique to this test so a parallel test cannot share its tree.
        let root = std::env::temp_dir().join(format!("ps-rescan-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("fresh/.git")).unwrap();
        std::fs::write(root.join("fresh/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let (tx, rx) = channel();
        let walked = root.clone();
        std::thread::spawn(move || {
            scan::scan(walked, tx, GitCache::default(), || {});
        });
        s.rx = Some(rx);
        // Long enough that a loaded machine still finishes: the walk is one tiny folder, but
        // the suite runs these in parallel on a cold cache, and a timeout here used to look
        // like a stale-row bug instead of a test that gave up early.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut landed = 0;
        let mut seen_row = false;
        while std::time::Instant::now() < deadline {
            if s.drain(0.0) {
                landed += 1;
            }
            if s.projects.iter().any(|p| p.name == "fresh") {
                seen_row = true;
            }
            // Done has to arrive before the list is judged: the demo's rows go at that
            // point, and only `fresh` is reported by this small root. Count events until
            // then, so the drain-proof is the scan finishing rather than one lucky poll.
            if !s.scanning {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!s.scanning, "the scan finished within the deadline");
        assert!(landed > 0, "events drain");
        assert!(seen_row, "the fresh folder's row was delivered mid-scan");
        // The scan is of a throwaway root holding one folder, so the demo rows it never
        // reported are gone by the time it finishes: the list is exactly the walked tree.
        assert_eq!(
            s.projects.len(),
            1,
            "only the walked folder remains: {:?}",
            s.projects.iter().map(|p| p.name.clone()).collect::<Vec<_>>()
        );
        assert_eq!(s.projects[0].name, "fresh");
        assert!(before > 1, "the demo list had rows before the scan");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rows_arrive_while_the_scan_runs() {
        // The window fills as folders are measured, not in one burst at the end. This is the
        // only test that walks the real root, on a state of its own, so no other test's
        // list can receive its rows.
        let root = PathBuf::from("P:/Projects");
        if !root.is_dir() {
            return;
        }
        let mut s = State::new();
        s.request_scan();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut seen: Vec<usize> = Vec::new();
        let mut last = 0;
        while std::time::Instant::now() < deadline {
            s.drain(0.0);
            if s.projects.len() != last {
                last = s.projects.len();
                seen.push(last);
            }
            if !s.scanning {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(seen.len() > 2, "rows land in several waves: {:?}", seen);
        assert!(
            *seen.first().unwrap() < *seen.last().unwrap(),
            "the list grows as the scan runs: {:?}",
            seen
        );
    }

    #[test]
    fn a_dead_scanner_never_leaves_the_app_scanning() {
        let mut s = State::new();
        s.scanning = true;
        let (tx, rx) = channel();
        s.rx = Some(rx);
        drop(tx);
        assert!(s.drain(0.0), "the scanner's death is a visible change");
        assert!(!s.scanning, "the scanning state ends");
        assert!(s.error.is_some(), "the user sees why");
        assert!(s.rx.is_none(), "the dead channel is dropped");
        // And it stays ended: draining again is quiet.
        assert!(!s.drain(0.0));
    }

    #[test]
    fn a_folder_that_vanished_is_dropped_when_the_scan_finishes() {
        let mut s = State::new();
        s.load_demo();
        let kept = s.projects[0].name.clone();
        let gone = s.projects[1].name.clone();
        s.scanning = true;
        let (tx, rx) = channel();
        s.rx = Some(rx);
        // Only one of the two folders is still on disk this scan.
        let survivor = s.projects[0].clone();
        tx.send(ScanEvent::Project(Box::new(survivor))).unwrap();
        tx.send(ScanEvent::Done { seconds: 1.0 }).unwrap();
        assert!(s.drain(0.0));
        let names: Vec<&str> = s.projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, [kept.as_str()], "the vanished folder is gone: {names:?}");
        assert!(!names.contains(&gone.as_str()));
        assert_eq!(s.done_count, 1, "the progress count matches the list");
    }

    #[test]
    fn a_finished_scan_leaves_no_channel_behind() {
        let mut s = State::new();
        s.scanning = true;
        let (tx, rx) = channel();
        s.rx = Some(rx);
        tx.send(ScanEvent::Done { seconds: 1.0 }).unwrap();
        drop(tx);
        assert!(s.drain(0.0));
        assert!(!s.scanning && s.rx.is_none());
        // The Done was already handled: the later disconnect must not raise a fake error.
        assert!(!s.drain(0.0));
        assert!(s.error.is_none());
    }
}
