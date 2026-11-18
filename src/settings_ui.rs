//! Settings: the projects folders, the editor Enter opens, how Home behaves, and what the app
//! remembers. Every change is saved the moment it is made.

use std::path::PathBuf;

use rune::{col, el, row, Color, Cursor, El, ScrollSpec, TextEdit, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, SegmentedControl, Switch, TextInput};

use crate::icons;
use crate::saved::Layout;
use crate::shell;
use crate::ui::{data, label, tag, Ink, R_CARD, R_CTRL, S1, S2, S3, S4, S5, S6};
use crate::State;

pub struct SettingsPage {
    pub new_root: TextEdit,
    pub problem: Option<String>,
}

impl SettingsPage {
    pub fn new() -> Self {
        SettingsPage { new_root: TextEdit::new(""), problem: None }
    }
}

fn add_root(s: &mut State) {
    let raw = s.settings.new_root.text.trim().trim_matches('"').to_string();
    if raw.is_empty() {
        return;
    }
    if !PathBuf::from(&raw).is_dir() {
        s.settings.problem = Some(format!("{raw} isn't a folder"));
        return;
    }
    s.settings.problem = None;
    if s.saved.add_root(&raw) {
        s.toast(format!("Added {raw}"));
    }
    s.settings.new_root.replace_text("");
}

/// One settings group: what it is on the left, its controls on the right.
fn group(key: &str, title: &str, about: &str, body: El, ink: &Ink) -> El {
    row()
        .key(format!("set/{key}"))
        .w_full()
        .gap(S8_)
        .py(S5)
        .border_sides([0.0, 0.0, 1.0, 0.0], ink.line)
        .child(
            col()
                .w(240.0)
                .shrink()
                .gap(S1 + 2.0)
                .child(label(title, 13.0, 500.0, ink.fg))
                .child(label(about, 12.0, 400.0, ink.faint).wrap().w_full()),
        )
        .child(col().grow(1.0).min_w(0.0).gap(S2).child(body))
}

const S8_: f32 = 32.0;

fn radio(on: bool, ink: &Ink) -> El {
    el().size(14.0, 14.0)
        .radius(7.0)
        .border(1.5, if on { ink.accent } else { ink.line_strong })
        .items_center()
        .justify_center()
        .child(el().size(6.0, 6.0).radius(3.0).fill(if on { ink.accent } else { Color::TRANSPARENT }))
}

fn roots(s: &State, ink: &Ink, k: &Kit) -> El {
    let rows = s.saved.roots.iter().map(|r| {
        let shown = r.eq_ignore_ascii_case(&s.root);
        let (a, b) = (r.clone(), r.clone());
        row()
            .key(format!("root/{r}"))
            .w_full()
            .h(40.0)
            .px(S3)
            .gap(S3)
            .radius(R_CTRL)
            .items_center()
            .fill(if shown { ink.raised } else { Color::TRANSPARENT })
            .hover(ink.hover)
            .cursor(if shown { Cursor::Default } else { Cursor::Pointer })
            .on_press(move |s: &mut State| s.switch_root(&a))
            .child(radio(shown, ink))
            .child(data(r.clone(), 12.0, if shown { ink.fg } else { ink.muted }).grow(1.0).min_w(0.0).truncate())
            .children(shown.then(|| data(format!("{} projects", s.projects.len()), 11.0, ink.faint)))
            .children((!shown && s.saved.roots.len() > 1).then(|| {
                IconButton::new(&icons::CLOSE)
                    .variant(IconButtonVariant::Ghost)
                    .control_size(ControlSize::Small)
                    .label("Remove")
                    .help("Remove from the list (the folder stays on disk)")
                    .on_press(move |s: &mut State| s.saved.remove_root(&b))
                    .build(k)
            }))
    });
    col()
        .w_full()
        .gap(S2)
        .children(rows)
        .child(
            row()
                .w_full()
                .gap(S2)
                .pt(S1)
                .child(
                    TextInput::new(&s.settings.new_root)
                        .placeholder("Another folder, e.g. D:\\Work")
                        .text_style(crate::ui::mono(12.0, 400.0))
                        .invalid(s.settings.problem.is_some())
                        .on_edit(|s: &mut State| &mut s.settings.new_root)
                        .on_submit(|s: &mut State, _: &mut Tx| add_root(s))
                        .build(k)
                        .key("new-root")
                        .grow(1.0),
                )
                .child(
                    Button::new("Add folder")
                        .variant(ButtonVariant::Neutral)
                        .icon(&icons::PLUS)
                        .control_size(ControlSize::Large)
                        .on_press(add_root)
                        .build(k),
                ),
        )
        .children(s.settings.problem.clone().map(|p| label(p, 12.0, 400.0, ink.red)))
}

