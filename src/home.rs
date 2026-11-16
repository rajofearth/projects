//! The projects page: every folder under the root, most recently worked on first, with a
//! search, recency chips, pins and a right-click menu of the things you do with a project.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rune::prefs::Prefs;
use rune::{col, el, row, text, text_input, Align, Color, Cursor, El, PopupItem, PopupMenu, ScrollSpec, TextEdit, Tx};

use crate::cats;
use crate::launch;
use crate::model::{self, Project};
use crate::nav::Page;
use crate::ui::{ch, chip, dots, mono, section_label, t, tb, Ink, FS};
use crate::State;

const PREFS_ID: &str = "projects";
const PINS_KEY: &str = "pinned";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recency {
    Today,
    Week,
    Month,
    Older,
    All,
}

impl Recency {
    pub const ALL: [Recency; 5] = [Recency::Today, Recency::Week, Recency::Month, Recency::Older, Recency::All];

    pub fn label(self) -> &'static str {
        match self {
            Recency::Today => "today",
            Recency::Week => "this week",
            Recency::Month => "this month",
            Recency::Older => "older",
            Recency::All => "all",
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
    pub pinned: HashSet<String>,
    /// The highlighted project, by name, so a rescan reordering rows keeps the pick.
    pub selected: Option<String>,
    /// Current branch per project, read from `.git/HEAD`.
    pub branches: HashMap<String, String>,
    pub new_open: bool,
    pub new_name: TextEdit,
    pub git_init: bool,
    pub note: Option<String>,
    prefs: Prefs,
}

impl Home {
    pub fn new() -> Self {
        let prefs = if cfg!(test) { Prefs::memory() } else { Prefs::open(PREFS_ID) };
        let pinned = prefs.get::<Vec<String>>(PINS_KEY).unwrap_or_default().into_iter().collect();
        Home {
            search: TextEdit::new(""),
            recency: Recency::All,
            pinned,
            selected: None,
            branches: HashMap::new(),
            new_open: false,
            new_name: TextEdit::new(""),
            git_init: true,
            note: None,
            prefs,
        }
    }

