//! Home: every project under the folder as a grid of cards (or a dense list), pinned and
//! recently opened ones first, with search, filters, a right-click menu and keyboard moves.
//! Clicking a card opens its details beside the grid.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use rune::{col, el, icon, row, Align, Color, Cursor, El, Key, KeyEvent, PointerPress, ScrollSpec, SurfaceFit, TextEdit, Tx};
use rune_kit::{
    context_menu, Button, ButtonVariant, ControlSize, Kit, MenuEntry, MenuItem,
    SegmentedControl, Select, Spinner, TextInput,
};

use crate::actions;
use crate::art;
use crate::fmt;
use crate::icons;
use crate::launch::App;
use crate::model::{self, Project};
use crate::saved::{now_unix, Layout, Order};
use crate::shell;
use crate::ui::{data, label, tag, Ink, R_CARD, R_CTRL, S1, S2, S3, S4, S6};
use crate::State;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recency {
    All,
    Today,
    Week,
    Month,
    Older,
}

impl Recency {
    pub const ALL: [Recency; 5] = [Recency::All, Recency::Today, Recency::Week, Recency::Month, Recency::Older];

    pub fn label(self) -> &'static str {
        match self {
            Recency::All => "Any time",
            Recency::Today => "Today",
            Recency::Week => "Week",
            Recency::Month => "Month",
            Recency::Older => "Older",
        }
    }

    /// Whether a project last committed `days` ago belongs here. No history counts as older.
    pub fn admits(self, days: Option<u64>) -> bool {
        match (self, days) {
            (Recency::All, _) => true,
            (Recency::Today, Some(d)) => d < 1,
            (Recency::Week, Some(d)) => d < 7,
            (Recency::Month, Some(d)) => d < 30,
            (Recency::Older, d) => d.is_none_or(|d| d >= 30),
            _ => false,
        }
    }
}

pub struct Home {
    pub search: TextEdit,
    pub recency: Recency,
    /// Only projects whose stack includes this.
    pub stack: Option<String>,
    pub show_archived: bool,
    /// The highlighted project, by name, so a rescan reordering rows keeps the pick.
    pub selected: Option<String>,
    /// The details panel beside the grid.
    pub detail: bool,
    pub hover: Option<String>,
    /// The right-click menu: which project, where, and which submenu is open.
    pub menu: Option<(String, PointerPress)>,
    pub sub: Option<&'static str>,
    /// Which toolbar select is open.
    pub open_select: Option<&'static str>,
    /// The content column's inner width, for the number of card columns.
    pub grid_w: f32,
    /// Current branch per project, read from `.git/HEAD`.
    pub branches: HashMap<String, String>,
}

impl Home {
    pub fn new() -> Self {
        Home {
            search: TextEdit::new(""),
            recency: Recency::All,
            stack: None,
            show_archived: false,
            selected: None,
            detail: false,
            hover: None,
            menu: None,
            sub: None,
            open_select: None,
            grid_w: 0.0,
            branches: HashMap::new(),
        }
    }

    /// Reads the branch of every project not seen yet: one small file per repo.
    pub fn refresh_branches(&mut self, root: &Path, projects: &[Project]) {
        for p in projects {
            if self.branches.contains_key(&p.name) {
                continue;
            }
            let head = if p.git {
                std::fs::read_to_string(root.join(&p.name).join(".git").join("HEAD")).unwrap_or_default()
            } else {
                String::new()
            };
            let head = head.trim();
            let branch = match head.strip_prefix("ref: refs/heads/") {
                Some(b) => b.to_string(),
                None => head.chars().take(7).collect(),
            };
            self.branches.insert(p.name.clone(), branch);
        }
    }

    pub fn filtering(&self) -> bool {
        !self.search.text.trim().is_empty() || self.recency != Recency::All || self.stack.is_some() || self.show_archived
    }

    pub fn clear_filters(&mut self) {
        self.search.replace_text("");
        self.recency = Recency::All;
        self.stack = None;
        self.show_archived = false;
    }
}

// ---- what is on screen ---------------------------------------------------------

/// A project's stack labels: the manifest pass when it has run, else the scan's own guess.
pub fn stacks(s: &State, p: &Project) -> Vec<String> {
    match s.inspect.stack(&p.name) {
        Some(st) if !st.is_empty() => st.iter().map(|x| x.to_string()).collect(),
        _ => p.stack.split(", ").filter(|x| !x.is_empty()).map(str::to_string).collect(),
    }
}

fn description(s: &State, name: &str) -> Option<String> {
    s.inspect.get(name).and_then(|i| i.description.clone())
}

