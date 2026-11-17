//! The window's frame: a slim sidebar of pages, a header per page, toasts, the shortcut sheet
//! and the window keys. Every page draws into the same frame, so the chrome never moves.

use rune::{col, el, icon, row, Color, Cursor, Cx, El, Icon, Key, KeyEvent, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Toast, ToastRegion};

use crate::icons;
use crate::ui::{data, keycap, label, Ink, R_CARD, R_CTRL, S1, S2, S3, S4, S5, S6};
use crate::State;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Storage,
    Usage,
    Templates,
    Settings,
}

impl Page {
    pub const ALL: [Page; 5] = [Page::Home, Page::Storage, Page::Usage, Page::Templates, Page::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Page::Home => "Home",
            Page::Storage => "Storage",
            Page::Usage => "Usage",
            Page::Templates => "Templates",
            Page::Settings => "Settings",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Page::Home => "home",
            Page::Storage => "storage",
            Page::Usage => "usage",
            Page::Templates => "templates",
            Page::Settings => "settings",
        }
    }

    fn icon(self) -> &'static Icon {
        match self {
            Page::Home => &icons::HOME,
            Page::Storage => &icons::STORAGE,
            Page::Usage => &icons::USAGE,
            Page::Templates => &icons::TEMPLATES,
            Page::Settings => &icons::SETTINGS,
        }
    }
}

pub const SIDEBAR_W: f32 = 216.0;

// ---- toasts -----------------------------------------------------------------

pub struct Note {
    pub id: u64,
    pub text: String,
    pub error: bool,
    /// Clock time it goes away; set by the first poll that sees it.
    pub until: Option<f64>,
}

const TOAST_SECS: f64 = 3.5;

/// Gives new toasts their deadline and drops expired ones. True when the list changed.
pub fn tick_toasts(s: &mut State, clock: f64, tx: &mut Tx) -> bool {
    let before = s.notes.len();
    for n in s.notes.iter_mut() {
        if n.until.is_none() {
            n.until = Some(clock + if n.error { TOAST_SECS * 2.0 } else { TOAST_SECS });
        }
    }
    s.notes.retain(|n| n.until.is_none_or(|u| u > clock));
    if let Some(next) = s.notes.iter().filter_map(|n| n.until).reduce(f64::min) {
        let wait = (next - clock).max(0.05);
        tx.wake_after(std::time::Duration::from_secs_f64(wait));
    }
    // Only three at a time: older news gives way.
    while s.notes.len() > 3 {
        s.notes.remove(0);
    }
    s.notes.len() != before
}

fn toasts(s: &State, ink: &Ink, k: &Kit) -> El {
    let mut region = ToastRegion::new();
    for n in &s.notes {
        let id = n.id;
        let glyph = icon(if n.error { &icons::ALERT } else { &icons::CHECK }, 16.0)
            .color(if n.error { ink.red } else { ink.accent });
        region = region.toast(
            Toast::new(format!("toast/{id}"), n.text.clone())
                .lead(glyph)
                .on_close(move |s: &mut State| s.notes.retain(|n| n.id != id))
                .build(k),
        );
    }
    region.build(k)
}

// ---- sidebar ----------------------------------------------------------------

/// The app's mark: three lit dots of the meter, the one picture the app has.
pub(crate) fn mark(ink: &Ink) -> El {
    let dot = |c: Color| el().size(5.0, 5.0).radius(2.5).fill(c);
    col()
        .gap(2.0)
        .child(row().gap(2.0).child(dot(ink.accent)).child(dot(ink.accent)))
        .child(row().gap(2.0).child(dot(ink.accent)).child(dot(ink.raised)))
}

fn nav_row(s: &State, ink: &Ink, page: Page, index: usize, count: Option<String>) -> El {
    let on = s.page == page;
    row()
        .key(format!("nav/{}", page.id()))
        .h(32.0)
        .px(S2 + 2.0)
        .gap(S3 - 2.0)
        .radius(R_CTRL)
        .items_center()
        .cursor(Cursor::Pointer)
        .fill(if on { ink.raised } else { Color::TRANSPARENT })
        .hover(ink.hover)
        .on_press(move |s: &mut State| s.go(page))
        .help(format!("{} · Ctrl+{}", page.label(), index + 1))
        .child(icon(page.icon(), 16.0).color(if on { ink.fg } else { ink.muted }))
        .child(label(page.label(), 13.0, if on { 500.0 } else { 400.0 }, if on { ink.fg } else { ink.muted }).grow(1.0))
        .children(count.map(|c| data(c, 11.0, ink.faint)))
}

