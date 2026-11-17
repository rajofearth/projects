//! The install window: where to put Projects, then a short progress line while it copies
//! itself, adds the shortcut and registers, then it starts the installed copy and closes.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use rune::file_dialog::FileDialogOptions;
use rune::{col, el, row, AppSpec, Cx, El, TextEdit, Tx, Waker};
use rune_kit::{Button, ButtonVariant, ControlSize, TextInput};

use super::{Existing, Step};
use crate::icons;
use crate::ui::{bar, data, label, mono, Ink, S1, S2, S3, S4, S6, S8};
use crate::update::version::Version;

pub enum Phase {
    Choosing,
    Working(Step),
    /// Installed; the new copy is starting and this window closes on the next frame.
    Done,
}

enum Msg {
    Step(Step),
    Done(Result<PathBuf, String>),
}

pub struct Setup {
    pub dir: TextEdit,
    pub phase: Phase,
    pub error: Option<String>,
    pub existing: Option<Existing>,
    rx: Option<Receiver<Msg>>,
    waker: Option<Waker>,
    /// Scenes show the window without touching the disk.
    demo: bool,
}

impl Setup {
    fn new() -> Setup {
        let existing = super::existing();
        let dir = existing.as_ref().map(|e| e.dir.clone()).unwrap_or_else(super::default_dir);
        Setup {
            dir: TextEdit::new(dir.display().to_string()),
            phase: Phase::Choosing,
            error: None,
            existing,
            rx: None,
            waker: None,
            demo: false,
        }
    }

    fn chosen(&self) -> PathBuf {
        PathBuf::from(self.dir.text.trim().trim_matches('"'))
    }

    /// The installed copy is this version or newer: opening it is the likely wish.
    fn up_to_date(&self) -> bool {
        self.existing.as_ref().and_then(|e| e.version).is_some_and(|v| v >= Version::current())
    }

    fn start(&mut self) {
        if !matches!(self.phase, Phase::Choosing) {
            return;
        }
        let target = match super::resolve_target(&self.chosen()) {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        self.error = None;
        self.phase = Phase::Working(Step::Copy);
        if self.demo {
            return;
        }
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let wake = || {
                if let Some(w) = &waker {
                    w();
                }
            };
            let result = super::install(&target, |step| {
                let _ = tx.send(Msg::Step(step));
                wake();
            });
            let _ = tx.send(Msg::Done(result));
            wake();
        });
    }
}

fn setup(s: &mut Setup, waker: Waker) {
    s.waker = Some(waker);
}

fn poll(s: &mut Setup, _clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    while let Some(rx) = &s.rx {
        match rx.try_recv() {
            Ok(Msg::Step(step)) => s.phase = Phase::Working(step),
            Ok(Msg::Done(Ok(exe))) => {
                s.rx = None;
                match super::launch(&exe) {
                    Ok(_) => {
                        s.phase = Phase::Done;
                        tx.quit();
                    }
                    Err(e) => {
                        s.phase = Phase::Choosing;
                        s.error = Some(format!("Installed, but {e}. Open Projects from the Start menu."));
                    }
                }
            }
            Ok(Msg::Done(Err(e))) => {
                s.rx = None;
                s.phase = Phase::Choosing;
                s.error = Some(e);
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                s.rx = None;
                s.phase = Phase::Choosing;
                s.error = Some("The install stopped unexpectedly".into());
            }
        }
        changed = true;
    }
    changed
}

fn browse(s: &mut Setup, tx: &mut Tx) {
    let start = s.chosen();
    let directory = start.ancestors().find(|p| p.is_dir()).map(PathBuf::from);
    tx.open_folder(
        FileDialogOptions { title: "Install Projects in".into(), directory, ..Default::default() },
        |s: &mut Setup, result, _| match result {
            Ok(Some(path)) => {
                s.dir.replace_text(&path.display().to_string());
                s.error = None;
            }
            Ok(None) => {}
            Err(e) => s.error = Some(format!("Couldn't open the folder picker ({e})")),
        },
    );
}

/// Starts this exe again as an uninstalled copy with updates off, for a quick look.
fn run_portable(_: &mut Setup, tx: &mut Tx) {
    if let Ok(exe) = std::env::current_exe() {
        let spawned = std::process::Command::new(exe).env("PROJECTS_PORTABLE", "1").env_remove("PROJECTS_SHOW_INSTALLER").spawn();
        if spawned.is_ok() {
            tx.quit();
        }
    }
}

fn open_existing(s: &mut Setup, tx: &mut Tx) {
    let Some(e) = &s.existing else { return };
    match super::launch(&e.dir.join(super::EXE_NAME)) {
        Ok(_) => tx.quit(),
        Err(err) => s.error = Some(err),
    }
}