fn editors(s: &State, ink: &Ink) -> El {
    if s.apps.is_empty() {
        return label(
            "No editors found. VS Code, Cursor, Zed, Windsurf, T3 Code and the JetBrains IDEs are looked for on PATH and in their usual install folders.",
            12.0,
            400.0,
            ink.muted,
        )
        .wrap()
        .w_full();
    }
    let current = crate::actions::default_app(s).map(|a| a.id);
    col().w_full().gap(S1).children(s.apps.iter().map(|a| {
        let on = current == Some(a.id);
        let id = a.id.to_string();
        row()
            .key(format!("editor/{}", a.id))
            .w_full()
            .h(36.0)
            .px(S3)
            .gap(S3)
            .radius(R_CTRL)
            .items_center()
            .cursor(Cursor::Pointer)
            .fill(if on { ink.raised } else { Color::TRANSPARENT })
            .hover(ink.hover)
            .on_press(move |s: &mut State| {
                s.saved.set_editor(Some(id.clone()));
            })
            .child(radio(on, ink))
            .child(label(a.name, 13.0, if on { 500.0 } else { 400.0 }, if on { ink.fg } else { ink.row }).grow(1.0))
            .children(on.then(|| data("Enter, double-click", 11.0, ink.faint)))
    }))
}

fn home_prefs(s: &State, ink: &Ink, k: &Kit) -> El {
    col()
        .w_full()
        .gap(S4)
        .child(
            row()
                .w_full()
                .items_center()
                .gap(S3)
                .child(label("Layout", 13.0, 400.0, ink.row).grow(1.0))
                .child(
                    SegmentedControl::new(&["Cards", "List"], Some(if s.saved.layout == Layout::Grid { 0 } else { 1 }))
                        .label("Layout")
                        .width(Some(160.0))
                        .on_pick(|s: &mut State, i, _| s.saved.set_layout(if i == 0 { Layout::Grid } else { Layout::List }))
                        .build(k),
                ),
        )
        .child(
            row()
                .w_full()
                .items_center()
                .gap(S3)
                .child(
                    col()
                        .grow(1.0)
                        .gap(2.0)
                        .child(label("Rescan every 5 minutes", 13.0, 400.0, ink.row))
                        .child(label("Sizes and git status stay current while the window is open.", 12.0, 400.0, ink.faint)),
                )
                .child(
                    Switch::new(s.saved.auto_rescan)
                        .label("Rescan every 5 minutes")
                        .hide_label()
                        .on_toggle(|s: &mut State| {
                            let on = !s.saved.auto_rescan;
                            s.saved.set_auto_rescan(on);
                        })
                        .build(k)
                        .key("auto-rescan"),
                ),
        )
}