fn scan_status(s: &State, ink: &Ink, k: &Kit) -> El {
    let line: El = if s.scanning {
        let pct = if s.total > 0 { s.done_count as f32 / s.total as f32 } else { 0.0 };
        col()
            .gap(S2)
            .w_full()
            .child(
                row()
                    .gap(S2)
                    .items_center()
                    .child(rune_kit::Spinner::new().build(k))
                    .child(label(
                        if s.total > 0 { format!("Scanning {}/{}", s.done_count, s.total) } else { "Starting scan".into() },
                        12.0,
                        400.0,
                        ink.muted,
                    )),
            )
            .child(crate::ui::bar(pct, ink.accent, ink))
    } else if let Some(e) = &s.error {
        label(e.clone(), 12.0, 400.0, ink.red).truncate()
    } else {
        let when = match s.last_done {
            Some(_) if s.demo => "Demo data".to_string(),
            Some(_) => format!("Scanned {}", s.done_at.clone().unwrap_or_default()),
            None => "Not scanned yet".to_string(),
        };
        row()
            .w_full()
            .items_center()
            .gap(S2)
            .child(label(when, 12.0, 400.0, ink.faint).grow(1.0).min_w(0.0).truncate())
            .child(
                IconButton::new(&icons::REFRESH)
                    .variant(IconButtonVariant::Ghost)
                    .control_size(ControlSize::Small)
                    .label("Rescan")
                    .help("Rescan · Ctrl+R")
                    .on_press(|s: &mut State| s.rescan())
                    .build(k)
                    .key("rescan"),
            )
    };
    col()
        .key("scan-status")
        .w_full()
        .gap(S1)
        .child(line)
        .children((!s.engine_line().is_empty()).then(|| data(s.engine_line(), 10.5, ink.faint)))
}

fn sidebar(s: &State, ink: &Ink, k: &Kit) -> El {
    let projects = s.projects.iter().filter(|p| !s.saved.archived.contains(&p.name)).count();
    let counts = |page: Page| -> Option<String> {
        match page {
            Page::Home if !s.projects.is_empty() => Some(projects.to_string()),
            Page::Templates if !s.tmpl.list.is_empty() => Some(s.tmpl.list.len().to_string()),
            _ => None,
        }
    };
    col()
        .key("sidebar")
        .w(SIDEBAR_W)
        .h_full()
        .shrink()
        .fill(ink.side)
        .border_sides([0.0, 1.0, 0.0, 0.0], ink.line)
        .px(S3)
        .pt(S5)
        .pb(S4)
        .gap(S1)
        .child(
            row()
                .key("brand")
                .px(S2 + 2.0)
                .gap(S3 - 2.0)
                .items_center()
                .child(mark(ink))
                .child(label("Projects", 14.0, 500.0, ink.fg)),
        )
        .child(
            row()
                .key("root")
                .px(S2 + 2.0)
                .pl(S2 + 2.0 + 14.0 + 10.0)
                .pb(S4)
                .child(data(crate::ui::clip_path(&s.root, 24), 11.0, ink.faint).truncate())
                .help("Projects folder · change it in Settings"),
        )
        .children(Page::ALL.into_iter().enumerate().map(|(i, p)| nav_row(s, ink, p, i, counts(p))))
        .child(el().grow(1.0))
        .child(
            row()
                .key("shortcuts")
                .h(28.0)
                .px(S2 + 2.0)
                .gap(S3 - 2.0)
                .radius(R_CTRL)
                .items_center()
                .cursor(Cursor::Pointer)
                .hover(ink.hover)
                .on_press(|s: &mut State| s.shortcuts = true)
                .child(icon(&icons::KEYBOARD, 16.0).color(ink.faint))
                .child(label("Shortcuts", 12.0, 400.0, ink.faint).grow(1.0))
                .child(keycap("?", ink)),
        )
        .child(el().h(S2))
        .child(
            col()
                .w_full()
                .px(S2 + 2.0)
                .pt(S3)
                .border_sides([1.0, 0.0, 0.0, 0.0], ink.line)
                .gap(S3)
                .children(crate::update_ui::sidebar_line(s, ink, k))
                .child(scan_status(s, ink, k)),
        )
}

// ---- page header ------------------------------------------------------------

/// A page's title row: the page's name, a quiet line of facts, and the page's actions at the
/// far end. Every page uses it, so titles sit at the same place on every page.
pub fn header(title: &str, meta: Option<String>, trailing: Vec<El>, ink: &Ink) -> El {
    row()
        .key("topline")
        .w_full()
        .h(64.0)
        .shrink()
        .px(S6)
        .gap(S3)
        .items_center()
        .child(
            row()
                .gap(S3)
                .items_end()
                .min_w(0.0)
                .shrink()
                .child(label(title, 18.0, 500.0, ink.fg))
                .children(meta.map(|m| data(m, 12.0, ink.faint).pb(2.0).truncate())),
        )
        .child(el().grow(1.0))
        .child(row().gap(S2).items_center().children(trailing))
}

/// The primary action of a page: the one accent button.
pub fn primary(text_: &str, glyph: &'static Icon) -> Button {
    Button::new(text_).variant(ButtonVariant::Submit).icon(glyph).control_size(ControlSize::Large)
}

// ---- modal sheet --------------------------------------------------------------

