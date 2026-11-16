//! The new project sheet: a name, a folder, a template and git, then the template's commands
//! run with their output streaming in, and the new project selected when they finish.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use rune::{col, el, icon, row, Color, Cursor, El, ScrollSpec, TextEdit, Tx, Waker};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, Select, Spinner, Switch, TextInput};

use crate::icons;
use crate::shell::{self, Page};
use crate::templates::{self, Event, Template};
use crate::ui::{data, label, tag, Ink, R_CTRL, S1, S2, S3, S4, S5};
use crate::State;

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Edit,
    Running,
    Done(PathBuf),
    Failed(String),
}

pub struct NewProject {
    pub open: bool,
    pub name: TextEdit,
    pub parent: String,
    pub template: usize,
    pub git: bool,
    pub phase: Phase,
    /// The template as it was when the run started, and the step running now.
    pub running: Option<Template>,
    pub step: Option<usize>,
    pub log: Vec<String>,
    pub problem: Option<String>,
    pub picking_parent: bool,
    rx: Option<Receiver<Event>>,
}

const LOG_LINES: usize = 600;

impl NewProject {
    pub fn new() -> Self {
        NewProject {
            open: false,
            name: TextEdit::new(""),
            parent: String::new(),
            template: 0,
            git: true,
            phase: Phase::Edit,
            running: None,
            step: None,
            log: Vec::new(),
            problem: None,
            picking_parent: false,
            rx: None,
        }
    }

    /// Opens the sheet fresh, unless a run is still going (then it just shows it again).
    pub fn open(&mut self, root: &str) {
        self.open = true;
        if self.phase == Phase::Running {
            return;
        }
        self.phase = Phase::Edit;
        self.name = TextEdit::new("");
        self.name.focused = true;
        self.parent = root.to_string();
        self.problem = None;
        self.log.clear();
        self.step = None;
        self.running = None;
    }

    pub fn running(&self) -> bool {
        self.phase == Phase::Running
    }

    fn push(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > LOG_LINES {
            let cut = self.log.len() - LOG_LINES;
            self.log.drain(..cut);
        }
    }
}

/// Starts the template's commands on a worker; output comes back through `drain`.
pub fn create(s: &mut State) {
    let name = s.newp.name.text.trim().to_string();
    if name.is_empty() {
        s.newp.problem = Some("Give the project a name".into());
        return;
    }
    let parent = PathBuf::from(&s.newp.parent);
    if parent.join(&name).exists() {
        s.newp.problem = Some(format!("{} already exists", parent.join(&name).display()));
        return;
    }
    let Some(t) = s.tmpl.list.get(s.newp.template).cloned() else {
        s.newp.problem = Some("Pick a template".into());
        return;
    };
    s.newp.problem = None;
    s.newp.log.clear();
    s.newp.step = None;
    s.newp.phase = Phase::Running;
    s.newp.running = Some(t.clone());
    let (tx, rx) = channel();
    s.newp.rx = Some(rx);
    let waker: Waker = s.waker.clone().unwrap_or_else(|| Arc::new(|| ()));
    let (inner_tx, inner_rx) = channel();
    templates::run(&t, &parent, &name, s.newp.git, inner_tx);
    // Forward each event and wake the window, since the template runner has no waker.
    std::thread::spawn(move || {
        for ev in inner_rx {
            if tx.send(ev).is_err() {
                break;
            }
            waker();
        }
        waker();
    });
}

