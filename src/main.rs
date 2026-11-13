//! Projects — live stats for a folder of code, built with Rune.
//! `cargo run -p projects-stats` opens the window and starts scanning;
//! `--scene demo` opens the window on the built-in fixture instead of scanning.

mod charts;
mod fixture;
mod model;
mod scan;
mod ui;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use rune::{AppSpec, Waker};
use model::ScanEvent;
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
    /// Frames per second over the last 2 s window.
    pub fps: f32,
    /// This process's working set, sampled once a second.
    pub ram_bytes: u64,
    /// Name of the item under the cursor, if any (drives the inspector bar).
    pub hover: Option<String>,
    /// Name of the clicked/pinned item — survives cursor movement until clicked again.
    pub pinned: Option<String>,
    fps_frames: u32,
    fps_since: f64,
    last_trim: f64,
    /// Git facts carried between scans, keyed by project path with a refs-mtime validity
    /// key — rescans skip the history walk for repos that haven't moved.
    pub git_cache: GitCache,
}

const RESCAN_EVERY: f64 = 300.0; // seconds

impl State {
    fn new() -> State {
        let root = std::env::var("RUNE_STATS_ROOT").unwrap_or_else(|_| r"P:\Projects".into());
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
            demo: false,
            fps: 0.0,
            ram_bytes: 0,
            hover: None,
            pinned: None,
            fps_frames: 0,
            fps_since: 0.0,
            last_trim: 0.0,
            git_cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
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
            parts.push(format!("scan {}", crate::charts::scan_time(self.scan_secs)));
        }
        parts.join(" · ")
    }

    fn load_demo(&mut self) {
        self.demo = true;
        self.scanning = false;
        self.error = None;
        self.projects = fixture::demo_projects();
        self.scan_secs = 0.0;
        self.last_done = Some(0.0);
    }

    /// Start a scan unless one is already running or demo mode holds the data.
    fn request_scan(&mut self) {
        if self.scanning || self.demo {
            return;
        }
        self.scanning = true;
        self.error = None;
        self.projects.clear();
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
    /// Drain the scanner's channel into state; true when anything arrived.
    pub fn drain(&mut self, clock: f64) -> bool {
        let mut changed = false;
        while let Some(rx) = self.rx.as_ref() {
            match rx.try_recv() {
                Ok(ScanEvent::Start { total }) => {
                    self.total = total;
                    changed = true;
                }
                Ok(ScanEvent::Project(p)) => {
                    self.current = p.name.clone();
                    self.done_count += 1;
                    self.projects.push(*p);
                    changed = true;
                }
                Ok(ScanEvent::Done { seconds }) => {
                    eprintln!("scan done in {seconds:.1}s");
                    self.scanning = false;
                    self.scan_secs = seconds;
                    self.last_done = Some(clock);
                    self.done_at = Some(wallclock());
                    // The scan's walkers and buffers just peaked the working set; hand
                    // those pages back so the app idles small.
                    trim_ram();
                    self.last_trim = clock;
                    changed = true;
                }
                Ok(ScanEvent::Failed(e)) => {
                    self.scanning = false;
                    self.error = Some(e);
                    changed = true;
                }
                Err(_) => break,
            }
        }
        changed
    }
}

fn poll(s: &mut State, clock: f64, _tx: &mut rune::Tx) -> bool {
    let mut changed = s.drain(clock);

    // Engine gauges: frame rate over a 2 s window (a 1 s window flickers between 59 and
    // 61) and resident memory sampled with it. The footer only commits when a displayed
    // value actually changes, so a steady frame rate costs nothing.
    s.fps_frames += 1;
    let elapsed = clock - s.fps_since;
    if elapsed >= 2.0 {
        let fps = (s.fps_frames as f32 / elapsed as f32).round();
        let ram = process_ram() / 1048576;
        s.fps_frames = 0;
        s.fps_since = clock;
        let fps_shown = s.fps as u32;
        let ram_shown = s.ram_bytes / 1048576;
        if fps != fps_shown as f32 || ram != ram_shown {
            s.fps = fps;
            s.ram_bytes = ram * 1048576;
            changed = true;
        }
    }
    // At rest, hand pages back to Windows every 30 s: the working set creeps up as the
    // GPU driver and allocator touch pages, and a dashboard that is sitting still has no
    // use for them.
    if !s.scanning && clock - s.last_trim >= 30.0 {
        trim_ram();
        s.last_trim = clock;
    }

    if !s.scanning && !s.demo && s.last_done.is_none_or(|t| clock - t >= RESCAN_EVERY) {
        s.request_scan();
        changed = true;
    }
    changed
}