fn dirty(s: &State, p: &Project) -> u64 {
    s.inspect.get(&p.name).map(|i| i.dirty as u64).unwrap_or(p.dirty)
}

pub struct Section {
    pub key: &'static str,
    pub title: String,
    pub items: Vec<usize>,
}

fn matches(s: &State, p: &Project, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    p.name.to_lowercase().contains(needle)
        || stacks(s, p).iter().any(|x| x.to_lowercase().contains(needle))
        || description(s, &p.name).is_some_and(|d| d.to_lowercase().contains(needle))
}

/// Indices into `s.projects` that pass the search and filters, in the chosen order.
pub fn visible(s: &State) -> Vec<usize> {
    let h = &s.home;
    let needle = h.search.text.trim().to_lowercase();
    let mut rows: Vec<usize> = (0..s.projects.len())
        .filter(|&i| {
            let p = &s.projects[i];
            let archived = s.saved.archived.contains(&p.name);
            archived == h.show_archived
                && h.recency.admits(p.days)
                && h.stack.as_ref().is_none_or(|want| stacks(s, p).iter().any(|x| x == want))
                && matches(s, p, &needle)
        })
        .collect();
    let ps = &s.projects;
    match s.saved.order {
        Order::Recent => rows.sort_by(|&a, &b| {
            ps[a].days
                .unwrap_or(u64::MAX)
                .cmp(&ps[b].days.unwrap_or(u64::MAX))
                .then(ps[a].name.to_lowercase().cmp(&ps[b].name.to_lowercase()))
        }),
        Order::Name => rows.sort_by_key(|&i| ps[i].name.to_lowercase()),
        Order::Size => rows.sort_by(|&a, &b| ps[b].bytes.cmp(&ps[a].bytes)),
    }
    rows
}

/// Pinned, then opened recently, then the rest; a search or filter shows one flat list.
pub fn sections(s: &State) -> Vec<Section> {
    let rows = visible(s);
    if s.home.filtering() {
        let title = if s.home.show_archived { "Archived" } else { "Results" };
        return vec![Section { key: "results", title: title.into(), items: rows }];
    }
    let pinned: Vec<usize> = rows.iter().copied().filter(|&i| s.saved.pinned.contains(&s.projects[i].name)).collect();
    let cutoff = now_unix() - 14 * 86_400;
    let mut recent: Vec<usize> = rows
        .iter()
        .copied()
        .filter(|&i| {
            let n = &s.projects[i].name;
            !s.saved.pinned.contains(n) && s.saved.opened.get(n).is_some_and(|&t| t >= cutoff)
        })
        .collect();
    recent.sort_by_key(|&i| std::cmp::Reverse(s.saved.opened.get(&s.projects[i].name).copied().unwrap_or(0)));
    recent.truncate(columns(s).max(3).min(6));
    let rest: Vec<usize> = rows.iter().copied().filter(|i| !pinned.contains(i) && !recent.contains(i)).collect();
    let mut out = Vec::new();
    if !pinned.is_empty() {
        out.push(Section { key: "pinned", title: "Pinned".into(), items: pinned });
    }
    if !recent.is_empty() {
        out.push(Section { key: "recent", title: "Opened recently".into(), items: recent });
    }
    if !rest.is_empty() {
        let title = if out.is_empty() { "All projects" } else { "Everything else" };
        out.push(Section { key: "all", title: title.into(), items: rest });
    }
    out
}

const CARD_MIN: f32 = 264.0;
const GAP: f32 = 16.0;
const CARD_H: f32 = 148.0;

pub fn columns(s: &State) -> usize {
    if s.saved.layout == Layout::List {
        return 1;
    }
    let w = if s.home.grid_w > 0.0 { s.home.grid_w } else { 900.0 };
    (((w + GAP) / (CARD_MIN + GAP)).floor() as usize).clamp(1, 6)
}

/// The rows as laid out: each section's items cut into rows of `columns`.
fn grid_rows(s: &State) -> Vec<Vec<usize>> {
    let cols = columns(s);
    sections(s).into_iter().flat_map(|sec| sec.items.chunks(cols).map(<[usize]>::to_vec).collect::<Vec<_>>()).collect()
}

// ---- keys ----------------------------------------------------------------------

fn select_index(s: &mut State, i: usize) {
    s.home.selected = Some(s.projects[i].name.clone());
}