    pub fn toggle_pin(&mut self, name: &str) {
        if !self.pinned.remove(name) {
            self.pinned.insert(name.to_string());
        }
        let mut list: Vec<&String> = self.pinned.iter().collect();
        list.sort();
        if let Err(e) = self.prefs.set(PINS_KEY, &list) {
            self.note = Some(format!("could not save pins: {e}"));
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

    /// The rows on screen: matching the search and the chip, pinned first, then most recent.
    pub fn visible<'a>(&self, projects: &'a [Project]) -> Vec<&'a Project> {
        let needle = self.search.text.trim().to_lowercase();
        let mut rows: Vec<&Project> = projects
            .iter()
            .filter(|p| needle.is_empty() || p.name.to_lowercase().contains(&needle))
            .filter(|p| self.recency.admits(p.days))
            .collect();
        rows.sort_by(|a, b| {
            let pa = !self.pinned.contains(&a.name);
            let pb = !self.pinned.contains(&b.name);
            pa.cmp(&pb)
                .then(a.days.unwrap_or(u64::MAX).cmp(&b.days.unwrap_or(u64::MAX)))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        rows
    }
}

// ---- actions ---------------------------------------------------------------

fn path_of(s: &State, name: &str) -> PathBuf {
    PathBuf::from(&s.root).join(name)
}

fn report(s: &mut State, what: &str, r: std::io::Result<()>) {
    s.home.note = Some(match r {
        Ok(()) => what.to_string(),
        Err(e) => format!("{what} failed: {e}"),
    });
}

/// Opens a project in the first editor found, the double-click action.
pub fn open_default(s: &mut State, name: &str) {
    let path = path_of(s, name);
    match s.apps.first() {
        Some(app) => {
            let (id, label) = (app.id, app.name);
            report(s, &format!("opened {name} in {label}"), launch::open_in(id, &path));
        }
        None => report(s, &format!("revealed {name}"), crate::browse::reveal(&path)),
    }
}

/// Shows the project in the storage page, highlighted in the full list.
pub fn show_in_storage(s: &mut State, name: &str) {
    s.page = Page::Storage;
    s.filter_to(crate::store::Filter::All);
    if let Some(i) = s.ui.entries(&s.projects).iter().position(|it| it.name == name) {
        s.ui.set_row(i);
    }
}

fn context_menu(s: &State, name: &str) -> PopupMenu {
    let mut open_in = PopupMenu::new();
    for app in &s.apps {
        open_in = open_in.item(PopupItem::new(format!("app:{}", app.id), app.name));
    }
    let pinned = s.home.pinned.contains(name);
    PopupMenu::new()
        .submenu_enabled("Open in", open_in, !s.apps.is_empty())
        .item(PopupItem::new("terminal", "Open terminal"))
        .item(PopupItem::new("reveal", "Reveal in Explorer"))
        .separator()
        .item(PopupItem::new("copy", "Copy path"))
        .item(PopupItem::new("pin", if pinned { "Unpin" } else { "Pin" }))
        .separator()
        .item(PopupItem::new("storage", "Show in Storage"))
}

fn pick(s: &mut State, name: &str, id: &str, tx: &mut Tx) {
    let path = path_of(s, name);
    match id {
        "terminal" => report(s, &format!("terminal at {name}"), launch::open_terminal(&path)),
        "reveal" => report(s, &format!("revealed {name}"), crate::browse::reveal(&path)),
        "copy" => {
            tx.copy(path.display().to_string());
            s.home.note = Some(format!("copied {}", path.display()));
        }
        "pin" => s.home.toggle_pin(name),
        "storage" => show_in_storage(s, name),
        other => {
            if let Some(app) = other.strip_prefix("app:") {
                let label = s.apps.iter().find(|a| a.id == app).map(|a| a.name).unwrap_or(app);
                let label = label.to_string();
                report(s, &format!("opened {name} in {label}"), launch::open_in(app, &path));
            }
        }
    }
}

fn create(s: &mut State) {
    let name = s.home.new_name.text.trim().to_string();
    if name.is_empty() {
        s.home.note = Some("give the project a name".into());
        return;
    }
    let root = PathBuf::from(&s.root);
    match launch::create_project(&root, &name, s.home.git_init) {
        Ok(path) => {
            s.home.note = Some(format!("created {}", path.display()));
            s.home.new_open = false;
            s.home.new_name = TextEdit::new("");
            s.home.selected = Some(name);
            s.request_scan();
        }
        Err(e) => s.home.note = Some(format!("could not create {name}: {e}")),
    }
}

// ---- view ------------------------------------------------------------------

const AGE_W: f32 = FS * 9.0;
const BRANCH_W: f32 = FS * 11.0;
const SIZE_W: f32 = FS * 8.0;
const DOTS: usize = 12;

fn field(edit: &TextEdit, key: &str, hint: &str, ink: &Ink, width: f32) -> El {
    text_input(edit, mono(FS, 400.0))
        .single_line()
        .key(key.to_string())
        .placeholder(hint, ink.faint)
        .caret(ink.fg, ink.picked)
        .color(ink.fg)
        .w(width)
        .px(ch(0.8))
        .py(4.0)
        .radius(7.0)
        .border(1.0, crate::ui::tint([230, 230, 230], 0.32))
}

fn header(s: &State, ink: &Ink) -> El {
    row()
        .key("home-head")
        .w_full()
        .items_center()
        .gap(ch(1.0))
        .child(
            field(&s.home.search, "search", "search projects  /", ink, ch(32.0))
                .on_edit(|s: &mut State| &mut s.home.search),
        )
        .children(Recency::ALL.into_iter().map(|r| {
            let on = s.home.recency == r;
            let mut c = chip(r.label(), if on { ink.fg } else { ink.muted }, [230, 230, 230], move |s: &mut State| {
                s.home.recency = r
            });
            if on {
                c = c.fill(ink.picked);
            }
            c.key(format!("recency/{}", r.label()))
        }))
        .child(el().grow(1.0))
        .child(
            chip("New project", ink.fg, [74, 222, 128], |s: &mut State| {
                s.home.new_open = !s.home.new_open;
            })
            .key("new-project"),
        )
}

fn new_form(s: &State, ink: &Ink) -> El {
    let git = s.home.git_init;
    row()
        .key("new-form")
        .w_full()
        .items_center()
        .gap(ch(1.0))
        .pt(ch(0.4))
        .child(t("new project in", ink.faint))
        .child(t(crate::ui::clip_path(&s.root, 30), ink.muted))
        .child(
            field(&s.home.new_name, "new-name", "name", ink, ch(28.0))
                .on_edit(|s: &mut State| &mut s.home.new_name)
                .on_submit(|s: &mut State, _| create(s)),
        )
        .child(chip(
            if git { "[x] git init" } else { "[ ] git init" },
            if git { ink.fg } else { ink.muted },
            [230, 230, 230],
            |s: &mut State| s.home.git_init = !s.home.git_init,
        ))
        .child(chip("Create", ink.green, [74, 222, 128], create))
        .child(chip("Cancel", ink.muted, [230, 230, 230], |s: &mut State| s.home.new_open = false))
}

fn columns(ink: &Ink) -> El {
    row()
        .w_full()
        .items_center()
        .gap(ch(1.5))
        .px(ch(0.5))
        .pt(ch(0.7))
        .pb(ch(0.15))
        .child(el().size(ch(1.0), 0.0))
        .child(t("project", ink.faint).grow(1.0).min_w(0.0))
        .child(t("last worked on", ink.faint).w(AGE_W).align(Align::End))
        .child(t("branch", ink.faint).w(BRANCH_W))
        .child(t("size", ink.faint).w(SIZE_W).align(Align::End))
        .child(el().size(crate::ui::dots_w(DOTS), 0.0))
}

fn age_words(days: Option<u64>) -> String {
    match days {
        None => "no commits".into(),
        Some(0) => "today".into(),
        Some(1) => "yesterday".into(),
        Some(d) => format!("{} ago", cats::short_age(d)),
    }
}

fn project_line(s: &State, ink: &Ink, p: &Project, index: usize, largest: u64) -> El {
    let selected = s.home.selected.as_deref() == Some(p.name.as_str());
    let pinned = s.home.pinned.contains(&p.name);
    let recent = p.days.is_some_and(|d| d < 7);
    let name = p.name.clone();
    let (n1, n2, n3) = (name.clone(), name.clone(), name.clone());
    row()
        .key(format!("proj/{index}"))
        .w_full()
        .h(FS * 2.0)
        .px(ch(0.5))
        .radius(4.0)
        .items_center()
        .gap(ch(1.5))
        .cursor(Cursor::Pointer)
        .fill(if selected { ink.picked } else { Color::TRANSPARENT })
        .hover(ink.hover)
        .on_press(move |s: &mut State| s.home.selected = Some(n1.clone()))
        .on_double_press(move |s: &mut State| open_default(s, &n2))
        .on_secondary_press(move |s: &mut State, at, tx| {
            s.home.selected = Some(n3.clone());
            let menu = context_menu(s, &n3);
            let target = n3.clone();
            tx.popup_menu(menu, at, move |s: &mut State, picked, tx| {
                if let Some(id) = picked {
                    pick(s, &target, id, tx);
                }
            });
        })
        .child(t(if pinned { "*" } else { "" }, ink.amber).w(ch(1.0)))
        .child(
            text(name)
                .font(mono(FS, if selected { 700.0 } else { 400.0 }))
                .color(if selected { ink.fg } else { ink.row })
                .grow(1.0)
                .min_w(0.0)
                .truncate(),
        )
        .child(
            t(age_words(p.days), if recent { ink.green } else if selected { ink.muted } else { ink.faint })
                .w(AGE_W)
                .align(Align::End),
        )
        .child(
            t(s.home.branches.get(&p.name).cloned().unwrap_or_default(), ink.muted)
                .w(BRANCH_W)
                .truncate(),
        )
        .child(
            t(model::gb(p.bytes), if selected { ink.fg } else { ink.muted })
                .w(SIZE_W)
                .align(Align::End),
        )
        .child(dots(p.bytes as f32 / largest.max(1) as f32, [230, 230, 230], DOTS, index + 5, !selected))
        .help(format!("{} · {}", p.name, if p.stack.is_empty() { p.cat } else { p.stack.as_str() }))
}

pub fn view(s: &State, ink: &Ink) -> El {
    let rows = s.home.visible(&s.projects);
    let largest = rows.iter().map(|p| p.bytes).max().unwrap_or(0);
    let pinned = rows.iter().filter(|p| s.home.pinned.contains(&p.name)).count();
    let summary = format!(
        "{} of {} projects{}",
        rows.len(),
        s.projects.len(),
        if pinned > 0 { format!(" · {pinned} pinned") } else { String::new() }
    );
    let list: El = if rows.is_empty() {
        t(
            if s.projects.is_empty() && s.scanning { "still looking…" } else { "no projects match" },
            ink.faint,
        )
        .pt(ch(0.6))
        .pl(ch(2.0))
    } else {
        col()
            .key("proj-scroll")
            .grow(1.0)
            .min_h(0.0)
            .scroll(ScrollSpec { stick_bottom: false })
            .scroll_edge_fade(26.0)
            .child(
                col()
                    .key("proj-rows")
                    .w_full()
                    .gap(ch(0.15))
                    .children(rows.iter().enumerate().map(|(i, p)| project_line(s, ink, p, i, largest))),
            )
    };
    col()
        .key("home")
        .w_full()
        .gap(ch(0.3))
        .child(header(s, ink))
        .children(s.home.new_open.then(|| new_form(s, ink)))
        .child(
            row()
                .w_full()
                .items_center()
                .pt(ch(0.6))
                .child(section_label(&summary, ink))
                .child(el().grow(1.0))
                .child(t("double-click opens · right-click for more", ink.faint)),
        )
        .child(columns(ink))
        .child(list)
        .child(el().grow(0.0001))
        .child(
            row()
                .key("home-bottom")
                .w_full()
                .items_center()
                .children(s.home.note.clone().map(|n| t(n, ink.muted).truncate()))
                .child(el().grow(1.0))
                .children(s.home.selected.clone().map(|n| tb(n, ink.faint))),
        )
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
        let mut h = Home::new();
        let mut ps = crate::fixture::demo_projects();
        ps.truncate(4);
        ps[3].days = Some(999);
        let last = ps[3].name.clone();
        h.pinned.insert(last.clone());
        assert_eq!(h.visible(&ps)[0].name, last);
    }
}
