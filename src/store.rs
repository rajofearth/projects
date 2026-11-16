//! What the window is showing: the category under the pointer, the row under the pointer, the
//! folders opened inside a project, and what the workspace measured for each one.
//!
//! Ported from the upstream interface's `store.ts` (MIT), with the keyboard-driven parts
//! replaced by pointer actions: every move here is what a click does, not a keystroke.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use crate::browse::{self, BrowseEvent, Culprit, Item};
use crate::cats::{self, Cat};
use crate::model::Project;

/// Longest list the detail pane draws.
pub const MAX_ROWS: usize = 150;

/// How the detail pane orders its rows. Clicking a column header sets one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Size,
    Name,
    Age,
    /// Not a column: a heading that names a cell nothing sorts by.
    None,
}

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Sort::Size => "size",
            Sort::Name => "name",
            Sort::Age => "last commit",
            Sort::None => "",
        }
    }
}
/// How deep the drill-down goes before it stops opening folders.
const MAX_DEPTH: usize = 12;

pub struct Ui {
    /// The category the detail pane shows.
    pub selected: Cat,
    /// The category under the pointer, in the meter or the category column.
    pub hover: Option<Cat>,
    /// The highlighted row in the detail pane.
    pub row: usize,
    /// Folders opened inside the category, outermost first.
    pub stack: Vec<Item>,
    /// Contents of the innermost opened folder.
    pub children: Vec<Item>,
    /// True while the opened folder is still being measured.
    pub listing: bool,
    /// Where the space inside each measured folder is; `None` while it is being measured.
    pub culprits: HashMap<PathBuf, Option<Culprit>>,
    /// One line about the last action.
    pub note: Option<String>,
    /// Show only the rows labelled junk or unused.
    pub junk_only: bool,
    /// The detail pane's order, and whether it runs largest/newest first.
    pub sort: Sort,
    /// True when the sort runs the other way: smallest, Z to A, oldest first.
    pub sort_down: bool,
    root: PathBuf,
    /// The listing in flight, if any.
    rx: Option<Receiver<BrowseEvent>>,
    cancel: Option<browse::Cancel>,
    /// Culprit searches in flight, one channel each.
    culprit_rx: Vec<Receiver<BrowseEvent>>,
    culprit_cancels: Vec<browse::Cancel>,
}

impl Ui {
    pub fn new(root: PathBuf) -> Self {
        Self {
            selected: Cat::Source,
            hover: None,
            row: 0,
            stack: Vec::new(),
            children: Vec::new(),
            listing: false,
            culprits: HashMap::new(),
            note: None,
            junk_only: false,
            sort: Sort::Size,
            sort_down: true,
            root,
            rx: None,
            cancel: None,
            culprit_rx: Vec::new(),
            culprit_cancels: Vec::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Bytes in each category across every project, in `CATEGORIES` order.
    pub fn category_sizes(&self, projects: &[Project]) -> [u64; 4] {
        let mut sizes = [0u64; 4];
        for p in projects {
            for (i, c) in cats::CATEGORIES.iter().enumerate() {
                sizes[i] += c.id.bytes(p);
            }
        }
        sizes
    }

    pub fn total(&self, projects: &[Project]) -> u64 {
        projects.iter().map(|p| p.bytes).sum()
    }

    /// How much of a row belongs to the category being viewed. At the top level that is the
    /// project's slice of it; inside a folder it is the folder's own size.
    pub fn share(&self, item: &Item, projects: &[Project]) -> u64 {
        if self.stack.is_empty() {
            return match self.project(&item.name, projects) {
                Some(p) => self.selected.bytes(p),
                None => item.size,
            };
        }
        item.size
    }

    /// The project a top-level row names, if any.
    fn project<'a>(&self, name: &str, projects: &'a [Project]) -> Option<&'a Project> {
        projects.iter().find(|p| p.name == name)
    }

