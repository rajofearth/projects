//! Projects — live stats for a folder of code, built with Rune.
//! `cargo run -p projects-stats` opens the window and starts scanning;
//! `--scene demo` renders the built-in fixture headlessly.

mod charts;
mod fixture;
mod model;
mod scan;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use rune::{AppSpec, Cx, El, Waker};
use model::ScanEvent;

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
        }
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
        std::thread::spawn(move || scan::scan(root, tx, move || waker()));
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
                    self.scanning = false;
                    self.scan_secs = seconds;
                    self.last_done = Some(clock);
                    self.done_at = Some(wallclock());
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

fn view(s: &State, cx: &mut Cx<State>) -> El {
    ui::view(s, cx)
}

fn poll(s: &mut State, clock: f64, _tx: &mut rune::Tx) -> bool {
    let mut changed = s.drain(clock);
    // Live: rescan every five minutes (demo data never rescans).
    if !s.scanning && !s.demo && s.last_done.is_none_or(|t| clock - t >= RESCAN_EVERY) {
        s.request_scan();
        changed = true;
    }
    changed
}

/// "13 Nov 14:32" from the wall clock (UTC).
fn wallclock() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Howard Hinnant's civil-from-days, UTC.
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let _y = if m <= 2 { y + 1 } else { y };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{} {} {:02}:{:02}",
        MONTHS[(m - 1) as usize],
        d,
        tod / 3600,
        (tod % 3600) / 60
    )
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
        ..AppSpec::new("Projects — Live Stats", (1100.0, 780.0), State::new, view)
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
        let mut rt = Runtime::new(state, view, (1100.0, 780.0), env);
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
    fn kpis_sum_the_fixture() {
        let rt = demo_runtime();
        let bytes: u64 = rt.state.projects.iter().map(|p| p.bytes).sum();
        assert!(bytes > 65_000_000_000, "fixture totals {bytes} bytes");
        assert!(rt.state.projects.iter().any(|p| p.name == "llama.cpp"));
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
        let handle = std::thread::spawn(move || scan::scan(scan_root, tx, || {}));
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
        assert_eq!(p.files, 4);
        assert_eq!(p.nm, 4096, "node_modules bytes bucketed");
        assert!(p.loc >= 5, "hand-written lines counted: {}", p.loc);
        assert!(p.langs.iter().any(|(l, _)| l == "Rust"));
        assert!(p.langs.iter().any(|(l, _)| l == "TypeScript"));
        if git_ok {
            assert!(p.git && p.commits >= 1 && p.days.is_some());
        }
    }

    /// Run once against the real root to check the live pipeline and its timing:
    /// `cargo test -p projects-stats real_root -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_root_scan_smoke() {
        let root = PathBuf::from(std::env::var("RUNE_STATS_ROOT").unwrap_or_else(|_| r"P:\Projects".into()));
        let (tx, rx) = channel();
        let handle = std::thread::spawn(move || scan::scan(root, tx, || {}));
        let mut n = 0;
        let mut bytes = 0u64;
        let mut loc = 0u64;
        while let Ok(ev) = rx.recv() {
            match ev {
                ScanEvent::Start { total } => println!("scanning {total} folders"),
                ScanEvent::Project(p) => {
                    n += 1;
                    bytes += p.bytes;
                    loc += p.loc;
                    if n % 20 == 0 { println!("  {n} done…"); }
                }
                ScanEvent::Done { seconds } => {
                    println!("done in {seconds:.1}s: {n} folders, {:.1} GiB, {} LOC", bytes as f64 / 1073741824.0, loc);
                    break;
                }
                ScanEvent::Failed(e) => panic!("{e}"),
            }
        }
        handle.join().unwrap();
        assert!(n > 0);
    }

    #[test]
    fn live_scan_drains_events_into_state() {
        let env = rune::Env { system_dark: true, scale: 2.0 };
        let (tx, rx) = channel();
        let mut state = State::new();
        state.rx = Some(rx);
        state.scanning = true;
        state.total = 2;
        let mut rt = Runtime::new(state, view, (1100.0, 780.0), env);
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
