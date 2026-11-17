//! The details panel beside the grid: what a project is, where git stands, what you can run,
//! and where its space goes. Facts come from `detect::inspect`, read on the inspector's thread.

use rune::{col, el, icon, row, text, Align, Color, Cursor, El, ScrollSpec, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Spinner};

use crate::actions;
use crate::art;
use crate::cats::{self, Cat};
use crate::fmt;
use crate::icons;
use crate::model::{self, Project};
use crate::saved::now_unix;
use crate::ui::{color_of, data, label, rgb, tag, Ink, R_CTRL, S1, S2, S3, S4, S5};
use crate::State;

pub const PANEL_W: f32 = 368.0;

/// A colour per language, muted to sit on the dark ramp (GitHub's hues, toned down).
fn lang_color(name: &str) -> Color {
    let v: [u8; 3] = match name {
        "TypeScript" => [86, 140, 206],
        "JavaScript" => [214, 196, 92],
        "Rust" => [206, 132, 96],
        "Python" => [92, 132, 186],
        "Go" => [96, 182, 206],
        "CSS" | "SCSS" => [150, 112, 190],
        "HTML" => [206, 110, 84],
        "C" | "C++" => [140, 150, 170],
        "C#" => [110, 160, 100],
        "Java" => [180, 140, 90],
        "Swift" => [220, 120, 90],
        "Kotlin" => [160, 120, 220],
        "Dart" => [90, 170, 190],
        "Shell" => [120, 180, 110],
        "Svelte" => [220, 110, 80],
        "Vue" => [100, 180, 140],
        _ => [130, 130, 140],
    };
    rgb(v)
}

fn heading(text_: &str, ink: &Ink) -> El {
    label(text_, 12.0, 500.0, ink.faint).pb(S1)
}

fn block(ink: &Ink) -> El {
    col().w_full().gap(S2).pt(S4).pb(S4).border_sides([1.0, 0.0, 0.0, 0.0], ink.line)
}

