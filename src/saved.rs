//! What the app remembers between runs: pins, archived and recently opened projects, the home
//! page's layout and order, the project folders and the default editor. One `rune::prefs`
//! file, written on every change.

use std::collections::{BTreeMap, BTreeSet};

use rune::prefs::Prefs;

const PREFS_ID: &str = "projects";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Grid,
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Recent,
    Name,
    Size,
}

impl Order {
    pub const ALL: [Order; 3] = [Order::Recent, Order::Name, Order::Size];
    pub fn label(self) -> &'static str {
        match self {
            Order::Recent => "Recent",
            Order::Name => "Name",
            Order::Size => "Size",
        }
    }
    fn id(self) -> &'static str {
        match self {
            Order::Recent => "recent",
            Order::Name => "name",
            Order::Size => "size",
        }
    }
    fn from_id(id: &str) -> Order {
        match id {
            "name" => Order::Name,
            "size" => Order::Size,
            _ => Order::Recent,
        }
    }
}

pub struct Saved {
    prefs: Prefs,
    pub pinned: BTreeSet<String>,
    pub archived: BTreeSet<String>,
    /// Project name → unix seconds it was last opened from here.
    pub opened: BTreeMap<String, i64>,
    pub layout: Layout,
    pub order: Order,
    /// Every projects folder the user has added; `root` is the one shown.
    pub roots: Vec<String>,
    pub root: String,
    /// The app id Enter and double-click open a project in; `None` is the first one found.
    pub editor: Option<String>,
    pub auto_rescan: bool,
    /// Check GitHub and download new versions in the background.
    pub auto_update: bool,
    /// The last error writing the file, for a toast.
    pub error: Option<String>,
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Saved {
    pub fn load(default_root: &str) -> Saved {
        let prefs = if cfg!(test) { Prefs::memory() } else { Prefs::open(PREFS_ID) };
        Saved::from_prefs(prefs, default_root)
    }

    pub fn from_prefs(prefs: Prefs, default_root: &str) -> Saved {
        let set = |key: &str| -> BTreeSet<String> {
            prefs.get::<Vec<String>>(key).unwrap_or_default().into_iter().collect()
        };
        let mut roots: Vec<String> = prefs.get("roots").unwrap_or_default();
        // The environment's root wins for this run (tests and headless scenes set it), and
        // joins the list so it can be switched back to.
        let root = std::env::var("RUNE_STATS_ROOT")
            .ok()
            .or_else(|| prefs.get::<String>("root"))
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| default_root.to_string());
        if !roots.iter().any(|r| r.eq_ignore_ascii_case(&root)) {
            roots.insert(0, root.clone());
        }
        Saved {
            pinned: set("pinned"),
            archived: set("archived"),
            opened: prefs.get("opened").unwrap_or_default(),
            layout: match prefs.get::<String>("layout").as_deref() {
                Some("list") => Layout::List,
                _ => Layout::Grid,
            },
            order: Order::from_id(&prefs.get::<String>("order").unwrap_or_default()),
            roots,
            root,
            editor: prefs.get("editor"),
            auto_rescan: prefs.get("auto_rescan").unwrap_or(true),
            auto_update: prefs.get("auto_update").unwrap_or(true),
            error: None,
            prefs,
        }
    }

    pub fn path(&self) -> Option<std::path::PathBuf> {
        self.prefs.path().map(|p| p.to_path_buf())
    }

    fn put<T: serde::Serialize + ?Sized>(&mut self, key: &str, value: &T) {
        if let Err(e) = self.prefs.set(key, value) {
            self.error = Some(format!("couldn't save settings: {e}"));
        }
    }

    pub fn toggle_pin(&mut self, name: &str) -> bool {
        let on = !self.pinned.remove(name);
        if on {
            self.pinned.insert(name.to_string());
        }
        let list: Vec<String> = self.pinned.iter().cloned().collect();
        self.put("pinned", &list);
        on
    }

    pub fn toggle_archived(&mut self, name: &str) -> bool {
        let on = !self.archived.remove(name);
        if on {
            self.archived.insert(name.to_string());
        }
        let list: Vec<String> = self.archived.iter().cloned().collect();
        self.put("archived", &list);
        on
    }

    pub fn mark_opened(&mut self, name: &str) {
        self.opened.insert(name.to_string(), now_unix());
        // Only the most recent few matter; the file stays small.
        if self.opened.len() > 40 {
            let mut by_age: Vec<(String, i64)> =
                self.opened.iter().map(|(k, v)| (k.clone(), *v)).collect();
            by_age.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
            self.opened = by_age.into_iter().take(40).collect();
        }
        let opened = self.opened.clone();
        self.put("opened", &opened);
    }

    pub fn clear_opened(&mut self) {
        self.opened.clear();
        self.put("opened", &BTreeMap::<String, i64>::new());
    }

    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
        self.put("layout", if layout == Layout::List { "list" } else { "grid" });
    }

    pub fn set_order(&mut self, order: Order) {
        self.order = order;
        self.put("order", order.id());
    }

    pub fn set_editor(&mut self, id: Option<String>) {
        self.editor = id;
        let v = self.editor.clone();
        self.put("editor", &v);
    }

    pub fn set_auto_rescan(&mut self, on: bool) {
        self.auto_rescan = on;
        self.put("auto_rescan", &on);
    }

    pub fn set_auto_update(&mut self, on: bool) {
        self.auto_update = on;
        self.put("auto_update", &on);
    }

    pub fn add_root(&mut self, root: &str) -> bool {
        if self.roots.iter().any(|r| r.eq_ignore_ascii_case(root)) {
            return false;
        }
        self.roots.push(root.to_string());
        let roots = self.roots.clone();
        self.put("roots", &roots);
        true
    }

    pub fn remove_root(&mut self, root: &str) {
        if self.roots.len() <= 1 || root == self.root {
            return;
        }
        self.roots.retain(|r| r != root);
        let roots = self.roots.clone();
        self.put("roots", &roots);
    }

    pub fn set_root(&mut self, root: &str) {
        self.root = root.to_string();
        let r = self.root.clone();
        self.put("root", &r);
        let roots = self.roots.clone();
        self.put("roots", &roots);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_archive_and_layout_survive_a_reload() {
        let dir = std::env::temp_dir().join(format!("ps-saved-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("prefs.json");
        let _ = std::fs::remove_file(&file);
        {
            let mut s = Saved::from_prefs(Prefs::at(&file), r"P:\Projects");
            s.toggle_pin("ply");
            s.toggle_archived("old");
            s.set_layout(Layout::List);
            s.set_order(Order::Size);
            s.set_editor(Some("zed".into()));
            s.mark_opened("ply");
        }
        let s = Saved::from_prefs(Prefs::at(&file), r"P:\Projects");
        assert!(s.pinned.contains("ply"));
        assert!(s.archived.contains("old"));
        assert_eq!(s.layout, Layout::List);
        assert_eq!(s.order, Order::Size);
        assert_eq!(s.editor.as_deref(), Some("zed"));
        assert!(s.opened.contains_key("ply"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