/// Output and progress that arrived since the last frame. True when anything did.
pub fn drain(s: &mut State) -> bool {
    let Some(rx) = s.newp.rx.take() else { return false };
    let mut changed = false;
    let mut finished: Option<std::io::Result<PathBuf>> = None;
    loop {
        match rx.try_recv() {
            Ok(Event::Line(l)) => {
                s.newp.push(l);
                changed = true;
            }
            Ok(Event::StepStarted(i)) => {
                s.newp.step = Some(i);
                let cmd = s.newp.running.as_ref().and_then(|t| t.steps.get(i)).map(|st| st.run.clone()).unwrap_or_default();
                s.newp.push(format!("$ {cmd}"));
                changed = true;
            }
            Ok(Event::Done(r)) => {
                finished = Some(r);
                break;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                finished = Some(Err(std::io::Error::other("the template run stopped unexpectedly")));
                break;
            }
        }
    }
    if finished.is_none() {
        s.newp.rx = Some(rx);
    }
    if let Some(r) = finished {
        changed = true;
        match r {
            Ok(path) => {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                s.newp.phase = Phase::Done(path.clone());
                s.newp.step = None;
                // If it landed in the folder on screen, find it there.
                if PathBuf::from(&s.root) == path.parent().map(|p| p.to_path_buf()).unwrap_or_default()
                    || s.newp.parent.eq_ignore_ascii_case(&s.root)
                {
                    s.home.selected = Some(name.clone());
                    s.saved.mark_opened(&name);
                    s.rescan();
                }
                s.toast(format!("Created {name}"));
            }
            Err(e) => {
                s.newp.phase = Phase::Failed(e.to_string());
                s.toast_error(format!("Couldn't create the project: {e}"));
            }
        }
    }
    changed
}

fn close(s: &mut State) {
    s.newp.open = false;
    s.newp.picking_parent = false;
    if let Phase::Done(_) | Phase::Failed(_) = s.newp.phase {
        s.newp.phase = Phase::Edit;
    }
}

fn template_row(s: &State, ink: &Ink, i: usize, t: &Template) -> El {
    let on = s.newp.template == i;
    let first = t.steps.first().map(|st| st.run.clone()).unwrap_or_else(|| "Just the folder".into());
    let more = t.steps.len().saturating_sub(1);
    row()
        .key(format!("tpl/{}", t.id))
        .w_full()
        .h(48.0)
        .px(S3)
        .gap(S3)
        .radius(R_CTRL)
        .items_center()
        .cursor(Cursor::Pointer)
        .fill(if on { ink.raised } else { Color::TRANSPARENT })
        .border(1.0, if on { ink.accent.alpha(0.55) } else { ink.line })
        .hover(ink.hover)
        .on_press(move |s: &mut State| s.newp.template = i)
        .on_double_press(create)
        .child(
            el().size(14.0, 14.0).radius(7.0).border(1.5, if on { ink.accent } else { ink.line_strong }).items_center().justify_center().child(
                el().size(6.0, 6.0).radius(3.0).fill(if on { ink.accent } else { Color::TRANSPARENT }),
            ),
        )
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(2.0)
                .child(label(t.name.clone(), 13.0, 500.0, ink.fg))
                .child(
                    data(
                        if more > 0 { format!("{first}  +{more} more") } else { first },
                        11.0,
                        ink.faint,
                    )
                    .truncate(),
                ),
        )
        .child(tag(t.kind.clone(), ink))
}