    /// Everything the detail pane lists right now, largest first.
    pub fn all_entries(&self, projects: &[Project]) -> Vec<Item> {
        let mut list: Vec<Item> = if self.stack.is_empty() {
            // A row is the whole project, not just the slice being viewed: one row's numbers
            // always describe that folder, and `share` says how much of it this category is.
            let mut rows: Vec<Item> = projects
                .iter()
                .map(|p| Item::folder(self.root.join(&p.name), p.name.clone(), p.bytes))
                .filter(|i| self.share(i, projects) > 0)
                .collect();
            rows.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
            rows
        } else {
            self.children.clone()
        };
        if self.junk_only {
            list.retain(|item| {
                matches!(
                    self.label(item, projects).verdict,
                    cats::Verdict::Junk | cats::Verdict::Unused
                )
            });
        }
        let down = self.sort_down;
        match self.sort {
            Sort::Size => {
                list.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)))
            }
            Sort::Name => {
                list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            }
            // Rows without a commit date sort last, whichever way the column runs.
            Sort::None => {}
            Sort::Age => list.sort_by(|a, b| {
                let key = |item: &Item| self.age_of(item, projects);
                match (key(a), key(b)) {
                    (Some(x), Some(y)) => x.cmp(&y).then_with(|| b.size.cmp(&a.size)),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => b.size.cmp(&a.size),
                }
            }),
        }
        if !down {
            list.reverse();
            // "No date" still belongs at the bottom, not the top.
            if self.sort == Sort::Age {
                list.sort_by_key(|item| self.age_of(item, projects).is_none());
            }
        }
        list
    }

    /// Days since the row's project last saw a commit, when git knows.
    pub fn age_of(&self, item: &Item, projects: &[Project]) -> Option<u64> {
        let name = if self.stack.is_empty() {
            item.name.as_str()
        } else {
            self.stack[0].name.as_str()
        };
        projects.iter().find(|p| p.name == name).and_then(|p| p.days)
    }

    /// A column header was clicked: sort by it, or flip it when it is already the order.
    pub fn set_sort(&mut self, sort: Sort) {
        if self.sort == sort {
            self.sort_down = !self.sort_down;
        } else {
            self.sort = sort;
            // Size and date read best biggest-first; names read best A to Z.
            self.sort_down = sort != Sort::Name;
        }
        self.row = 0;
    }

    /// The rows drawn, capped so a folder with a thousand files stays responsive.
    pub fn entries(&self, projects: &[Project]) -> Vec<Item> {
        let mut list = self.all_entries(projects);
        list.truncate(MAX_ROWS);
        list
    }

    /// What a row is, and why.
    pub fn label(&self, item: &Item, projects: &[Project]) -> cats::Label {
        if self.stack.is_empty() {
            return match self.project(&item.name, projects) {
                Some(p) => cats::classify_project(p, self.selected),
                None => cats::classify(&item.name, item.dir, None),
            };
        }
        // Inside a project, a folder's age is the project's: git knows when work last happened.
        let days = self
            .project(&self.stack[0].name, projects)
            .and_then(|p| p.days);
        cats::classify(&item.name, item.dir, days)
    }

    /// Where the space inside a row is. Top-level rows use the scan's own buckets, so the
    /// answer is there before any folder is measured.
    pub fn culprit(&self, item: &Item, projects: &[Project]) -> Option<Culprit> {
        if self.stack.is_empty() {
            return self
                .project(&item.name, projects)
                .map(project_culprit)
                .filter(|c| !c.chain.is_empty() || !c.top.is_empty());
        }
        self.culprits.get(&item.path).cloned().flatten()
    }

    /// True while a row's culprit is still being measured.
    pub fn culprit_pending(&self, item: &Item) -> bool {
        !self.stack.is_empty() && self.culprits.get(&item.path).is_some_and(|c| c.is_none())
    }

    pub fn scope_size(&self, projects: &[Project]) -> u64 {
        match self.stack.last() {
            Some(top) => top.size,
            None => self.category_sizes(projects)[cat_index(self.selected)],
        }
    }

    /// The whole disk: every byte of every project, the denominator the dots share.
    pub fn disk_size(&self, projects: &[Project]) -> u64 {
        self.total(projects)
    }

    /// The four-way split of one project, for the hover readout.
    pub fn split(&self, item: &Item, projects: &[Project]) -> Option<[(Cat, u64); 4]> {
        let p = self.project(&item.name, projects)?;
        Some(cats::CATEGORIES.map(|c| (c.id, c.id.bytes(p))))
    }

    /// How much of what is listed is labelled junk, and how much is probably unused.
    pub fn reclaimable(&self, projects: &[Project]) -> (u64, u64) {
        let mut junk = 0;
        let mut unused = 0;
        for item in self.all_entries(projects) {
            match self.label(&item, projects).verdict {
                cats::Verdict::Junk => junk += item.size,
                cats::Verdict::Unused => unused += item.size,
                _ => {}
            }
        }
        (junk, unused)
    }

    pub fn set_hover(&mut self, hover: Option<Cat>) {
        self.hover = hover;
    }

    pub fn select(&mut self, cat: Cat) {
        if cat == self.selected && self.stack.is_empty() {
            return;
        }
        self.cancel_work();
        self.culprits.clear();
        self.selected = cat;
        self.stack.clear();
        self.children.clear();
        self.listing = false;
        self.row = 0;
        self.note = None;
    }

    pub fn set_row(&mut self, row: usize) {
        self.row = row;
    }

    pub fn toggle_junk_only(&mut self) {
        self.junk_only = !self.junk_only;
        self.row = 0;
    }

    /// Opens the row's folder and measures what is inside.
    pub fn open(&mut self, projects: &[Project], demo: bool) {
        let Some(item) = self.entries(projects).into_iter().nth(self.row) else {
            return;
        };
        if !item.dir || self.stack.len() >= MAX_DEPTH {
            return;
        }
        self.push_level(item, projects, demo, None);
    }

    fn push_level(
        &mut self,
        item: Item,
        projects: &[Project],
        demo: bool,
        land_on: Option<&Path>,
    ) {
        self.cancel_work();
        self.culprits.clear();
        self.note = None;
        self.stack.push(item.clone());
        self.row = 0;
        if demo {
            let depth = self.stack.len();
            self.children = demo_children(&item, depth);
            self.listing = false;
            self.row = land_on
                .and_then(|p| self.children.iter().position(|c| c.path == p))
                .unwrap_or(0);
            return;
        }
        self.children.clear();
        self.listing = true;
        self.spawn_listing(item.path, land_on.map(|p| p.to_path_buf()));
        let _ = projects;
    }

    fn spawn_listing(&mut self, path: PathBuf, land_on: Option<PathBuf>) {
        let (tx, rx) = channel();
        let cancel = browse::cancel_flag();
        self.rx = Some(rx);
        self.cancel = Some(cancel.clone());
        browse::list_folder(path.clone(), tx, cancel);
        // The culprit search runs beside the listing: its line is the one the reader wants.
        self.want_culprit_at(path);
        let _ = land_on;
    }

    fn want_culprit_at(&mut self, path: PathBuf) {
        if self.culprits.contains_key(&path) {
            return;
        }
        self.culprits.insert(path.clone(), None);
        let (tx, rx) = channel();
        let cancel = browse::cancel_flag();
        self.culprit_rx.push(rx);
        self.culprit_cancels.push(cancel.clone());
        browse::find_culprit(path, tx, cancel);
    }

    /// Goes up one folder; from a category's top level, nothing happens.
    pub fn back(&mut self, projects: &[Project], demo: bool) {
        if self.stack.is_empty() {
            return;
        }
        self.cancel_work();
        self.culprits.clear();
        self.note = None;
        let left = self.stack.pop();
        match self.stack.last().cloned() {
            None => {
                self.children.clear();
                self.listing = false;
                self.row = left
                    .and_then(|left| {
                        self.all_entries(projects)
                            .iter()
                            .position(|i| i.name == left.name)
                    })
                    .unwrap_or(0);
            }
            Some(parent) => {
                self.children.clear();
                self.row = 0;
                if demo {
                    let depth = self.stack.len();
                    self.children = demo_children(&parent, depth);
                    self.listing = false;
                } else {
                    self.listing = true;
                    self.spawn_listing(parent.path, None);
                }
            }
        }
    }

    /// Jumps back to a level in the breadcrumb: 0 is the category itself.
    pub fn jump(&mut self, level: usize, _projects: &[Project], demo: bool) {
        // 0 is the category root, which is always reachable; the level already showing is not.
        if level >= self.stack.len() {
            return;
        }
        self.cancel_work();
        self.culprits.clear();
        self.note = None;
        self.stack.truncate(level);
        self.row = 0;
        match self.stack.last().cloned() {
            None => {
                self.children.clear();
                self.listing = false;
            }
            Some(parent) => {
                self.children.clear();
                if demo {
                    let depth = self.stack.len();
                    self.children = demo_children(&parent, depth);
                    self.listing = false;
                } else {
                    self.listing = true;
                    self.spawn_listing(parent.path, None);
                }
            }
        }
    }

    /// Jumps to the folder that holds the row's culprit.
    pub fn go_to_culprit(&mut self, projects: &[Project], demo: bool) {
        let Some(item) = self.entries(projects).into_iter().nth(self.row) else {
            return;
        };
        let Some(culprit) = self.culprit(&item, projects) else {
            return;
        };
        match culprit.chain.len() {
            0 => {}
            1 => {
                let target = culprit.chain[0].clone();
                self.push_level(item, projects, demo, None);
                self.row = self
                    .children
                    .iter()
                    .position(|c| c.path == target.path)
                    .unwrap_or(0);
            }
            n => {
                // Open every folder on the way down; land on the one holding the culprit.
                let holder = culprit.chain[n - 2].clone();
                let last = culprit.chain[n - 1].path.clone();
                self.push_level(item, projects, demo, None);
                self.push_level(holder, projects, demo, Some(&last));
            }
        }
    }

    /// Reads what the workers sent. True when anything arrived.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Some(rx) = self.rx.as_ref() {
            match rx.try_recv() {
                Ok(BrowseEvent::Listing { path, items, done }) => {
                    if self.stack.last().is_none_or(|top| top.path != path) {
                        continue;
                    }
                    self.children = items;
                    self.listing = !done;
                    self.row = self.row.min(self.children.len().saturating_sub(1));
                    changed = true;
                    if done {
                        self.rx = None;
                        self.cancel = None;
                        break;
                    }
                }
                Ok(BrowseEvent::Culprit { path, culprit }) => {
                    self.culprits.insert(path, Some(culprit));
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.rx = None;
                    self.cancel = None;
                    self.listing = false;
                    changed = true;
                    break;
                }
            }
        }
        let mut i = 0;
        while i < self.culprit_rx.len() {
            let finished = match self.culprit_rx[i].try_recv() {
                Ok(BrowseEvent::Culprit { path, culprit }) => {
                    self.culprits.insert(path, Some(culprit));
                    changed = true;
                    true
                }
                Ok(_) => false,
                Err(std::sync::mpsc::TryRecvError::Empty) => false,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => true,
            };
            if finished {
                self.culprit_rx.remove(i);
                if i < self.culprit_cancels.len() {
                    self.culprit_cancels.remove(i);
                }
            } else {
                i += 1;
            }
        }
        changed
    }

    /// Asks for the culprit of the highlighted row, when it is on screen and not measured yet.
    pub fn want_culprit(&mut self, projects: &[Project], demo: bool) {
        if self.stack.is_empty() || self.listing {
            return;
        }
        let Some(item) = self.entries(projects).into_iter().nth(self.row) else {
            return;
        };
        if !item.dir || self.culprits.contains_key(&item.path) {
            return;
        }
        if demo {
            let culprit = demo_culprit(&item);
            self.culprits.insert(item.path, Some(culprit));
            return;
        }
        self.want_culprit_at(item.path);
    }

    pub fn reveal(&mut self, projects: &[Project], demo: bool) {
        let Some(item) = self.entries(projects).into_iter().nth(self.row) else {
            return;
        };
        if demo {
            self.note = Some(format!("Demo: would show {} in Explorer.", item.name));
            return;
        }
        self.note = Some(match browse::reveal(&item.path) {
            Ok(()) => format!("Showing {} in Explorer.", item.name),
            Err(e) => format!("Could not show {}: {e}", item.name),
        });
    }

    /// A new scan started: the old drill-down no longer applies.
    pub fn reset(&mut self) {
        self.cancel_work();
        self.culprits.clear();
        self.stack.clear();
        self.children.clear();
        self.listing = false;
        self.row = 0;
        self.note = None;
    }

    pub fn cancel_work(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        for cancel in self.culprit_cancels.drain(..) {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.rx = None;
        self.culprit_rx.clear();
    }
}

