//! Projects — live stats for a folder of code, built with Rune.
//! `cargo run -p projects-stats` opens the window and starts scanning;
//! `--scene demo` opens the window on the built-in fixture instead of scanning.

mod browse;
mod cats;
mod fixture;
mod meter;
mod model;
mod scan;
mod store;
mod ui;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

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
}

const RESCAN_EVERY: f64 = 300.0; // seconds

impl State {
    fn new() -> State {
        let root = std::env::var("RUNE_STATS_ROOT").unwrap_or_else(|_| r"P:\Projects".into());
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
            parts.push(format!("scan {:.1}s", self.scan_secs));
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
        self.ui.reset();
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
                    self.refreshing.clear();
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

    /// Shows a category in the detail pane, back at its top level.
    pub fn pick(&mut self, cat: cats::Cat) {
        self.ui.select(cat);
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
            hover: self.ui.hover.or(match self.ui.stack.is_empty() {
                true => Some(self.ui.selected),
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
    s.ui.want_culprit(&s.projects, s.demo);
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
        // Everything on screen sets in the mono slot: the platform's monospaced UI font,
        // Cascadia Mono on Windows. Figures line up without any tabular trick.
        families: ("Inter", "ui-monospace"),
        ..AppSpec::new(
            "Projects — Live Stats",
            (1180.0, 820.0),
            State::new,
            view,
        )
    });
}

/// The view: remembers the window's pixel scale so the meter can draw one pixel per pixel.
fn view(s: &State, cx: &mut rune::Cx<State>) -> rune::El {
    // The meter budgets its buffer in device pixels, so it needs the window's scale; it
    // picks it up on layout, which is where `meter_resize` learns the size anyway.
    let _ = cx.env().scale;
    ui::view(s, cx)
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use rune::Runtime;

    fn demo_runtime() -> Runtime<State> {
        let env = rune::Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut state = State::new();
        state.load_demo();
        let mut rt = Runtime::new(state, ui::view, (1180.0, 820.0), env);
        rt.frame(0.0);
        rt
    }

    fn texts(rt: &Runtime<State>) -> Vec<String> {
        rt.elements().into_iter().filter_map(|e| e.text).collect()
    }

    #[test]
    fn the_page_shows_the_meter_the_categories_and_the_inside_pane() {
        let rt = demo_runtime();
        assert!(rt.find("meter").is_some(), "the dot meter is laid out");
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
        assert_eq!(rt.state.ui.selected, cats::Cat::Build);
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
    fn the_junk_only_filter_narrows_the_list_without_losing_it() {
        let mut rt = demo_runtime();
        let before = rt.state.ui.entries(&rt.state.projects).len();
        rt.update(|s, _| s.ui.toggle_junk_only());
        rt.frame(0.0);
        let filtered = rt.state.ui.entries(&rt.state.projects).len();
        assert!(filtered <= before, "filtering never grows the list");
        rt.update(|s, _| s.ui.toggle_junk_only());
        rt.frame(0.0);
        assert_eq!(
            rt.state.ui.entries(&rt.state.projects).len(),
            before,
            "clearing the filter restores the list"
        );
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
        // The detail lives in its own strip, below the list.
        let (strip, _) = rt.presented("preview").expect("the preview strip is laid out");
        let last = before.last().unwrap();
        assert!(
            strip[1] >= last[1],
            "the strip sits under the rows: {strip:?} vs {last:?}"
        );
    }

    #[test]
    fn every_header_sits_over_the_column_it_names() {
        let rt = demo_runtime();
        // Find the header by its label and the row cell by the text it shows.
        let mut at = |want: &str| -> Option<[f32; 4]> {
            rt.elements()
                .into_iter()
                .find(|e| e.text.as_deref().is_some_and(|t| t.starts_with(want)))
                .map(|e| e.rect)
        };
        for header in ["folder", "size", "last commit"] {
            assert!(at(header).is_some(), "header {header} is laid out");
        }
        let h = at("size").unwrap();
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
        let (crumb, _) = rt
            .presented("crumb/1")
            .expect("the first crumb is laid out");
        let (x, y) = (crumb[0] + crumb[2] / 2.0, crumb[1] + crumb[3] / 2.0);

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
            rt.state.ui.selected,
            picked.unwrap(),
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
        // any row of the same name; the old list is never cleared mid-scan.
        let root = std::env::temp_dir().join("ps-rescan-keep");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("fresh/.git")).unwrap();
        std::fs::write(root.join("fresh/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            scan::scan(root, tx, GitCache::default(), || {});
        });
        s.rx = Some(rx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut landed = 0;
        while std::time::Instant::now() < deadline {
            if s.drain(0.0) {
                landed += 1;
            } else if s.projects.iter().any(|p| p.name == "fresh") {
                break;
            } else {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        assert!(landed > 0, "events drain");
        assert!(s.projects.iter().any(|p| p.name == "fresh"));
        assert_eq!(
            s.projects.len(),
            before + 1,
            "old rows stay, fresh row joins"
        );
    }

    #[test]
    fn rows_arrive_while_the_scan_runs() {
        // The window fills as folders are measured, not in one burst at the end.
        let mut s = State::new();
        let root = PathBuf::from("P:/Projects");
        if !root.is_dir() {
            return;
        }
        s.request_scan();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut seen: Vec<usize> = Vec::new();
        let mut last = 0;
        let mut ticks = 0;
        while std::time::Instant::now() < deadline {
            s.drain(0.0);
            if s.projects.len() != last {
                last = s.projects.len();
                seen.push(last);
                ticks += 1;
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