pub fn on_key(s: &mut State, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if ev.key == Key::Escape {
        if s.home.menu.is_some() {
            s.home.menu = None;
        } else if s.home.detail {
            s.home.detail = false;
        } else if s.home.filtering() {
            s.home.clear_filters();
        } else {
            return false;
        }
        return true;
    }
    let rows = grid_rows(s);
    if rows.is_empty() {
        return false;
    }
    let at = s.home.selected.as_ref().and_then(|name| {
        rows.iter().enumerate().find_map(|(r, line)| {
            line.iter().position(|&i| &s.projects[i].name == name).map(|c| (r, c))
        })
    });
    let (dr, dc): (i32, i32) = match ev.key {
        Key::Down => (1, 0),
        Key::Up => (-1, 0),
        Key::Right => (0, 1),
        Key::Left => (0, -1),
        _ => (0, 0),
    };
    if dr != 0 || dc != 0 {
        let (r, c) = match at {
            None => (0, 0),
            Some((r, c)) => {
                if dc != 0 {
                    // Left and right walk on through row ends, the way reading order goes.
                    let flat: Vec<usize> = rows.iter().flatten().copied().collect();
                    let here = flat.iter().position(|&i| i == rows[r][c]).unwrap_or(0) as i32;
                    let next = (here + dc).clamp(0, flat.len() as i32 - 1) as usize;
                    select_index(s, flat[next]);
                    tx.scroll_into_view(&format!("card/{}", s.projects[flat[next]].name));
                    return true;
                }
                let nr = (r as i32 + dr).clamp(0, rows.len() as i32 - 1) as usize;
                (nr, c.min(rows[nr].len() - 1))
            }
        };
        let i = rows[r][c];
        select_index(s, i);
        tx.scroll_into_view(&format!("card/{}", s.projects[i].name));
        return true;
    }
    let Some(name) = s.home.selected.clone() else { return false };
    match &ev.key {
        Key::Enter => actions::open_default(s, &name),
        Key::Text(t) if t == " " => s.home.detail = !s.home.detail,
        Key::Text(t) if t.eq_ignore_ascii_case("p") => actions::toggle_pin(s, &name),
        Key::Text(t) if t.eq_ignore_ascii_case("t") => actions::terminal(s, &name),
        _ => return false,
    }
    true
}

// ---- the context menu ----------------------------------------------------------

fn close_menu(s: &mut State) {
    s.home.menu = None;
    s.home.sub = None;
}

fn menu_item(text_: impl Into<String>, glyph: &'static rune::Icon, f: impl Fn(&mut State) + 'static) -> MenuItem {
    MenuItem::new(text_).icon(glyph).on_select(move |s: &mut State| {
        f(s);
        close_menu(s);
    })
}

/// A menu row for an editor: its own icon once decoded, else the generic glyph.
fn app_item(s: &State, app: &App, text_: String, f: impl Fn(&mut State) + 'static) -> MenuItem {
    match art::ready(s.art.app(app)) {
        Some(img) => MenuItem::new(text_).lead(el().size(14.0, 14.0).pixels(img.pixels())).on_select(move |s: &mut State| {
            f(s);
            close_menu(s);
        }),
        None => menu_item(text_, &icons::CODE, f),
    }
}

/// A project's logo at `pt` points, or its first letter on a tile until the logo decodes
/// (or when there is none).
pub fn logo_mark(s: &State, ink: &Ink, p: &Project, pt: f32) -> El {
    match art::ready(s.art.logo(&actions::path_of(s, &p.name))) {
        Some(img) => el().size(pt, pt).radius(4.0).pixels_fit(img.pixels(), SurfaceFit::Fit),
        None => {
            let initial = p.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            row()
                .size(pt, pt)
                .radius(4.0)
                .fill(ink.raised)
                .items_center()
                .justify_center()
                .child(label(initial, (pt * 0.5).round(), 600.0, ink.muted))
        }
    }
}