/// A centred modal panel over a scrim: the app's dialogs (new project, shortcuts). Escape and a
/// press outside call `close`.
pub fn sheet(
    key: &str,
    title: &str,
    width: f32,
    body: El,
    close: impl Fn(&mut State) + Clone + 'static,
    ink: &Ink,
    k: &Kit,
    layer: &mut Vec<El>,
) {
    let x = close.clone();
    let panel = col()
        .key(format!("{key}-sheet"))
        .role("dialog")
        .label(title.to_string())
        .w_full()
        .max_w(width)
        .radius(R_CARD + 2.0)
        .fill(ink.card)
        .border(1.0, ink.line_strong)
        .shadow(16.0, 48.0, Color::BLACK.alpha(0.5))
        .child(
            row()
                .w_full()
                .px(S5)
                .pt(S5)
                .pb(S3)
                .items_center()
                .child(label(title, 15.0, 500.0, ink.fg).grow(1.0))
                .child(
                    IconButton::new(&icons::CLOSE)
                        .variant(IconButtonVariant::Ghost)
                        .label("Close")
                        .on_press(move |s: &mut State| x(s))
                        .build(k),
                ),
        )
        .child(body)
        .modal(true, close)
        .token_motion(k.motion.overlay);
    layer.push(
        el().key(format!("{key}-scrim"))
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .fill(Color::BLACK.alpha(0.55))
            .token_motion(k.motion.overlay)
            .a11y_hidden(),
    );
    layer.push(
        el().key(format!("{key}-layer"))
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .padding(S6)
            .center()
            .child(panel),
    );
}

const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl 1–5", "Go to Home, Storage, Usage, Templates, Settings"),
    ("/  or  Ctrl F", "Search projects"),
    ("Ctrl N", "New project"),
    ("Ctrl R", "Rescan the projects folder"),
    ("Arrows", "Move between projects"),
    ("Enter", "Open in your editor"),
    ("Space", "Show or hide details"),
    ("P", "Pin or unpin"),
    ("T", "Open a terminal"),
    ("Esc", "Close, clear the search"),
];

fn shortcut_sheet(ink: &Ink, k: &Kit, layer: &mut Vec<El>) {
    let body = col()
        .key("shortcut-list")
        .w_full()
        .px(S5)
        .pb(S5)
        .gap(S1)
        .children(SHORTCUTS.iter().map(|(keys, what)| {
            row()
                .w_full()
                .h(30.0)
                .items_center()
                .gap(S3)
                .child(
                    row()
                        .w(132.0)
                        .gap(S1)
                        .children(keys.split("  or  ").enumerate().flat_map(|(i, alt)| {
                            let caps: Vec<El> = alt.split(' ').map(|c| keycap(c, ink)).collect();
                            let or = (i > 0).then(|| data("or", 11.0, ink.faint));
                            or.into_iter().chain(caps)
                        })),
                )
                .child(label(*what, 13.0, 400.0, ink.muted))
        }));
    sheet("shortcuts", "Keyboard shortcuts", 520.0, body, |s: &mut State| s.shortcuts = false, ink, k, layer);
}

// ---- the window ----------------------------------------------------------------

pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    s.art.set_scale(cx.env().scale);
    let ink = Ink::new();
    let k = ink.kit();
    let mut layer: Vec<El> = Vec::new();
    let body = match s.page {
        Page::Home => crate::home::view(s, &ink, &k, &mut layer),
        Page::Storage => crate::storage_ui::view(s, &ink, &k, cx),
        Page::Usage => crate::usage_ui::view(s, &ink, &k),
        Page::Templates => crate::templates_ui::view(s, &ink, &k, &mut layer),
        Page::Settings => crate::settings_ui::view(s, &ink, &k, &mut layer),
    };
    let window = row()
        .key("page")
        .w_full()
        .h_full()
        .fill(ink.bg)
        .on_layout(|s: &mut State, r: [f32; 4]| {
            if (r[2] - s.window_w).abs() > 0.5 {
                s.window_w = r[2];
            }
        })
        .child(sidebar(s, &ink, &k))
        .child(col().key("main").grow(1.0).min_w(0.0).h_full().child(body.grow(1.0).min_h(0.0)));
    crate::newproj::dialog(s, &ink, &k, &mut layer);
    if s.shortcuts {
        shortcut_sheet(&ink, &k, &mut layer);
    }
    layer.push(toasts(s, &ink, &k));
    rune_kit::root(window, layer)
}

// ---- keys ---------------------------------------------------------------------

/// Window keys. Text fields keep their own keys; everything else lands here.
pub fn on_key(s: &mut State, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let typing = tx.focused().is_some_and(|k| k.ends_with("input"));
    for (i, page) in Page::ALL.into_iter().enumerate() {
        if ev.is_command(char::from(b'1' + i as u8)) {
            s.go(page);
            return true;
        }
    }
    if ev.is_command('n') {
        s.newp.open(&s.saved.root);
        return true;
    }
    if ev.is_command('r') {
        s.rescan();
        return true;
    }
    let slash = matches!(&ev.key, Key::Text(t) if t == "/") && !typing;
    if slash || ev.is_command('f') {
        s.go(Page::Home);
        s.home.search.focused = true;
        return true;
    }
    if s.newp.open || s.shortcuts {
        return false;
    }
    if !typing && matches!(&ev.key, Key::Text(t) if t == "?") {
        s.shortcuts = true;
        return true;
    }
    if s.page == Page::Home && !typing {
        return crate::home::on_key(s, ev, tx);
    }
    false
}