fn languages(ink: &Ink, langs: &[(&'static str, f32)]) -> El {
    let shown: Vec<(&str, f32)> = langs.iter().filter(|(_, f)| *f >= 0.01).map(|(n, f)| (*n, *f)).collect();
    let sum: f32 = shown.iter().map(|(_, f)| f).sum::<f32>().max(0.0001);
    col()
        .key("languages")
        .w_full()
        .gap(S3)
        .child(
            row()
                .w_full()
                .h(6.0)
                .radius(3.0)
                .gap(2.0)
                .children(shown.iter().map(|(n, f)| el().h_full().w_frac(f / sum).min_w(3.0).radius(3.0).fill(lang_color(n)))),
        )
        .child(row().w_full().flex_wrap().gap(S3).children(shown.iter().map(|(n, f)| {
            row()
                .gap(6.0)
                .items_center()
                .child(el().size(6.0, 6.0).radius(3.0).fill(lang_color(n)))
                .child(label(*n, 12.0, 400.0, ink.muted))
                .child(data(format!("{:.0}%", f * 100.0), 11.0, ink.faint))
        })))
}

fn fact(name: &str, value: El, ink: &Ink) -> El {
    row()
        .w_full()
        .h(24.0)
        .items_center()
        .child(label(name, 12.0, 400.0, ink.faint).w(72.0))
        .child(value.grow(1.0).min_w(0.0))
}

fn git_block(s: &State, ink: &Ink, k: &Kit, p: &Project) -> El {
    let info = s.inspect.get(&p.name);
    let branch = s.home.branches.get(&p.name).cloned().unwrap_or_default();
    let mut b = block(ink).key("git").child(heading("Git", ink));
    if !p.git {
        return b.child(label("Not a git repository", 13.0, 400.0, ink.muted));
    }
    b = b.child(fact(
        "Branch",
        row()
            .gap(S2)
            .items_center()
            .child(icon(&icons::BRANCH, 13.0).color(ink.muted))
            .child(data(if branch.is_empty() { "detached".into() } else { branch }, 12.0, ink.fg).truncate()),
        ink,
    ));
    if let Some(info) = info {
        let mut status = row().gap(S3).items_center();
        status = if info.dirty > 0 {
            status
                .child(el().size(6.0, 6.0).radius(3.0).fill(ink.accent))
                .child(data(format!("{} changed", info.dirty), 12.0, ink.accent))
        } else {
            status.child(data("clean", 12.0, ink.muted))
        };
        if info.ahead > 0 {
            status = status.child(data(format!("↑{} ahead", info.ahead), 12.0, ink.muted));
        }
        if info.behind > 0 {
            status = status.child(data(format!("↓{} behind", info.behind), 12.0, ink.muted));
        }
        b = b.child(fact("Status", status, ink));
        if let Some(remote) = info.remote.clone() {
            let shown = remote.trim_start_matches("https://").to_string();
            let url = remote.clone();
            b = b.child(fact(
                "Remote",
                row()
                    .key("remote")
                    .gap(S2)
                    .items_center()
                    .cursor(Cursor::Pointer)
                    .on_press(move |s: &mut State| actions::open_url(s, &url))
                    .help(format!("Open {remote}"))
                    .child(data(shown, 12.0, ink.fg).truncate().shrink())
                    .child(icon(&icons::EXTERNAL, 12.0).color(ink.faint)),
                ink,
            ));
        }
        if !info.commits.is_empty() {
            let now = now_unix();
            b = b.child(label("Recent commits", 12.0, 400.0, ink.faint).pt(S2)).child(
                col().w_full().gap(2.0).children(info.commits.iter().take(5).map(|c| {
                    row()
                        .w_full()
                        .h(26.0)
                        .gap(S3)
                        .items_center()
                        .child(data(c.hash.chars().take(7).collect::<String>(), 11.0, ink.faint).w(52.0))
                        .child(label(c.subject.clone(), 12.0, 400.0, ink.row).grow(1.0).min_w(0.0).truncate())
                        .child(data(fmt::secs_ago(now - c.unix), 11.0, ink.faint))
                        .help(format!("{} · {}", c.subject, c.author))
                })),
            );
        }
    } else {
        b = b.child(row().gap(S2).items_center().child(Spinner::new().build(k)).child(label("Reading git", 12.0, 400.0, ink.faint)));
    }
    b
}

fn scripts_block(s: &State, ink: &Ink, k: &Kit, p: &Project) -> Option<El> {
    let info = s.inspect.get(&p.name)?;
    if info.scripts.is_empty() {
        return None;
    }
    let pm = info.package_manager;
    Some(
        block(ink)
            .key("scripts")
            .child(
                row()
                    .w_full()
                    .items_center()
                    .child(heading("Scripts", ink).grow(1.0))
                    .children(pm.map(|pm| data(pm, 11.0, ink.faint))),
            )
            .child(col().w_full().gap(2.0).children(info.scripts.iter().take(10).map(|(name, command)| {
                let (n, sc) = (p.name.clone(), name.clone());
                row()
                    .key(format!("script/{name}"))
                    .w_full()
                    .h(32.0)
                    .pl(S2)
                    .pr(S1)
                    .gap(S3)
                    .radius(R_CTRL)
                    .items_center()
                    .hover(ink.hover)
                    .child(data(name.clone(), 12.0, ink.fg).w(84.0).truncate())
                    .child(data(command.clone(), 11.0, ink.faint).grow(1.0).min_w(0.0).truncate())
                    .child(
                        IconButton::new(&icons::PLAY)
                            .variant(IconButtonVariant::Ghost)
                            .control_size(ControlSize::Small)
                            .label(format!("Run {name}"))
                            .help(format!("Run {} in a terminal", actions::script_line(pm, name, command)))
                            .on_press(move |s: &mut State| actions::run_script(s, &n, &sc))
                            .build(k),
                    )
            }))),
    )
}

fn storage_block(ink: &Ink, k: &Kit, p: &Project) -> El {
    let parts = [Cat::Source, Cat::Deps, Cat::Build, Cat::Other];
    let total = p.bytes.max(1);
    let name = p.name.clone();
    block(ink)
        .key("space")
        .child(
            row()
                .w_full()
                .items_center()
                .child(heading("Space", ink).grow(1.0))
                .child(data(model::gb(p.bytes), 12.0, ink.fg)),
        )
        .child(
            row().w_full().h(6.0).gap(2.0).children(parts.iter().filter(|c| c.bytes(p) > 0).map(|c| {
                el().h_full().w_frac(c.bytes(p) as f32 / total as f32).min_w(3.0).radius(3.0).fill(color_of(c.color()))
            })),
        )
        .child(row().w_full().flex_wrap().gap(S3).children(parts.iter().filter(|c| c.bytes(p) > 0).map(|c| {
            row()
                .gap(6.0)
                .items_center()
                .child(el().size(6.0, 6.0).radius(3.0).fill(color_of(c.color())))
                .child(label(c.name(), 12.0, 400.0, ink.muted))
                .child(data(model::gb(c.bytes(p)), 11.0, ink.faint))
        })))
        .child(
            Button::new("Show in Storage")
                .variant(ButtonVariant::GhostMuted)
                .icon(&icons::STORAGE)
                .on_press(move |s: &mut State| actions::show_in_storage(s, &name))
                .build(k)
                .key("to-storage")
                .self_start()
                .ml(-6.0),
        )
}

pub fn panel(s: &State, ink: &Ink, k: &Kit, p: &Project, _layer: &mut Vec<El>) -> El {
    let info = s.inspect.get(&p.name);
    let path = actions::path_of(s, &p.name).display().to_string();
    let editor = actions::default_app(s).map(|a| (a.id, a.name));
    let editor_icon = actions::default_app(s).and_then(|a| art::ready(s.art.app(a)));
    let (n1, n2, n3, n4) = (p.name.clone(), p.name.clone(), p.name.clone(), p.name.clone());
    let _pinned = s.saved.pinned.contains(&p.name);
    let stacks = crate::home::stacks(s, p);
    let mut body = col()
        .key("detail-body")
        .w_full()
        .px(S5)
        .pb(S5)
        // Who and what.
        .child(
            col()
                .w_full()
                .gap(S3)
                .pb(S4)
                .children(match info.and_then(|i| i.description.clone()) {
                    Some(d) => Some(label(d, 13.0, 400.0, ink.row).wrap().w_full()),
                    None if info.is_none() => None,
                    None => Some(label("No description in a manifest or README", 13.0, 400.0, ink.faint)),
                })
                .children((!stacks.is_empty()).then(|| row().w_full().flex_wrap().gap(6.0).children(stacks.iter().map(|t| tag(t.clone(), ink)))))
                .child(
                    row()
                        .w_full()
                        .gap(S4)
                        .child(data(fmt::days_ago(p.days), 11.0, ink.faint))
                        .child(data(format!("{} commits", p.commits), 11.0, ink.faint))
                        .child(data(format!("{} lines", fmt::count(p.loc)), 11.0, ink.faint)),
                ),
        );
    if let Some(i) = info.filter(|i| !i.languages.is_empty()) {
        body = body.child(block(ink).key("langs").child(heading("Languages", ink)).child(languages(ink, &i.languages)));
    }
    body = body.child(git_block(s, ink, k, p));
    if let Some(sc) = scripts_block(s, ink, k, p) {
        body = body.child(sc);
    }
    body = body.child(storage_block(ink, k, p));

    let open = match editor {
        Some((id, name)) => {
            let button = Button::new(format!("Open in {name}"))
                .variant(ButtonVariant::Submit)
                .control_size(ControlSize::Large)
                .on_press(move |s: &mut State| actions::open_in(s, &n1, id));
            match editor_icon {
                Some(img) => button
                    .label_with(move |font, ink| {
                        row()
                            .gap(S2)
                            .items_center()
                            .child(el().size(16.0, 16.0).pixels(img.pixels()))
                            .child(text(format!("Open in {name}")).font(font).color(ink))
                    })
                    .build(k),
                None => button.icon(&icons::CODE).build(k),
            }
        }
        None => Button::new("Reveal")
            .variant(ButtonVariant::Submit)
            .icon(&icons::FOLDER)
            .control_size(ControlSize::Large)
            .on_press(move |s: &mut State| actions::reveal(s, &n1))
            .build(k),
    };
    let ghost = |glyph: &'static rune::Icon, tip: &str, f: Box<dyn Fn(&mut State)>| {
        IconButton::new(glyph)
            .variant(IconButtonVariant::Ghost)
            .control_size(ControlSize::Large)
            .label(tip.to_string())
            .help(tip.to_string())
            .on_press(move |s: &mut State| f(s))
            .build(k)
    };
    let name_for_menu = p.name.clone();
    col()
        .key("detail")
        .w(PANEL_W)
        .h_full()
        .shrink()
        .fill(ink.side)
        .border_sides([0.0, 0.0, 0.0, 1.0], ink.line)
        .child(
            col()
                .w_full()
                .px(S5)
                .pt(S4)
                .pb(S4)
                .gap(S1)
                .child(
                    row()
                        .w_full()
                        .h(32.0)
                        .items_center()
                        .gap(S2)
                        .child(crate::home::logo_mark(s, ink, p, 28.0))
                        .child(label(p.name.clone(), 16.0, 500.0, ink.fg).grow(1.0).min_w(0.0).truncate())
                        .child(crate::home::star(s, ink, &n4, true))
                        .child(
                            IconButton::new(&icons::CLOSE)
                                .variant(IconButtonVariant::Ghost)
                                .label("Close details")
                                .help("Close · Esc")
                                .on_press(|s: &mut State| s.home.detail = false)
                                .build(k)
                                .key("detail-close"),
                        ),
                )
                .child(
                    row()
                        .key("detail-path")
                        .cursor(Cursor::Pointer)
                        .on_press_tx(move |s: &mut State, tx: &mut Tx| actions::copy_path(s, &n2, tx))
                        .help("Copy path")
                        .child(data(crate::ui::clip_path(&path, 48), 11.0, ink.faint).truncate()),
                )
                .child(
                    row()
                        .w_full()
                        .pt(S3)
                        .gap(S1)
                        .items_center()
                        .child(open)
                        .child(el().grow(1.0))
                        .child(ghost(&icons::TERMINAL, "Open terminal", Box::new(move |s| actions::terminal(s, &n3))))
                        .child(ghost(
                            &icons::FOLDER,
                            "Reveal in Explorer",
                            Box::new({
                                let n = p.name.clone();
                                move |s| actions::reveal(s, &n)
                            }),
                        ))
                        .child(
                            IconButton::new(&icons::MORE)
                                .variant(IconButtonVariant::Ghost)
                                .control_size(ControlSize::Large)
                                .label("More")
                                .help("More actions")
                                .build(k)
                                .key("detail-more")
                                .on_press_tx(move |s: &mut State, _tx: &mut Tx| {
                                    let at = rune::PointerPress { x: s.window_w - 236.0, y: 108.0 };
                                    s.home.menu = Some((name_for_menu.clone(), at));
                                    s.home.sub = None;
                                }),
                        ),
                ),
        )
        .child(
            col()
                .key("detail-scroll")
                .grow(1.0)
                .min_h(0.0)
                .w_full()
                .scroll(ScrollSpec { stick_bottom: false })
                .scroll_edge_fade(16.0)
                .child(body),
        )
}

#[allow(dead_code)]
fn unused(_: Align) -> Option<cats::Tier> {
    None
}