fn context(s: &State, k: &Kit, layer: &mut Vec<El>) -> El {
    let Some((name, at)) = s.home.menu.clone() else {
        return el().key("ctx-anchor");
    };
    let pinned = s.saved.pinned.contains(&name);
    let archived = s.saved.archived.contains(&name);
    let n = name.clone();
    let apps: Vec<MenuEntry> = s
        .apps
        .iter()
        .map(|a| {
            let (n, id) = (n.clone(), a.id);
            MenuEntry::item(app_item(s, a, a.name.to_string(), move |s| actions::open_in(s, &n, id)))
        })
        .collect();
    let scripts: Vec<MenuEntry> = match s.inspect.get(&name) {
        Some(info) if !info.scripts.is_empty() => info
            .scripts
            .iter()
            .map(|(script, command)| {
                let (n, sc) = (name.clone(), script.clone());
                MenuEntry::item(
                    menu_item(script.clone(), &icons::PLAY, move |s| actions::run_script(s, &n, &sc)).description(command.clone()),
                )
            })
            .collect(),
        _ => Vec::new(),
    };
    let has_scripts = !scripts.is_empty();
    let sub = s.home.sub;
    let toggle = |id: &'static str| {
        move |s: &mut State| s.home.sub = if s.home.sub == Some(id) { None } else { Some(id) }
    };
    let (a, b, c, d, e, f, g) = (name.clone(), name.clone(), name.clone(), name.clone(), name.clone(), name.clone(), name.clone());
    let mut m = context_menu("ctx", el().key("ctx-anchor"), Some(at)).min_width(220.0);
    if let Some(app) = actions::default_app(s) {
        let id = app.id;
        m = m.item(
            app_item(s, app, format!("Open in {}", app.name), move |s| actions::open_in(s, &g, id)).shortcut("Enter"),
        );
    }
    m = m
        .item(MenuItem::new("Open in").icon(&icons::EXTERNAL).submenu(apps, sub == Some("apps")).disabled(s.apps.is_empty()).on_select(toggle("apps")))
        .item(menu_item("Open terminal", &icons::TERMINAL, move |s| actions::terminal(s, &a)).shortcut("T"))
        .item(
            MenuItem::new(if has_scripts { "Run script" } else { "No scripts found" })
                .icon(&icons::PLAY)
                .submenu(scripts, sub == Some("scripts"))
                .disabled(!has_scripts)
                .on_select(toggle("scripts")),
        )
        .separator()
        .item(menu_item("Reveal in Explorer", &icons::FOLDER, move |s| actions::reveal(s, &b)))
        .item(MenuItem::new("Copy path").icon(&icons::COPY).on_select_tx(move |s: &mut State, tx: &mut Tx| {
            actions::copy_path(s, &c, tx);
            close_menu(s);
        }))
        .item(menu_item("Show in Storage", &icons::STORAGE, move |s| actions::show_in_storage(s, &d)))
        .separator()
        .item(menu_item(if pinned { "Unpin" } else { "Pin" }, &icons::STAR, move |s| actions::toggle_pin(s, &e)).shortcut("P"))
        .item(menu_item(if archived { "Restore from archive" } else { "Archive" }, &icons::ARCHIVE, move |s| {
            actions::toggle_archived(s, &f)
        }))
        .on_close(close_menu);
    m.build(k, layer)
}

fn open_menu(s: &mut State, name: &str, at: PointerPress) {
    s.home.selected = Some(name.to_string());
    s.home.menu = Some((name.to_string(), at));
    s.home.sub = None;
    let path = actions::path_of(s, name);
    let waker = s.waker.clone();
    s.inspect.want(name, path, true, waker.as_ref());
}

// ---- cards and rows ------------------------------------------------------------

pub fn star(s: &State, ink: &Ink, name: &str, show: bool) -> El {
    let pinned = s.saved.pinned.contains(name);
    let n = name.to_string();
    row()
        .key("star")
        .size(24.0, 24.0)
        .radius(6.0)
        .items_center()
        .justify_center()
        .cursor(Cursor::Pointer)
        .hover(ink.hover)
        .on_press(move |s: &mut State| actions::toggle_pin(s, &n))
        .help(if pinned { "Unpin" } else { "Pin to the top" })
        .child(
            icon(if pinned { &icons::STAR_FILLED } else { &icons::STAR }, 14.0)
                .color(if pinned { ink.accent } else { ink.faint })
                .opacity(if pinned || show { 1.0 } else { 0.0 }),
        )
}

fn tags(s: &State, ink: &Ink, p: &Project, max: usize) -> El {
    let all = stacks(s, p);
    let extra = all.len().saturating_sub(max);
    row()
        .gap(S1 + 2.0)
        .min_w(0.0)
        .shrink()
        .items_center()
        .children(all.into_iter().take(max).map(|t| tag(t, ink)))
        .children((extra > 0).then(|| data(format!("+{extra}"), 11.0, ink.faint)))
}

fn age(p: &Project) -> String {
    fmt::days_ago(p.days)
}

/// Branch and uncommitted changes, the footer of a card and two columns of a row.
fn git_bits(s: &State, ink: &Ink, p: &Project) -> (El, El) {
    let branch = s.home.branches.get(&p.name).cloned().unwrap_or_default();
    let b = if p.git && !branch.is_empty() {
        row()
            .gap(S1)
            .min_w(0.0)
            .shrink()
            .items_center()
            .child(icon(&icons::BRANCH, 12.0).color(ink.faint))
            .child(data(branch, 11.0, ink.muted).truncate())
    } else if p.git {
        row()
    } else {
        data("no git", 11.0, ink.faint)
    };
    let d = dirty(s, p);
    let dirt = if d > 0 {
        row()
            .gap(S1)
            .items_center()
            .child(el().size(6.0, 6.0).radius(3.0).fill(ink.accent))
            .child(data(d.to_string(), 11.0, ink.accent))
            .help(format!("{d} uncommitted {}", if d == 1 { "change" } else { "changes" }))
    } else {
        row()
    };
    (b, dirt)
}

