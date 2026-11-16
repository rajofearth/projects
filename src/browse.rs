//! What is inside a folder, and where its space actually is.
//!
//! The on-demand half of the scanner: opening a project lists its children measured
//! directory by directory, and a culprit search follows the biggest child down while it holds
//! most of its parent. Both run on a thread and stream back so the window never blocks; a
//! newer request cancels the older one.
//!
//! Ported from the upstream interface's `browse.ts` (MIT), with `du` replaced by a directory
//! walk that the scanner already knows how to do.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::cats::is_whole;

/// A child of a folder, as the detail pane lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub dir: bool,
}

/// Where a folder's space actually is: folders (and maybe one file) each holding most of the
/// one before, plus the biggest things directly inside for when no single one dominates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Culprit {
    pub chain: Vec<Item>,
    pub top: Vec<Item>,
    /// Size of the folder the search started in, the denominator the chain's share uses.
    pub root_size: u64,
}

pub enum BrowseEvent {
    /// The list so far for `path`, largest first; `done` once every folder is measured.
    Listing {
        path: PathBuf,
        items: Vec<Item>,
        done: bool,
    },
    Culprit {
        path: PathBuf,
        culprit: Culprit,
    },
}

pub type Cancel = Arc<AtomicBool>;

pub fn cancel_flag() -> Cancel {
    Arc::new(AtomicBool::new(false))
}

/// Bytes on disk under `path`: every file's length, junctions and symlinks left alone.
pub fn folder_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

fn child(path: &Path, name: &str, size: u64, dir: bool) -> Item {
    Item {
        path: path.join(name),
        name: name.to_string(),
        size,
        dir,
    }
}

/// Every child of `path`, largest first, without measuring subfolders (their sizes are 0).
pub fn list_immediate(path: &Path) -> (Vec<Item>, Vec<Item>) {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let Ok(entries) = fs::read_dir(path) else {
        return (files, dirs);
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if kind.is_dir() {
            dirs.push(child(path, &name, 0, true));
        } else if let Ok(meta) = entry.metadata() {
            files.push(child(path, &name, meta.len(), false));
        }
    }
    files.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    dirs.sort_by(|a, b| a.name.cmp(&b.name));
    (files, dirs)
}

/// The whole list, measured, largest first. Blocking; used by tests and by the worker.
pub fn list_folder_blocking(path: &Path, mut on_progress: impl FnMut(&[Item])) -> Vec<Item> {
    let (mut files, dirs) = list_immediate(path);
    let mut items = std::mem::take(&mut files);
    items.sort_by(|a, b| b.size.cmp(&a.size));
    on_progress(&items);
    for mut dir in dirs {
        dir.size = folder_size(&dir.path);
        items.push(dir);
        items.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
        on_progress(&items);
    }
    items
}

/// Lists `path` on a thread, streaming the list as folders come back measured.
pub fn list_folder(path: PathBuf, tx: Sender<BrowseEvent>, cancel: Cancel) {
    std::thread::spawn(move || {
        let (files, dirs) = list_immediate(&path);
        let mut items = files;
        let mut send = |items: &[Item], done: bool| {
            let _ = tx.send(BrowseEvent::Listing {
                path: path.clone(),
                items: items.to_vec(),
                done,
            });
        };
        send(&items, dirs.is_empty());
        for mut dir in dirs {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            dir.size = folder_size(&dir.path);
            items.push(dir);
            items.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
            send(&items, false);
        }
        send(&items, true);
    });
}

/// A child holding at least this share of its parent counts as "where the space is".
const DOMINANT: f64 = 0.5;
const DEPTH: usize = 12;

pub fn find_culprit_blocking(path: &Path) -> Culprit {
    let mut chain: Vec<Item> = Vec::new();
    let mut current = path.to_path_buf();
    let mut current_size = folder_size(path);
    let root_size = current_size;
    let mut top: Vec<Item> = Vec::new();

    for depth in 0..DEPTH {
        let (files, dirs) = list_immediate(&current);
        let mut kids: Vec<Item> = files
            .into_iter()
            .chain(dirs.into_iter().map(|mut d| {
                d.size = folder_size(&d.path);
                d
            }))
            .collect();
        kids.sort_by(|a, b| b.size.cmp(&a.size));
        if depth == 0 {
            top = kids.iter().take(3).cloned().collect();
        }
        let Some(best) = kids.into_iter().next() else {
            break;
        };
        if (best.size as f64) < current_size as f64 * DOMINANT {
            break;
        }
        let whole = is_whole(&best.name);
        current = best.path.clone();
        current_size = best.size;
        chain.push(best);
        if whole {
            break;
        }
    }
    Culprit {
        chain,
        top,
        root_size,
    }
}

/// Searches for the culprit on a thread.
pub fn find_culprit(path: PathBuf, tx: Sender<BrowseEvent>, cancel: Cancel) {
    std::thread::spawn(move || {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let culprit = find_culprit_blocking(&path);
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let _ = tx.send(BrowseEvent::Culprit { path, culprit });
    });
}

/// Shows the item in Explorer (Finder, or the desktop's file manager elsewhere).
pub fn reveal(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &path.to_string_lossy()])
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path.parent().unwrap_or(path))
            .spawn()
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ps-browse-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn listing_measures_folders_and_sorts_largest_first() {
        let root = scratch("listing");
        fs::create_dir_all(root.join("small")).unwrap();
        fs::write(root.join("small/a.txt"), "x".repeat(100)).unwrap();
        fs::create_dir_all(root.join("big")).unwrap();
        fs::write(root.join("big/a.bin"), "x".repeat(9000)).unwrap();
        fs::write(root.join("loose.txt"), "x".repeat(500)).unwrap();

        let mut progress = 0;
        let items = list_folder_blocking(&root, |_| progress += 1);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].name, "big", "the folder with 9 kB leads");
        assert_eq!(items[0].size, 9000);
        assert!(items[0].dir);
        assert_eq!(items[1].name, "loose.txt");
        assert!(!items[1].dir);
        assert_eq!(items[2].size, 100);
        assert!(progress >= 2, "the list streamed as folders landed");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_culprit_follows_the_biggest_child_down() {
        let root = scratch("culprit");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "x".repeat(100)).unwrap();
        fs::create_dir_all(root.join("node_modules/dep")).unwrap();
        fs::write(root.join("node_modules/dep/index.js"), "x".repeat(20_000)).unwrap();

        let culprit = find_culprit_blocking(&root);
        let names: Vec<&str> = culprit.chain.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["node_modules"], "the search stops at node_modules");
        assert_eq!(culprit.chain[0].size, 20_000);
        assert_eq!(culprit.top.len(), 2, "both children are listed as the spread");
        assert_eq!(culprit.top[0].name, "node_modules");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_spread_out_folder_has_no_chain() {
        let root = scratch("spread");
        for name in ["a", "b", "c"] {
            fs::create_dir_all(root.join(name)).unwrap();
            fs::write(root.join(name).join("f.bin"), "x".repeat(1000)).unwrap();
        }
        let culprit = find_culprit_blocking(&root);
        assert!(culprit.chain.is_empty(), "no one child holds half of it");
        assert_eq!(culprit.top.len(), 3);
        fs::remove_dir_all(&root).ok();
    }
}