fn view(s: &Setup, _cx: &mut Cx<Setup>) -> El {
    let ink = Ink::new();
    let k = ink.kit();
    let working = !matches!(s.phase, Phase::Choosing);
    let version = Version::current();

    let brand = row()
        .gap(S3 - 2.0)
        .items_center()
        .child(crate::shell::mark(&ink))
        .child(label("Projects", 14.0, 500.0, ink.fg))
        .child(el().grow(1.0))
        .child(data(format!("v{version}"), 11.0, ink.faint));

    let target = super::resolve_target(&s.chosen()).ok();
    let elsewhere = target.as_ref().filter(|t| **t != s.chosen()).map(|t| format!("Installs into {}", t.display()));
    let existing_line = s.existing.as_ref().map(|e| {
        let v = e.version.map(|v| format!("Version {v}")).unwrap_or_else(|| "A copy".into());
        if s.up_to_date() {
            format!("{v} is already installed in {}.", e.dir.display())
        } else {
            format!("{v} is installed in {}. Installing replaces it and keeps your settings.", e.dir.display())
        }
    });

    let location = col()
        .w_full()
        .gap(S1)
        .child(label("Location", 12.0, 400.0, ink.faint))
        .child(
            row()
                .w_full()
                .gap(S2)
                .child(
                    TextInput::new(&s.dir)
                        .text_style(mono(12.0, 400.0))
                        .invalid(s.error.is_some())
                        .disabled(working)
                        .on_edit(|s: &mut Setup| &mut s.dir)
                        .on_submit(|s: &mut Setup, _: &mut Tx| s.start())
                        .build(&k)
                        .key("dir-input")
                        .grow(1.0),
                )
                .child(
                    Button::new("Browse")
                        .variant(ButtonVariant::Neutral)
                        .icon(&icons::FOLDER)
                        .control_size(ControlSize::Large)
                        .disabled(working)
                        .on_press_tx(browse)
                        .build(&k),
                ),
        )
        .children(elsewhere.map(|t| data(t, 11.0, ink.faint).truncate()));

    let footer = match &s.phase {
        Phase::Choosing => {
            let (primary_text, secondary) = if s.up_to_date() {
                ("Open Projects", Some(Button::new("Reinstall").variant(ButtonVariant::Neutral).control_size(ControlSize::Large).on_press(Setup::start)))
            } else {
                (if s.existing.is_some() { "Update" } else { "Install" }, None)
            };
            let primary = Button::new(primary_text).variant(ButtonVariant::Submit).control_size(ControlSize::Large);
            let primary = if s.up_to_date() { primary.on_press_tx(open_existing) } else { primary.icon(&icons::DOWNLOAD).on_press(Setup::start) };
            row()
                .w_full()
                .gap(S2)
                .items_center()
                .child(
                    Button::new("Run without installing")
                        .variant(ButtonVariant::GhostMuted)
                        .help("Opens this copy once. It won't update itself.")
                        .on_press_tx(run_portable)
                        .build(&k),
                )
                .child(el().grow(1.0))
                .children(secondary.map(|b| b.build(&k)))
                .child(primary.build(&k).key("install"))
        }
        Phase::Working(step) => {
            let n = match step {
                Step::Copy => 1,
                Step::Shortcut => 2,
                Step::Register => 3,
            };
            col()
                .w_full()
                .gap(S2)
                .child(
                    row()
                        .w_full()
                        .child(label(step.label(), 13.0, 400.0, ink.muted).grow(1.0))
                        .child(data(format!("{n} of 3"), 11.0, ink.faint)),
                )
                .child(bar(n as f32 / 3.0, ink.accent, &ink))
        }
        Phase::Done => col().w_full().gap(S2).child(label("Starting Projects", 13.0, 400.0, ink.muted)).child(bar(1.0, ink.accent, &ink)),
    };

    let body = col()
        .key("setup")
        .w_full()
        .h_full()
        .fill(ink.bg)
        .px(S8)
        .pt(S6)
        .pb(S6)
        .child(brand)
        .child(el().h(S8))
        .child(label("Install Projects", 20.0, 500.0, ink.fg))
        .child(el().h(S2))
        .child(
            label(
                "Projects lists the repositories in your code folder with their git state and disk use, and totals what your coding agents spend.",
                13.0,
                400.0,
                ink.muted,
            )
            .wrap()
            .w_full(),
        )
        .child(el().h(S6))
        .child(location)
        .child(el().h(S3))
        .child(
            label(
                "For your account only, so Windows won't ask for admin rights. Projects adds itself to the Start menu and installs updates from GitHub.",
                12.0,
                400.0,
                ink.faint,
            )
            .wrap()
            .w_full(),
        )
        .children(existing_line.map(|l| col().pt(S3).w_full().child(label(l, 12.0, 400.0, ink.muted).wrap().w_full())))
        .children(s.error.clone().map(|e| col().pt(S3).w_full().child(label(e, 12.0, 400.0, ink.red).wrap().w_full())))
        .child(el().grow(1.0).min_h(S4))
        .child(footer);
    rune_kit::root(body, Vec::new())
}

fn scene_setup(s: &mut Setup) {
    s.demo = true;
    s.existing = None;
    s.dir.replace_text(r"C:\Users\you\AppData\Local\Programs\Projects");
}

fn scene_working(s: &mut Setup) {
    scene_setup(s);
    s.phase = Phase::Working(Step::Shortcut);
}

fn scene_existing(s: &mut Setup) {
    scene_setup(s);
    s.existing = Some(Existing { dir: PathBuf::from(r"C:\Users\you\AppData\Local\Programs\Projects"), version: Version::parse("0.0.9") });
}

fn scene_error(s: &mut Setup) {
    scene_setup(s);
    s.dir.replace_text(r"C:\Program Files\Projects");
    s.error = Some("That folder needs admin rights. Choose one in your user folder.".into());
}

pub fn run() {
    rune::run(AppSpec::<Setup> {
        scenes: &[
            ("setup", scene_setup),
            ("setup-working", scene_working),
            ("setup-existing", scene_existing),
            ("setup-error", scene_error),
        ],
        setup: Some(setup),
        poll: Some(poll),
        families: ("Inter", "ui-monospace"),
        min_size: Some((520.0, 420.0)),
        ..AppSpec::new("Install Projects", (600.0, 440.0), Setup::new, view)
    });
}