fn wire(e: El, name: &str) -> El {
    let (n1, n2, n3, n4) = (name.to_string(), name.to_string(), name.to_string(), name.to_string());
    e.cursor(Cursor::Pointer)
        .on_press(move |s: &mut State| {
            s.home.selected = Some(n1.clone());
            s.home.detail = true;
            s.home.menu = None;
        })
        .on_double_press(move |s: &mut State| actions::open_default(s, &n2))
        .on_secondary_press(move |s: &mut State, at, _| open_menu(s, &n3, at))
        .on_hover_change(move |s: &mut State, inside, _| {
            if inside {
                s.home.hover = Some(n4.clone());
            } else if s.home.hover.as_deref() == Some(n4.as_str()) {
                s.home.hover = None;
            }
        })
}

fn card(s: &State, ink: &Ink, p: &Project, w: f32) -> El {
    let selected = s.home.selected.as_deref() == Some(p.name.as_str());
    let hovered = s.home.hover.as_deref() == Some(p.name.as_str());
    let desc = description(s, &p.name);
    let (branch, dirt) = git_bits(s, ink, p);
    let loading = desc.is_none() && s.inspect.get(&p.name).is_none();
    let e = col()
        .key(format!("card/{}", p.name))
        .w(w)
        .h(CARD_H)
        .padding(S4)
        .pr(S2 + 2.0)
        .pt(S3)
        .gap(S2)
        .radius(R_CARD)
        .fill(if selected { ink.raised } else { ink.card })
        .border(1.0, if selected { ink.accent.alpha(0.55) } else { ink.line })
        .hover(ink.hover)
        .child(
            row()
                .w_full()
                .items_center()
                .gap(S2)
                .child(logo_mark(s, ink, p, 20.0))
                .child(label(p.name.clone(), 14.0, 500.0, ink.fg).grow(1.0).min_w(0.0).truncate())
                .child(star(s, ink, &p.name, hovered || selected)),
        )
        .child(
            el().w_full().h(34.0).pr(S2).child(match desc {
                Some(d) => label(d, 12.0, 400.0, ink.muted).wrap().max_lines(2).w_full(),
                None => label(if loading { "" } else { "No description" }, 12.0, 400.0, ink.faint),
            }),
        )
        .child(tags(s, ink, p, 3).pr(S2))
        .child(el().grow(1.0))
        .child(
            row()
                .w_full()
                .pr(S2)
                .items_center()
                .gap(S3)
                .child(branch)
                .child(dirt)
                .child(el().grow(1.0))
                .child(data(age(p), 11.0, if p.days.is_some_and(|d| d < 7) { ink.muted } else { ink.faint }))
                .child(data(model::gb(p.bytes), 11.0, ink.faint)),
        );
    wire(e, &p.name)
}

const LIST_NAME_W: f32 = 176.0;
const LIST_STACK_W: f32 = 180.0;
const LIST_BRANCH_W: f32 = 150.0;
const LIST_AGE_W: f32 = 84.0;
const LIST_SIZE_W: f32 = 76.0;

fn list_row(s: &State, ink: &Ink, p: &Project, wide: bool) -> El {
    let selected = s.home.selected.as_deref() == Some(p.name.as_str());
    let hovered = s.home.hover.as_deref() == Some(p.name.as_str());
    let (branch, dirt) = git_bits(s, ink, p);
    let e = row()
        .key(format!("card/{}", p.name))
        .w_full()
        .h(40.0)
        .px(S2)
        .gap(S3)
        .radius(R_CTRL)
        .items_center()
        .fill(if selected { ink.raised } else { Color::TRANSPARENT })
        .border(1.0, if selected { ink.accent.alpha(0.45) } else { Color::TRANSPARENT })
        .hover(ink.hover)
        .child(star(s, ink, &p.name, hovered || selected))
        .child(label(p.name.clone(), 13.0, 500.0, ink.fg).w(LIST_NAME_W).truncate())
        .child(row().w(LIST_STACK_W).child(tags(s, ink, p, 2)))
        .child(
            label(description(s, &p.name).unwrap_or_default(), 12.0, 400.0, ink.muted)
                .grow(1.0)
                .min_w(0.0)
                .truncate(),
        )
        .children(wide.then(|| row().w(LIST_BRANCH_W).gap(S2).items_center().child(branch).child(dirt)))
        .child(data(age(p), 11.0, ink.faint).w(LIST_AGE_W).align(Align::End))
        .child(data(model::gb(p.bytes), 11.0, ink.faint).w(LIST_SIZE_W).align(Align::End));
    wire(e, &p.name)
}