pub fn cat_index(cat: Cat) -> usize {
    cats::CATEGORIES
        .iter()
        .position(|c| c.id == cat)
        .unwrap_or(0)
}

/// The folder name a bucket's bytes keep, for a culprit line measured from the scan alone.
pub fn bucket_name(cat: Cat) -> &'static str {
    match cat {
        Cat::Source => "source/",
        Cat::Deps => "node_modules/",
        Cat::Build => "build output/",
        Cat::Other => ".git/",
    }
}

/// Where a project's space is, straight from the scan: its biggest bucket, when that bucket
/// holds most of the project.
pub fn project_culprit(p: &Project) -> Culprit {
    let mut buckets: Vec<(Cat, u64)> = cats::CATEGORIES
        .iter()
        .map(|c| (c.id, c.id.bytes(p)))
        .filter(|(_, n)| *n > 0)
        .collect();
    buckets.sort_by(|a, b| b.1.cmp(&a.1));
    let top: Vec<Item> = buckets
        .iter()
        .take(3)
        .map(|(cat, size)| Item {
            path: PathBuf::from(bucket_name(*cat)),
            name: bucket_name(*cat).to_string(),
            size: *size,
            dir: true,
        })
        .collect();
    let chain = match top.first() {
        Some(first) if first.size * 2 >= p.bytes.max(1) => vec![first.clone()],
        _ => Vec::new(),
    };
    Culprit {
        chain,
        top,
        root_size: p.bytes,
    }
}