#[cfg(windows)]
fn process_ram() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
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
    s.waker = Some(waker);
    if !s.demo {
        s.request_scan();
    }
}

fn main() {
    rune::run(AppSpec::<State> {
        scenes: &[("demo", State::load_demo)],
        setup: Some(setup),
        poll: Some(poll),
        // The mono slot is the platform's monospaced UI font: Cascadia Mono on Windows.
        // Every figure and axis label sets in it, tabular by nature.
        families: ("Inter", "ui-monospace"),
        ..AppSpec::new("Projects — Live Stats", (1100.0, 780.0), State::new, ui::view)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::Runtime;
    use std::process::Command;

    fn demo_runtime() -> Runtime<State> {
        let env = rune::Env { system_dark: true, scale: 2.0 };
        let mut state = State::new();
        state.load_demo();
        let mut rt = Runtime::new(state, ui::view, (1100.0, 780.0), env);
        rt.frame(0.0);
        rt
    }

    #[test]
    fn demo_scene_shows_the_dashboard() {
        let rt = demo_runtime();
        assert!(rt.find("treemap").is_some(), "treemap is laid out");
        assert!(rt.find("scatter").is_some(), "scatter is laid out");
        assert!(rt.find("rescan").is_some(), "rescan button exists");
        assert_eq!(rt.state.projects.len(), 16);
        assert!(!rt.state.scanning);
    }

    #[test]
    fn treemap_tiles_lay_out_inside_the_map() {
        let rt = demo_runtime();
        let (rect, _) = rt.presented("treemap").expect("treemap laid out");
        assert!((rect[2] - 1008.0).abs() < 2.0, "treemap width {}", rect[2]);
        assert!((rect[3] - 300.0).abs() < 2.0, "treemap height {}", rect[3]);
        let (tile, _) = rt
            .presented("tile/chinchilla")
            .unwrap_or_else(|| panic!("biggest tile present"));
        assert!(tile[2] > 100.0, "chinchilla tile is wide: {tile:?}");
        // Every tile stays inside the map.
        for info in rt.elements() {
            if info.key.as_deref().is_some_and(|k| k.starts_with("tile/")) {
                let key = info.key.clone().unwrap();
                assert!(info.rect[2] <= 1008.0 + 0.5 && info.rect[3] <= 300.0 + 0.5,
                    "{key} overflows: {:?}", info.rect);
            }
        }
    }

    #[test]
    fn kpi_card_shows_the_summed_disk_total() {
        let rt = demo_runtime();
        let bytes: u64 = rt.state.projects.iter().map(|p| p.bytes).sum();
        let want = format!("{:.1} GiB", model::gib(bytes));
        let texts: Vec<String> = rt.elements().into_iter().filter_map(|e| e.text).collect();
        assert!(
            texts.iter().any(|t| *t == want),
            "on-disk KPI should read {want}: {texts:?}"
        );
    }

    #[test]
    fn rescan_button_does_not_clobber_demo_data() {
        let mut rt = demo_runtime();
        let (x, y) = rt.find("rescan").unwrap();
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        assert!(!rt.state.scanning, "demo data stays put");
        assert_eq!(rt.state.projects.len(), 16);
    }

    #[test]
    fn scan_a_real_tiny_root_end_to_end() {
        // A throwaway root with one fake project: two source files, node_modules junk, a git repo.
        let root = std::env::temp_dir().join(format!("rune-stats-test-{}", std::process::id()));
        let proj = root.join("demo-one");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::create_dir_all(proj.join("node_modules")).unwrap();
        std::fs::write(proj.join("src/main.rs"), "fn main() {}\nfn a() {}\nfn b() {}\n").unwrap();
        std::fs::write(proj.join("src/lib.ts"), "export const x = 1;\nexport const y = 2;\n").unwrap();
        std::fs::write(proj.join("node_modules/big.js"), "x".repeat(4096)).unwrap();
        std::fs::write(proj.join("package.json"), r#"{ "dependencies": { "next": "1" } }"#).unwrap();
        let git_ok = Command::new("git").args(["init", "-q"]).current_dir(&proj).status().map(|s| s.success()).unwrap_or(false);
        if git_ok {
            Command::new("git").args(["config", "user.email", "t@t"]).current_dir(&proj).status().unwrap();
            Command::new("git").args(["config", "user.name", "t"]).current_dir(&proj).status().unwrap();
            Command::new("git").args(["add", "."]).current_dir(&proj).status().unwrap();
            Command::new("git").args(["commit", "-qm", "init"]).current_dir(&proj).status().unwrap();
        }

        let (tx, rx) = channel();
        let scan_root = root.clone();
        let cache: GitCache = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let handle = std::thread::spawn(move || scan::scan(scan_root, tx, cache, || {}));
        let mut projects = Vec::new();
        let mut done = false;
        while !done {
            match rx.recv().unwrap() {
                ScanEvent::Project(p) => projects.push(*p),
                ScanEvent::Done { .. } => done = true,
                ScanEvent::Start { total } => assert_eq!(total, 1),
                ScanEvent::Failed(e) => panic!("scan failed: {e}"),
            }
        }
        handle.join().unwrap();
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(projects.len(), 1);
        let p = &projects[0];
        assert_eq!(p.name, "demo-one");
        assert_eq!(p.cat, "Web App", "package.json with next");
        assert!(p.stack.contains("Next.js"));
        assert!(p.files >= 4, "hidden .git files count too: {}", p.files);
        assert_eq!(p.nm, 4096, "node_modules bytes bucketed");
        if git_ok {
            assert!(p.other > 0, ".git history lands in the other bucket");
        }
        assert!(p.loc >= 5, "hand-written lines counted: {}", p.loc);
        assert!(p.langs.iter().any(|(l, _)| l == "Rust"));
        assert!(p.langs.iter().any(|(l, _)| l == "TypeScript"));
        if git_ok {
            assert!(p.git && p.commits >= 1 && p.days.is_some());
        }
    }

    /// Run against the real root to check the live pipeline, the git cache and its timing:
    /// `cargo test -p projects-stats real_root -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_root_scan_smoke() {
        let root = PathBuf::from(std::env::var("RUNE_STATS_ROOT").unwrap_or_else(|_| r"P:\Projects".into()));
        let cache: GitCache = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let (first, cold) = run_real_scan(root.clone(), cache.clone());
        let (second, warm) = run_real_scan(root, cache);
        let bytes: u64 = first.iter().map(|p| p.bytes).sum();
        let loc: u64 = first.iter().map(|p| p.loc).sum();
        println!(
            "cold {:.1}s, warm rescan {:.1}s over {} folders, {:.1} GiB, {} LOC",
            cold,
            warm,
            first.len(),
            model::gib(bytes),
            loc
        );
        assert!(!first.is_empty());
        assert_eq!(first.len(), second.len(), "rescan sees the same folders");
        for (a, b) in first.iter().zip(&second) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.commits, b.commits, "{}: cached commit count", a.name);
            assert_eq!(a.days, b.days, "{}: cached last-commit age", a.name);
        }
    }

    fn run_real_scan(root: PathBuf, cache: GitCache) -> (Vec<model::Project>, f64) {
        let (tx, rx) = channel();
        let handle = std::thread::spawn(move || scan::scan(root, tx, cache, || {}));
        let mut projects = Vec::new();
        let mut seconds = 0.0;
        while let Ok(ev) = rx.recv() {
            match ev {
                ScanEvent::Project(p) => projects.push(*p),
                ScanEvent::Done { seconds: s } => {
                    seconds = s;
                    break;
                }
                ScanEvent::Failed(e) => panic!("{e}"),
                ScanEvent::Start { .. } => {}
            }
        }
        handle.join().unwrap();
        (projects, seconds)
    }

    #[test]
    fn live_scan_drains_events_into_state() {
        let env = rune::Env { system_dark: true, scale: 2.0 };
        let (tx, rx) = channel();
        let mut state = State::new();
        state.rx = Some(rx);
        state.scanning = true;
        state.total = 2;
        let mut rt = Runtime::new(state, ui::view, (1100.0, 780.0), env);
        tx.send(ScanEvent::Start { total: 2 }).unwrap();
        tx.send(ScanEvent::Project(Box::new(model::Project {
            name: "demo-one".into(),
            cat: "Web App",
            stack: "Next.js".into(),
            bytes: 1000,
            files: 10,
            loc: 100,
            src: 900,
            nm: 100,
            target: 0,
            other: 0,
            langs: vec![("TypeScript".into(), 100)],
            git: true,
            commits: 5,
            days: Some(3),
            dirty: 1,
        })))
        .unwrap();
        tx.send(ScanEvent::Done { seconds: 1.0 }).unwrap();
        // `Runtime::frame` doesn't run the shell's poll; drain through an update so the
        // runtime sees the change and rebuilds the view.
        let mut drained = false;
        rt.update_if(|s, _| {
            drained = s.drain(0.0);
            drained
        });
        assert!(drained, "poll's drain saw the events");
        rt.frame(0.0);
        assert_eq!(rt.state.projects.len(), 1);
        assert_eq!(rt.state.done_count, 1);
        assert!(!rt.state.scanning, "Done clears the flag");
        assert_eq!(rt.state.projects[0].name, "demo-one");
        // The dashboard now shows the scanned folder.
        assert!(rt.find("tile/demo-one").is_some(), "scanned folder gets a tile");
    }
}