fn edit_body(s: &State, ink: &Ink, k: &Kit, layer: &mut Vec<El>) -> El {
    let n = &s.newp;
    let roots: Vec<String> = s.saved.roots.clone();
    let preview = PathBuf::from(&n.parent).join(if n.name.text.trim().is_empty() { "name" } else { n.name.text.trim() });
    col()
        .key("new-edit")
        .w_full()
        .px(S5)
        .pb(S5)
        .gap(S4)
        .child(
            row()
                .w_full()
                .gap(S3)
                .items_start()
                .child(
                    col().grow(1.0).min_w(0.0).gap(6.0).child(label("Name", 12.0, 400.0, ink.muted)).child(
                        TextInput::new(&n.name)
                            .placeholder("my-new-thing")
                            .large()
                            .invalid(n.problem.is_some())
                            .on_edit(|s: &mut State| &mut s.newp.name)
                            .on_submit(|s: &mut State, _: &mut Tx| create(s))
                            .build(k)
                            .w_full()
                            .key("new-name"),
                    ),
                )
                .child(
                    col().w(240.0).gap(6.0).child(label("In", 12.0, 400.0, ink.muted)).child(
                        Select::new("new-parent", &roots)
                            .current(Some(&n.parent))
                            .width(240.0)
                            .label("Folder")
                            .appearance(rune_kit::SelectAppearance::Field)
                            .open(n.picking_parent)
                            .on_toggle(|s: &mut State| s.newp.picking_parent = !s.newp.picking_parent)
                            .on_close(|s: &mut State| s.newp.picking_parent = false)
                            .on_choose(|s: &mut State, v: &str, _| {
                                s.newp.parent = v.to_string();
                                s.newp.picking_parent = false;
                            })
                            .build(k, layer),
                    ),
                ),
        )
        .child(
            col()
                .w_full()
                .gap(6.0)
                .child(
                    row()
                        .w_full()
                        .items_center()
                        .child(label("Template", 12.0, 400.0, ink.muted).grow(1.0))
                        .child(
                            row()
                                .key("edit-templates")
                                .cursor(Cursor::Pointer)
                                .on_press(|s: &mut State| {
                                    s.newp.open = false;
                                    s.go(Page::Templates);
                                })
                                .child(label("Edit templates", 12.0, 400.0, ink.accent)),
                        ),
                )
                .child(
                    col()
                        .key("tpl-scroll")
                        .w_full()
                        .max_h(260.0)
                        .scroll(ScrollSpec { stick_bottom: false })
                        .child(
                            col()
                                .w_full()
                                .gap(6.0)
                                .children(s.tmpl.list.iter().enumerate().map(|(i, t)| template_row(s, ink, i, t))),
                        ),
                ),
        )
        .child(
            row()
                .w_full()
                .items_center()
                .gap(S3)
                .child(
                    Switch::new(n.git)
                        .label("Initialise git")
                        .on_toggle(|s: &mut State| s.newp.git = !s.newp.git)
                        .build(k)
                        .key("new-git"),
                )
                .child(el().grow(1.0))
                .child(data(crate::ui::clip_path(&preview.display().to_string(), 44), 11.0, ink.faint)),
        )
        .children(n.problem.clone().map(|p| label(p, 12.0, 400.0, ink.red)))
        .child(
            row()
                .w_full()
                .gap(S2)
                .pt(S1)
                .items_center()
                .child(el().grow(1.0))
                .child(Button::new("Cancel").variant(ButtonVariant::Ghost).control_size(ControlSize::Large).on_press(close).build(k))
                .child(shell::primary("Create", &icons::FOLDER_PLUS).on_press(create).build(k).key("new-create")),
        )
}