// ---- the page ------------------------------------------------------------------

fn search(s: &State, ink: &Ink, k: &Kit) -> El {
    let empty = s.home.search.text.is_empty() && !s.home.search.focused;
    let mut input = TextInput::new(&s.home.search)
        .placeholder("Search")
        .leading_icon(&icons::SEARCH)
        .leading_icon_size(14.0)
        .width(Some(if s.home.detail { 200.0 } else { 240.0 }))
        .on_edit(|s: &mut State| &mut s.home.search)
        .on_key(|s: &mut State, ev: &KeyEvent, tx: &mut Tx| match ev.key {
            Key::Escape => {
                s.home.search.replace_text("");
                tx.blur();
                true
            }
            Key::Down | Key::Enter => {
                if let Some(first) = grid_rows(s).first().and_then(|r| r.first().copied()) {
                    select_index(s, first);
                }
                tx.blur();
                true
            }
            _ => false,
        });
    if empty {
        input = input.trailing(crate::ui::keycap("/", ink));
    } else if !s.home.search.text.is_empty() {
        input = input.clear_button();
    }
    row().key("search").child(input.build(k))
}

fn layout_toggle(s: &State, ink: &Ink) -> El {
    let seg = |l: Layout, glyph: &'static rune::Icon, tip: &'static str| {
        let on = s.saved.layout == l;
        row()
            .key(format!("layout/{}", if l == Layout::Grid { "grid" } else { "list" }))
            .size(28.0, 24.0)
            .radius(5.0)
            .items_center()
            .justify_center()
            .cursor(Cursor::Pointer)
            .fill(if on { ink.raised } else { Color::TRANSPARENT })
            .hover(ink.hover)
            .on_press(move |s: &mut State| s.saved.set_layout(l))
            .help(tip)
            .child(icon(glyph, 14.0).color(if on { ink.fg } else { ink.faint }))
    };
    row()
        .key("layout")
        .h(28.0)
        .padding(2.0)
        .gap(2.0)
        .radius(R_CTRL)
        .border(1.0, ink.line)
        .child(seg(Layout::Grid, &icons::HOME, "Cards"))
        .child(seg(Layout::List, &icons::LIST, "List"))
}