#[cfg(test)]
mod inspector_tests {
    use super::*;
    use rune::Runtime;

    fn demo() -> Runtime<State> {
        let env = rune::Env { system_dark: true, scale: 2.0 };
        let mut state = State::new();
        state.load_demo();
        let mut rt = Runtime::new(state, ui::view, (1100.0, 780.0), env);
        rt.frame(0.0);
        rt
    }

    fn inspector_text(rt: &Runtime<State>) -> String {
        rt.elements()
            .into_iter()
            .filter_map(|e| e.text)
            .find(|t| t.contains("hover a tile") || t.contains("\u{b7} pinned") || t.contains("dirty"))
            .unwrap_or_default()
    }

    #[test]
    fn hover_names_the_item_and_leaving_clears_it() {
        let mut rt = demo();
        assert!(inspector_text(&rt).contains("hover a tile"), "hint first");
        let (x, y) = rt.find("tile/chinchilla").expect("tile");
        rt.pointer_move(x, y);
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.hover.as_deref(), Some("chinchilla"));
        let texts: Vec<String> = rt.elements().into_iter().filter_map(|e| e.text).collect();
        assert!(texts.iter().any(|t| t == "chinchilla"), "inspector leads with the name");
        assert!(texts.iter().any(|t| t.contains("dirty")), "inspector shows the details");
        rt.pointer_leave();
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.hover, None, "leaving clears the preview");
    }

    #[test]
    fn click_pins_and_click_again_unpins() {
        // The scatter sits below the fold at the default window height.
        let env = rune::Env { system_dark: true, scale: 2.0 };
        let mut state = State::new();
        state.load_demo();
        let mut rt = Runtime::new(state, ui::view, (1100.0, 2500.0), env);
        rt.frame(0.0);
        let (x, y) = rt.find("dot/whirl").expect("dot");
        rt.pointer_move(x, y);
        rt.frame(1.0 / 60.0);

        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.pinned.as_deref(), Some("whirl"));
        rt.pointer_leave();
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.pinned.as_deref(), Some("whirl"), "pin survives leaving");
        let texts: Vec<String> = rt.elements().into_iter().filter_map(|e| e.text).collect();
        assert!(texts.iter().any(|t| t.contains("\u{b7} pinned")), "inspector shows the pin");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.pinned, None, "second click unpins");
    }
}