fn run_body(s: &State, ink: &Ink, k: &Kit) -> El {
    let n = &s.newp;
    let steps = n.running.as_ref().map(|t| t.steps.clone()).unwrap_or_default();
    let done_all = matches!(n.phase, Phase::Done(_));
    let failed = matches!(n.phase, Phase::Failed(_));
    let step_rows = steps.iter().enumerate().map(|(i, st)| {
        let state = match n.step {
            _ if done_all => 2,
            Some(cur) if i < cur => 2,
            Some(cur) if i == cur => {
                if failed {
                    3
                } else {
                    1
                }
            }
            _ => 0,
        };
        let mark: El = match state {
            1 => Spinner::new().build(k),
            2 => icon(&icons::CHECK, 14.0).color(ink.green),
            3 => icon(&icons::ALERT, 14.0).color(ink.red),
            _ => el().size(6.0, 6.0).radius(3.0).fill(ink.line_strong),
        };
        row()
            .w_full()
            .h(28.0)
            .gap(S3)
            .items_center()
            .child(row().size(16.0, 16.0).items_center().justify_center().child(mark))
            .child(data(st.run.clone(), 12.0, if state == 0 { ink.faint } else { ink.fg }).grow(1.0).min_w(0.0).truncate())
            .child(data(if st.in_parent { "in parent" } else { "in project" }, 11.0, ink.faint))
    });
    let log = col()
        .key("new-log")
        .w_full()
        .h(240.0)
        .radius(R_CTRL)
        .fill(ink.side)
        .border(1.0, ink.line)
        .scroll(ScrollSpec { stick_bottom: true })
        .child(
            col()
                .w_full()
                .padding(S3)
                .gap(2.0)
                .children(n.log.iter().enumerate().map(|(i, l)| {
                    let cmd = l.starts_with("$ ");
                    data(l.clone(), 11.0, if cmd { ink.accent } else { ink.muted }).key(format!("log{i}")).wrap_anywhere()
                }))
                .children(n.log.is_empty().then(|| data("Waiting for output", 11.0, ink.faint))),
        );
    let footer: El = match &n.phase {
        Phase::Running => row()
            .w_full()
            .items_center()
            .gap(S2)
            .child(label(
                match n.step {
                    Some(i) => format!("Step {} of {}", i + 1, steps.len().max(1)),
                    None => "Creating the folder".into(),
                },
                12.0,
                400.0,
                ink.muted,
            ))
            .child(el().grow(1.0))
            .child(Button::new("Hide").variant(ButtonVariant::Ghost).control_size(ControlSize::Large).on_press(close).build(k)),
        Phase::Done(path) => {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let editor = crate::actions::default_app(s).map(|a| (a.id, a.name));
            row()
                .w_full()
                .items_center()
                .gap(S2)
                .child(data(crate::ui::clip_path(&path.display().to_string(), 40), 12.0, ink.green).grow(1.0).min_w(0.0).truncate())
                .child(Button::new("Done").variant(ButtonVariant::Ghost).control_size(ControlSize::Large).on_press(close).build(k))
                .children(editor.map(|(id, app)| {
                    let path = path.clone();
                    shell::primary(&format!("Open in {app}"), &icons::CODE)
                        .on_press(move |s: &mut State| {
                            s.saved.mark_opened(&name);
                            match crate::launch::open_in(id, &path) {
                                Ok(()) => s.toast(format!("Opened {name} in {app}")),
                                Err(e) => s.toast_error(format!("Couldn't open {name}: {e}")),
                            }
                            close(s);
                        })
                        .build(k)
                }))
        }
        Phase::Failed(e) => row()
            .w_full()
            .items_center()
            .gap(S2)
            .child(label(e.clone(), 12.0, 400.0, ink.red).grow(1.0).min_w(0.0).wrap())
            .child(
                Button::new("Back")
                    .variant(ButtonVariant::Neutral)
                    .control_size(ControlSize::Large)
                    .on_press(|s: &mut State| s.newp.phase = Phase::Edit)
                    .build(k),
            ),
        Phase::Edit => row(),
    };
    col()
        .key("new-run")
        .w_full()
        .px(S5)
        .pb(S5)
        .gap(S3)
        .child(col().w_full().gap(2.0).children(step_rows).children(steps.is_empty().then(|| {
            row().h(28.0).items_center().child(data("Make the folder", 12.0, ink.fg))
        })))
        .child(log)
        .child(footer)
}

pub fn dialog(s: &State, ink: &Ink, k: &Kit, layer: &mut Vec<El>) {
    if !s.newp.open {
        return;
    }
    let mut inner = Vec::new();
    let (title, body) = match s.newp.phase {
        Phase::Edit => ("New project".to_string(), edit_body(s, ink, k, &mut inner)),
        _ => {
            let name = s.newp.name.text.trim().to_string();
            (format!("Creating {name}"), run_body(s, ink, k))
        }
    };
    shell::sheet("new", &title, 640.0, body, close, ink, k, layer);
    layer.extend(inner);
}

#[allow(dead_code)]
fn spacing() -> f32 {
    S1
}