// ---- the demo scene's made-up folders ----------------------------------------

const DEMO_POOL: [&str; 14] = [
    "src",
    "node_modules",
    "target",
    "assets",
    "public",
    "docs",
    "dist",
    ".git",
    "tests",
    "scripts",
    "main.rs",
    "README.md",
    "package.json",
    "bundle.min.js",
];

/// Made-up contents for a demo folder, so the drill-down has something to show.
pub fn demo_children(parent: &Item, depth: usize) -> Vec<Item> {
    let start = (hash(&parent.name) * DEMO_POOL.len() as f64) as usize;
    let count = 4 + (hash(&format!("{}#", parent.name)) * 6.0) as usize;
    let lopsided = hash(&format!("{}!", parent.name)) < 0.6;
    let names: Vec<&str> = (0..count)
        .map(|i| DEMO_POOL[(start + i) % DEMO_POOL.len()])
        .collect();
    let weights: Vec<f64> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            (1.0 / (i as f64 + 1.2)) * (0.6 + hash(name)) * if lopsided && i == 0 { 5.0 } else { 1.0 }
        })
        .collect();
    let sum: f64 = weights.iter().sum();
    let mut items: Vec<Item> = names
        .iter()
        .zip(&weights)
        .map(|(name, w)| Item {
            path: parent.path.join(name),
            name: name.to_string(),
            size: (parent.size as f64 * w / sum.max(1e-9)) as u64,
            dir: depth < 7 && !name.contains('.'),
        })
        .collect();
    items.sort_by(|a, b| b.size.cmp(&a.size));
    items
}