fn stack_options(s: &State) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for p in &s.projects {
        for st in stacks(s, p).into_iter().take(2) {
            *counts.entry(st).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    std::iter::once("All stacks".to_string()).chain(v.into_iter().take(14).map(|(n, _)| n)).collect()
}

fn toolbar(s: &State, ink: &Ink, k: &Kit, layer: &mut Vec<El>) -> El {
    let rec = Recency::ALL.iter().position(|r| *r == s.home.recency);
    // Beside the details panel the bar is narrow: shorter labels, no "Sort" caption.
    let narrow = s.home.grid_w > 0.0 && s.home.grid_w < 760.0;
    let labels: Vec<&str> =
        Recency::ALL.iter().map(|r| if narrow && *r == Recency::All { "All" } else { r.label() }).collect();
    let stacks = stack_options(s);
    let stack_now = s.home.stack.clone().unwrap_or_else(|| "All stacks".into());
    let orders: Vec<&str> = Order::ALL.iter().map(|o| o.label()).collect();
    let archived = s.projects.iter().filter(|p| s.saved.archived.contains(&p.name)).count();
    row()
        .key("home-tools")
        .w_full()
        .h(44.0)
        .shrink()
        .px(S6)
        .gap(S3)
        .items_center()
        .child(if narrow {
            let names: Vec<&str> = Recency::ALL.iter().map(|r| r.label()).collect();
            Select::new("recency", &names)
                .current(Some(s.home.recency.label()))
                .width(112.0)
                .label("Last worked on")
                .open(s.home.open_select == Some("recency"))
                .on_toggle(|s: &mut State| {
                    s.home.open_select = if s.home.open_select == Some("recency") { None } else { Some("recency") }
                })
                .on_close(|s: &mut State| s.home.open_select = None)
                .on_choose(|s: &mut State, v: &str, _| {
                    if let Some(r) = Recency::ALL.into_iter().find(|r| r.label() == v) {
                        s.home.recency = r;
                    }
                    s.home.open_select = None;
                })
                .build(k, layer)
        } else {
            SegmentedControl::new(&labels, rec)
                .label("Last worked on")
                .fit()
                .on_pick(|s: &mut State, i, _| s.home.recency = Recency::ALL[i])
                .build(k)
        })
        .child(
            Select::new("stack-filter", &stacks)
                .current(Some(&stack_now))
                .width(if narrow { 120.0 } else { 150.0 })
                .label("Stack")
                .open(s.home.open_select == Some("stack"))
                .on_toggle(|s: &mut State| {
                    s.home.open_select = if s.home.open_select == Some("stack") { None } else { Some("stack") }
                })
                .on_close(|s: &mut State| s.home.open_select = None)
                .on_choose(|s: &mut State, v: &str, _| {
                    s.home.stack = if v == "All stacks" { None } else { Some(v.to_string()) };
                    s.home.open_select = None;
                })
                .build(k, layer),
        )
        .child(el().grow(1.0))
        .children((archived > 0).then(|| {
            Button::new(format!("Archived {archived}"))
                .variant(if s.home.show_archived { ButtonVariant::Neutral } else { ButtonVariant::GhostMuted })
                .icon(&icons::ARCHIVE)
                .on_press(|s: &mut State| s.home.show_archived = !s.home.show_archived)
                .build(k)
                .key("show-archived")
        }))
        .children((!narrow).then(|| label("Sort", 12.0, 400.0, ink.faint)))
        .child(
            Select::new("sort", &orders)
                .current(Some(s.saved.order.label()))
                .width(96.0)
                .label("Sort by")
                .open(s.home.open_select == Some("sort"))
                .on_toggle(|s: &mut State| {
                    s.home.open_select = if s.home.open_select == Some("sort") { None } else { Some("sort") }
                })
                .on_close(|s: &mut State| s.home.open_select = None)
                .on_choose(|s: &mut State, v: &str, _| {
                    if let Some(o) = Order::ALL.into_iter().find(|o| o.label() == v) {
                        s.saved.set_order(o);
                    }
                    s.home.open_select = None;
                })
                .build(k, layer),
        )
        .child(layout_toggle(s, ink))
}

fn empty_state(s: &State, ink: &Ink, k: &Kit) -> El {
    let (title, line, action): (String, String, Option<El>) = if s.scanning && s.projects.is_empty() {
        ("Looking for projects".into(), format!("Reading {}", s.root), None)
    } else if s.projects.is_empty() {
        (
            "No projects here yet".into(),
            format!("{} has no folders. Start one, or point the app at another folder in Settings.", s.root),
            Some(
                shell::primary("New project", &icons::PLUS)
                    .on_press(|s: &mut State| s.newp.open(&s.saved.root))
                    .build(k),
            ),
        )
    } else {
        let q = s.home.search.text.trim();
        (
            if q.is_empty() { "Nothing matches these filters".into() } else { format!("Nothing matches \u{201c}{q}\u{201d}") },
            "Names, stacks and descriptions are searched.".into(),
            Some(
                Button::new("Clear filters")
                    .variant(ButtonVariant::Neutral)
                    .control_size(ControlSize::Large)
                    .on_press(|s: &mut State| s.home.clear_filters())
                    .build(k),
            ),
        )
    };
    col()
        .key("home-empty")
        .w_full()
        .pt(96.0)
        .gap(S3)
        .items_center()
        .children((s.scanning && s.projects.is_empty()).then(|| Spinner::new().build(k)))
        .child(label(title, 15.0, 500.0, ink.fg))
        .child(label(line, 13.0, 400.0, ink.muted).align(Align::Center).max_w(420.0).wrap())
        .children(action.map(|a| a.mt(S2)))
}

fn section_view(s: &State, ink: &Ink, sec: &Section) -> El {
    let cols = columns(s);
    let w = s.home.grid_w.max(CARD_MIN);
    let card_w = ((w - GAP * (cols as f32 - 1.0)) / cols as f32).floor();
    let wide = w > 760.0;
    let body: El = if s.saved.layout == Layout::List {
        col()
            .w_full()
            .gap(2.0)
            .children(sec.items.iter().map(|&i| list_row(s, ink, &s.projects[i], wide)))
    } else {
        col().w_full().gap(GAP).children(sec.items.chunks(cols).enumerate().map(|(r, chunk)| {
            row()
                .key(format!("{}/row{r}", sec.key))
                .gap(GAP)
                .children(chunk.iter().map(|&i| card(s, ink, &s.projects[i], card_w)))
        }))
    };
    col()
        .key(format!("section/{}", sec.key))
        .w_full()
        .gap(S3)
        .child(
            row()
                .gap(S2)
                .items_center()
                .child(label(sec.title.clone(), 13.0, 500.0, ink.muted))
                .child(data(sec.items.len().to_string(), 11.0, ink.faint)),
        )
        .child(body)
}

pub fn view(s: &State, ink: &Ink, k: &Kit, layer: &mut Vec<El>) -> El {
    let live: Vec<&Project> = s.projects.iter().filter(|p| !s.saved.archived.contains(&p.name)).collect();
    let changed = live.iter().filter(|p| dirty(s, p) > 0).count();
    let total: u64 = live.iter().map(|p| p.bytes).sum();
    let meta = if live.is_empty() {
        None
    } else {
        let mut m = fmt::plural(live.len(), "project", "projects");
        if !s.home.detail {
            m.push_str(&format!(" · {}", model::gb(total)));
        }
        if changed > 0 && !s.home.detail {
            m.push_str(&format!(" · {changed} with changes"));
        }
        Some(m)
    };
    let header = shell::header(
        "Home",
        meta,
        vec![
            search(s, ink, k),
            shell::primary("New project", &icons::PLUS)
                .on_press(|s: &mut State| s.newp.open(&s.saved.root))
                .help("New project · Ctrl+N")
                .build(k)
                .key("new-project"),
        ],
        ink,
    );
    let secs = sections(s);
    let content: El = if secs.iter().all(|sec| sec.items.is_empty()) {
        empty_state(s, ink, k)
    } else {
        col().w_full().gap(S6 + S2).children(secs.iter().map(|sec| section_view(s, ink, sec)))
    };
    let scroll = col()
        .key("home-scroll")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .scroll(ScrollSpec { stick_bottom: false })
        .scroll_edge_fade(16.0)
        .child(
            col()
                .key("home-body")
                .w_full()
                .px(S6)
                .pt(S2)
                .pb(S6)
                .on_layout(|s: &mut State, r: [f32; 4]| {
                    let w = (r[2] - 2.0 * S6).max(0.0);
                    if (w - s.home.grid_w).abs() > 0.5 {
                        s.home.grid_w = w;
                    }
                })
                .child(content),
        );
    let main = col()
        .key("home-main")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .child(header)
        .child(toolbar(s, ink, k, layer))
        .child(scroll);
    let panel = s
        .home
        .selected
        .as_ref()
        .filter(|_| s.home.detail)
        .and_then(|n| s.projects.iter().find(|p| &p.name == n))
        .map(|p| crate::detail::panel(s, ink, k, p, layer));
    row()
        .key("home")
        .w_full()
        .h_full()
        .child(main)
        .children(panel)
        .child(context(s, k, layer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recency_chips_bucket_by_days() {
        assert!(Recency::Today.admits(Some(0)));
        assert!(!Recency::Today.admits(Some(1)));
        assert!(Recency::Week.admits(Some(6)));
        assert!(Recency::Month.admits(Some(29)));
        assert!(Recency::Older.admits(Some(30)));
        assert!(Recency::Older.admits(None));
        assert!(!Recency::Week.admits(None));
    }

    #[test]
    fn pinned_come_first_then_recent() {
        let mut s = State::new();
        s.load_demo();
        let last = s.projects.iter().max_by_key(|p| p.days.unwrap_or(u64::MAX)).unwrap().name.clone();
        s.saved.toggle_pin(&last);
        let secs = sections(&s);
        assert_eq!(secs[0].key, "pinned");
        assert_eq!(s.projects[secs[0].items[0]].name, last);
        // The pinned one isn't listed twice.
        assert!(secs.iter().skip(1).all(|sec| sec.items.iter().all(|&i| s.projects[i].name != last)));
    }

    #[test]
    fn archived_projects_leave_home_until_asked_for() {
        let mut s = State::new();
        s.load_demo();
        let name = s.projects[0].name.clone();
        s.saved.toggle_archived(&name);
        assert!(visible(&s).iter().all(|&i| s.projects[i].name != name));
        s.home.show_archived = true;
        assert_eq!(visible(&s).len(), 1);
    }

    #[test]
    fn search_matches_names_and_stacks() {
        let mut s = State::new();
        s.load_demo();
        s.home.search.replace_text("rust");
        let found = visible(&s);
        assert!(!found.is_empty());
        assert!(found.iter().all(|&i| {
            let p = &s.projects[i];
            p.name.to_lowercase().contains("rust") || p.stack.to_lowercase().contains("rust")
        }));
    }
}