fn memory(s: &State, ink: &Ink, k: &Kit) -> El {
    let archived: Vec<String> = s.saved.archived.iter().cloned().collect();
    col()
        .w_full()
        .gap(S3)
        .child(
            row()
                .w_full()
                .items_center()
                .gap(S3)
                .child(
                    label(
                        match s.saved.opened.len() {
                            0 => "Nothing opened from here yet".to_string(),
                            n => format!("{} recently opened", crate::fmt::plural(n, "project", "projects")),
                        },
                        13.0,
                        400.0,
                        ink.row,
                    )
                    .grow(1.0),
                )
                .child(
                    Button::new("Forget")
                        .variant(ButtonVariant::Neutral)
                        .disabled(s.saved.opened.is_empty())
                        .on_press(|s: &mut State| {
                            s.saved.clear_opened();
                            s.toast("Cleared recently opened".to_string());
                        })
                        .build(k),
                ),
        )
        .child(label(
            if archived.is_empty() { "No archived projects".to_string() } else { "Archived, hidden from Home".to_string() },
            13.0,
            400.0,
            ink.row,
        ))
        .children(archived.into_iter().map(|name| {
            let n = name.clone();
            row()
                .w_full()
                .h(32.0)
                .px(S3)
                .gap(S3)
                .radius(R_CTRL)
                .items_center()
                .border(1.0, ink.line)
                .child(data(name, 12.0, ink.muted).grow(1.0).truncate())
                .child(
                    Button::new("Restore")
                        .variant(ButtonVariant::GhostMuted)
                        .control_size(ControlSize::Small)
                        .on_press(move |s: &mut State| crate::actions::toggle_archived(s, &n))
                        .build(k),
                )
        }))
}

fn files(s: &State, ink: &Ink, k: &Kit) -> El {
    let line = |what: &str, path: Option<PathBuf>| -> El {
        let p = path.clone();
        row()
            .w_full()
            .h(32.0)
            .gap(S3)
            .items_center()
            .child(label(what, 13.0, 400.0, ink.row).w(96.0))
            .child(
                data(path.map(|p| p.display().to_string()).unwrap_or_else(|| "in memory".into()), 11.0, ink.faint)
                    .grow(1.0)
                    .min_w(0.0)
                    .truncate(),
            )
            .children(p.map(|p| {
                IconButton::new(&icons::FOLDER)
                    .variant(IconButtonVariant::Ghost)
                    .label("Reveal")
                    .help("Reveal in Explorer")
                    .on_press(move |s: &mut State| match crate::browse::reveal(&p) {
                        Ok(()) => {}
                        Err(e) => s.toast_error(format!("Couldn't reveal: {e}")),
                    })
                    .build(k)
            }))
    };
    col()
        .w_full()
        .gap(S1)
        .child(line("Settings", s.saved.path()))
        .child(line("Templates", Some(crate::templates::path())))
}

pub fn view(s: &State, ink: &Ink, k: &Kit, _layer: &mut Vec<El>) -> El {
    let header = shell::header("Settings", Some("Saved as you change them".into()), vec![], ink);
    let body = col()
        .key("settings-body")
        .w_full()
        .max_w(980.0)
        .px(S6)
        .pb(S6)
        .child(group(
            "roots",
            "Projects folders",
            "Each folder's direct subfolders are your projects. Home and Storage show one folder at a time.",
            roots(s, ink, k),
            ink,
        ))
        .child(group(
            "editor",
            "Open projects in",
            "What Enter, double-click and the Open button use. Every editor stays in the right-click menu.",
            editors(s, ink),
            ink,
        ))
        .child(group("home", "Home", "How the project grid looks and stays up to date.", home_prefs(s, ink, k), ink))
        .child(group("memory", "Remembered", "Recently opened projects lead Home; archived ones leave it.", memory(s, ink, k), ink))
        .child(group(
            "updates",
            "Updates",
            "New versions come from GitHub releases and are checked against the release signature before they install.",
            crate::update_ui::settings(s, ink, k),
            ink,
        ))
        .child(group("files", "Files", "Plain JSON, safe to edit or back up.", files(s, ink, k), ink));
    col()
        .key("settings")
        .w_full()
        .h_full()
        .child(header)
        .child(
            col()
                .key("settings-scroll")
                .grow(1.0)
                .min_h(0.0)
                .w_full()
                .scroll(ScrollSpec { stick_bottom: false })
                .scroll_edge_fade(16.0)
                .child(body),
        )
}

#[allow(dead_code)]
fn unused(ink: &Ink) -> (El, f32) {
    (tag("", ink), R_CARD)
}