/// Demo stand-in for a culprit search: follows the made-up tree down.
pub fn demo_culprit(item: &Item) -> Culprit {
    let mut chain = Vec::new();
    let mut current = item.clone();
    for depth in 0..6 {
        if !current.dir {
            break;
        }
        let Some(best) = demo_children(&current, depth + 1).into_iter().next() else {
            break;
        };
        if best.size * 2 < current.size {
            break;
        }
        let whole = cats::is_whole(&best.name);
        current = best.clone();
        chain.push(best);
        if whole {
            break;
        }
    }
    let top = demo_children(item, 1).into_iter().take(3).collect();
    Culprit {
        chain,
        top,
        root_size: item.size,
    }
}

fn hash(text: &str) -> f64 {
    let mut h: u32 = 2_166_136_261;
    for b in text.bytes() {
        h = (h ^ b as u32).wrapping_mul(16_777_619);
    }
    h as f64 / 4_294_967_296.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cats::CATEGORIES;

    fn project(name: &str, nm: u64, target: u64, other: u64, src: u64, days: Option<u64>) -> Project {
        Project {
            name: name.into(),
            cat: "Web App",
            stack: String::new(),
            bytes: nm + target + other + src,
            files: 1,
            loc: 1,
            src,
            nm,
            target,
            other,
            langs: Vec::new(),
            git: days.is_some(),
            commits: 0,
            days,
            dirty: 0,
        }
    }

    fn fixture() -> Vec<Project> {
        vec![
            project("heavy-deps", 900, 10, 10, 80, Some(10)),
            project("native", 0, 900, 50, 50, Some(20)),
            project("old", 10, 10, 10, 970, Some(500)),
        ]
    }

    #[test]
    fn a_category_lists_the_projects_by_that_categories_bytes() {
        let projects = fixture();
        let ui = Ui::new(PathBuf::from(r"P:\Projects"));
        let mut ui = ui;
        ui.selected = Cat::Deps;
        let rows = ui.entries(&projects);
        assert_eq!(rows[0].name, "heavy-deps");
        // The row is the whole project; its share is the slice of that category, and the
        // two are different numbers so no row can be misread as the other.
        assert_eq!(rows[0].size, 1000, "the row is the whole folder");
        assert_eq!(ui.share(&rows[0], &projects), 900, "900 of it is dependencies");
        // Projects with none of that category are left out rather than listed at zero.
        assert!(!rows.iter().any(|r| r.name == "native"));
    }

    #[test]
    fn the_culprit_of_a_project_is_its_biggest_bucket() {
        let projects = fixture();
        let ui = Ui::new(PathBuf::from(r"P:\Projects"));
        let item = Item {
            path: PathBuf::from(r"P:\Projects\heavy-deps"),
            name: "heavy-deps".into(),
            size: 1000,
            dir: true,
        };
        let culprit = ui.culprit(&item, &projects).expect("a culprit");
        assert_eq!(culprit.chain[0].name, "node_modules/");
        assert_eq!(culprit.chain[0].size, 900);
        assert_eq!(culprit.top.len(), 3, "the spread lists three buckets");
    }

    #[test]
    fn a_spread_out_project_has_no_culprit_chain() {
        let projects = vec![project("even", 100, 100, 100, 100, Some(1))];
        let ui = Ui::new(PathBuf::from("root"));
        let item = Item {
            path: PathBuf::from("root/even"),
            name: "even".into(),
            size: 400,
            dir: true,
        };
        let culprit = ui.culprit(&item, &projects).expect("tops still known");
        assert!(culprit.chain.is_empty());
        assert_eq!(culprit.top.len(), 3);
    }

    #[test]
    fn the_demo_drill_down_lands_on_a_culprit() {
        let projects = fixture();
        let mut ui = Ui::new(PathBuf::from("root"));
        ui.selected = Cat::Source;
        ui.set_row(0);
        ui.open(&projects, true);
        assert_eq!(ui.stack.len(), 1, "the first row opened");
        assert!(!ui.children.is_empty(), "the folder has made-up contents");
        assert!(!ui.listing, "demo folders are ready at once");
        // The demo culprit follows the same tree down, for a row that is itself a folder.
        let dir_row = ui
            .entries(&projects)
            .iter()
            .position(|i| i.dir)
            .expect("a folder to look into");
        ui.set_row(dir_row);
        ui.want_culprit(&projects, true);
        let item = ui.entries(&projects)[dir_row].clone();
        let culprit = ui.culprit(&item, &projects).expect("demo culprit");
        assert!(
            !culprit.chain.is_empty() || !culprit.top.is_empty(),
            "the demo names where the space went"
        );
    }

    /// A drill-down four folders deep, the way a click path goes.
    fn deep_ui(projects: &[Project]) -> Ui {
        let mut ui = Ui::new(PathBuf::from("root"));
        ui.selected = Cat::Source;
        for _ in 0..4 {
            match ui.entries(projects).iter().position(|i| i.dir) {
                Some(k) => {
                    ui.set_row(k);
                    ui.open(projects, true);
                }
                None => break,
            }
        }
        ui
    }

    #[test]
    fn every_breadcrumb_level_above_the_leaf_jumps_to_it() {
        let projects = fixture();
        let deep = deep_ui(&projects).stack.len();
        assert!(deep >= 2, "the walk went down: {deep}");
        // Each crumb above the leaf is reachable, including the category root at 0.
        for level in 0..deep {
            let mut ui = deep_ui(&projects);
            ui.jump(level, &projects, true);
            assert_eq!(
                ui.stack.len(),
                level,
                "jumping to crumb {level} lands at that level"
            );
        }
        // The level already showing is not a target and changes nothing.
        let mut ui = deep_ui(&projects);
        ui.jump(deep, &projects, true);
        assert_eq!(ui.stack.len(), deep, "the leaf crumb does nothing");
    }

    #[test]
    fn going_back_restores_the_category_list() {
        let projects = fixture();
        let mut ui = Ui::new(PathBuf::from("root"));
        ui.selected = Cat::Source;
        ui.open(&projects, true);
        assert_eq!(ui.stack.len(), 1);
        ui.back(&projects, true);
        assert!(ui.stack.is_empty());
        assert!(ui.children.is_empty());
        assert_eq!(ui.entries(&projects).len(), projects.len());
    }

    #[test]
    fn junk_only_keeps_the_rows_worth_looking_at() {
        let projects = fixture();
        let mut ui = Ui::new(PathBuf::from("root"));
        ui.selected = Cat::Source;
        let all = ui.entries(&projects).len();
        ui.toggle_junk_only();
        let only = ui.entries(&projects);
        assert!(only.len() < all, "the filter drops rows that are fine");
        assert!(only.iter().any(|r| r.name == "old"), "the stale one stays");
    }

    #[test]
    fn every_category_row_accounts_for_its_share_of_the_disk() {
        let projects = fixture();
        let ui = Ui::new(PathBuf::from("root"));
        let sizes = ui.category_sizes(&projects);
        let total: u64 = sizes.iter().sum();
        assert_eq!(total, ui.total(&projects));
        assert_eq!(sizes.len(), CATEGORIES.len());
    }
}